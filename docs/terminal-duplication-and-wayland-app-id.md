# Terminal Word Duplication & Wayland App ID Classification Analysis

> **Date:** 2026-11-05  
> **Status:** Investigation Complete & Solution Designed  
> **Affected Component:** `fcitx5-smartkey` (`adapters/fcitx5-smartkey`), Wayland environment (`niri`), Ghostty terminal  
> **Target Fix:** Enhanced Wayland `app_id` normalization + terminal bypass matching in `smartkey-addon.cpp`

---

## 1. Problem Statement & Observed Symptoms

During interactive typing on Wayland compositors (specifically Niri) with `fcitx5-smartkey` active on English keyboard layouts (`keyboard-us`):

1. **Word Duplication on Autocorrect:**
   When SmartKey triggered an autocorrect or shift-slip fix (e.g. `dont` → `don't`, `SEe` → `See`, `whats` → `what's`, `thats` → `that's`, `english` → `englishEnglish`), instead of deleting the typo and inserting the replacement, the replacement word was appended directly after the typo without removing it (yielding `dontdon't`, `SEeSee`, `whatswhat's`, `thatsthat's`, `englishEnglish`).

2. **Subsequent Autocorrect Failure on Previously Corrected Words:**
   Because the user pressed `BackSpace` repeatedly to clear the duplicated tokens, SmartKey's learning state machine registered these as `manual_backspace_fix` and `backspace_undo` events. Once accumulated, words were placed into `rules.toml` rejection `blocklist` entries (`{ typed = "didnt", rejected = "didn't" }`) or tagged in `learner_state.json` `untouched_unknowns` / `trouble_words`, effectively suppressing future autocorrection for those words.

3. **Inconsistent App Behavior:**
   - Browsers (e.g., Zen Browser) initially exhibited this duplication until session environment variables (`GTK_IM_MODULE`, `QT_IM_MODULE`) were unset in favor of native Wayland text-input protocols (`wayland_v2` / `text-input-v3`), after which Zen operated flawlessly.
   - The terminal emulator (**Ghostty**), however, continued to duplicate words even after global environment variables were purged.

---

## 2. Root Cause Analysis (Multi-Layer)

A deep-dive investigation using binary disassembly (`objdump`), process environment introspection (`/proc/$PID/environ`), DBus controller inspection (`Fcitx.Controller1 DebugInfo`), and adapter source audits revealed three intersecting root causes:

### Layer 1: GTK4 `libim-fcitx5.so` `ForwardKey` Stub (`frontend:dbus`)

When `GTK_IM_MODULE=fcitx` is exported in the user's environment, GTK4 applications load `/usr/lib/gtk-4.0/4.0.0/immodules/libim-fcitx5.so` and connect to Fcitx5 via DBus (`frontend:dbus`).

Disassembly of `/usr/lib/gtk-4.0/4.0.0/immodules/libim-fcitx5.so` revealed:
```assembly
0000000000006b50 <_fcitx_im_context_forward_key_cb>:
    6b50: f3 0f 1e fa                   endbr64
    6b54: c3                            ret
```
- In GTK4, upstream developers completely removed synthetic event delivery (`gdk_event_put`). Consequently, Fcitx5's GTK4 IM module implements the DBus `ForwardKey` callback as a hard no-op (`ret`).
- When SmartKey calls `ic->forwardKey(FcitxKey_BackSpace)` or attempts surrounding text deletion, the backspaces are silently dropped by the GTK4 client.
- However, `ic->commitString(...)` succeeds via DBus, resulting in the replacement string being inserted immediately following the un-deleted typo (`orig + replacement`).
- In addition, the DBus GTK4 frontend reports `ic->program()` as empty (`""`), which in `smartkey-addon.cpp` falls back to `SMARTKEY_PROFILE_PROSE`.

### Layer 2: Ghostty Single-Instance Daemon Architecture

While global environment files (`/home/precision/.config/environment.d/fcitx.conf` and `~/.config/niri/config.kdl`) were updated to unset `GTK_IM_MODULE`:
- Ghostty runs as a background user systemd daemon:
  `systemctl --user status app-com.mitchellh.ghostty.service`
  `ExecStart=ghostty --gtk-single-instance=true --initial-window=false`
- In `~/.config/ghostty/config`, the options `gtk-single-instance = true` and `quit-after-last-window-closed = false` are set.
- As a result, closing all terminal windows **never terminated the daemon process** (PID `805`). PID `805` remained alive since initial login, retaining `GTK_IM_MODULE=fcitx` in its `/proc/805/environ` and keeping `libim-fcitx5.so` loaded.
- Closing and reopening terminal windows simply created new GTK4 surfaces attached to the existing legacy daemon.

### Layer 3: Wayland Reverse-DNS `app_id` Classifier Mismatch in `libsmartkey.so`

Per SmartKey's specification ([`architecture-and-plan.md`](./architecture-and-plan.md) and [`README.md`](../README.md#L147)), terminals must be **100% bypassed** (`AppProfile::Bypass`) because:
1. Raw PTY streams have no `SurroundingText` buffer semantics (`ic->surroundingText().isValid() == false`).
2. Prose autocorrect must never alter shell commands, arguments, compiler flags, or REPL inputs.

When Ghostty runs under native Wayland (`frontend:wayland_v2`), Fcitx5 obtains the application identifier from the Wayland surface's desktop `app_id`:
```
IC [109f16a3dc07405a9942418a7a2d27ed] program:com.mitchellh.ghostty frontend:wayland_v2 cap:72
```

In `adapters/fcitx5-smartkey/src/smartkey-addon.cpp`:
```cpp
std::string normalizeProgramBasename(std::string_view raw) {
    if (const auto slash = raw.rfind('/'); slash != std::string_view::npos) {
        raw.remove_prefix(slash + 1);
    }
    std::string lower(raw);
    std::transform(lower.begin(), lower.end(), lower.begin(),
                   [](unsigned char c) { return static_cast<char>(std::tolower(c)); });
    return lower;
}
```
And in `classifyProfile(InputContext *ic)`:
```cpp
const std::string prog = normalizeProgramBasename(ic->program());
// ...
if (matchesAnyExact(prog, {
        "alacritty", "kitty", "foot", "footclient", "ghostty",
        "wezterm", "wezterm-gui", "konsole", "gnome-terminal",
        // ...
    }) ||
    matchesAnySubstring(prog, {"terminal", "jetbrains-"})) {
    return SMARTKEY_PROFILE_BYPASS;
}
```

**The Failure:**
1. `normalizeProgramBasename` strips path slashes (`/`), but does not strip reverse-DNS dots (`.`).
2. `prog` evaluates to `"com.mitchellh.ghostty"`.
3. `"com.mitchellh.ghostty"` does not match `"ghostty"` in `matchesAnyExact`.
4. `"com.mitchellh.ghostty"` does not contain the substring `"terminal"`.
5. Wayland text-input protocol does not set `CapabilityFlag::Terminal`.
6. `classifyProfile` falls through to `SMARTKEY_PROFILE_PROSE` (Active).
7. Ghostty cannot be configured with `class = "ghostty"` because GTK `GApplication` enforces a reverse-DNS naming scheme (with at least one dot), and altering it breaks DBus activation and desktop file matching.

---

## 3. Feedback Loop into Learner State

When SmartKey incorrectly activates inside a terminal and backspaces are dropped:
1. The user types `dont`, SmartKey emits `don't`, producing `dontdon't`.
2. The user presses `BackSpace` multiple times to erase the duplicated word.
3. SmartKey's `SmartKeySession` observes backspace events immediately following an autocorrection:
   - Increments `backspace_undos` and `trouble_words`.
   - Records `staged_undo_keep` entries.
   - At threshold count, adds `{ typed = "dont", rejected = "don't" }` to `blocklist` in `rules.toml`.
4. Once in the blocklist, SmartKey permanently stops correcting `dont` even in legitimate prose apps like Zen Browser.

---

## 4. Proposed Solution Architecture

### Step 1: Robust `app_id` Normalization in `smartkey-addon.cpp`

Update the program normalization logic in `adapters/fcitx5-smartkey/src/smartkey-addon.cpp` to handle reverse-DNS desktop identifiers and flatpak-style application IDs:

```cpp
std::string normalizeProgramBasename(std::string_view raw) {
    if (const auto slash = raw.rfind('/'); slash != std::string_view::npos) {
        raw.remove_prefix(slash + 1);
    }
    // If the ID is a reverse-DNS desktop app_id (e.g. "com.mitchellh.ghostty",
    // "org.wezfurlong.wezterm", "io.github.alacritty", "org.gnome.Terminal"),
    // strip everything up to the final dot to extract the base name.
    if (const auto dot = raw.rfind('.'); dot != std::string_view::npos) {
        raw.remove_prefix(dot + 1);
    }
    std::string lower(raw);
    std::transform(lower.begin(), lower.end(), lower.begin(),
                   [](unsigned char c) { return static_cast<char>(std::tolower(c)); });
    return lower;
}
```

In addition, add explicit reverse-DNS entries and common prefixes to `classifyProfile` as defense-in-depth:
- Explicit bypass for `"com.mitchellh.ghostty"`, `"org.wezfurlong.wezterm"`, `"io.github.alacritty"`, `"org.gnome.terminal"`, `"org.gnome.terminal.server"`.
- Preserve full app_id matching if a future app profile requires distinguishing between package namespaces.

### Step 2: Session Environment & Ghostty Daemon Restart

1. Global configuration already established:
   - `~/.config/environment.d/fcitx.conf`: Only `XMODIFIERS=@im=fcitx` (no `GTK_IM_MODULE` or `QT_IM_MODULE`).
   - `~/.config/niri/config.kdl`: Nullified `GTK_IM_MODULE`, `QT_IM_MODULE`, `SDL_IM_MODULE`, `GLFW_IM_MODULE`.
2. Cleanly restart Ghostty background daemon:
   ```bash
   systemctl --user restart app-com.mitchellh.ghostty.service
   ```
   *(Note: This restarts the single-instance daemon; will close any open Ghostty windows).*

### Step 3: Learner State Recovery

1. Clean accumulated `backspace_undo` events and false `blocklist` entries in:
   - `/home/precision/.config/smartkey/rules.toml`
   - `/home/precision/.config/smartkey/learner_state.json`
2. Run `smartkeyctl reload` or restart Fcitx5 to refresh active engine rules.

---

## 5. Verification Matrix

| Test Scenario | Environment | Expected Profile | Expected Behavior |
|---|---|---|---|
| Zen Browser / Firefox | Wayland (`wayland_v2`) | `SMARTKEY_PROFILE_BROWSER` | Autocorrect active; single word replaced atomically. |
| Ghostty Terminal | Wayland (`wayland_v2`, `app_id: com.mitchellh.ghostty`) | `SMARTKEY_PROFILE_BYPASS` | 100% hands-off; no words altered; no duplicate words. |
| Kitty / Alacritty / Foot | Wayland (`wayland_v2`) | `SMARTKEY_PROFILE_BYPASS` | 100% hands-off. |
| Neovim / VSCode / Zed | Wayland (`wayland_v2`) | `SMARTKEY_PROFILE_BYPASS` | 100% hands-off. |
| Discord / Telegram | Wayland (`wayland_v2`) | `SMARTKEY_PROFILE_CHAT` | Autocorrect active. |
| Vietnamese Switch (`Alt+Z`) | Any application | `SMARTKEY_PROFILE_*` | Steps aside completely for `fcitx5-lotus` / Telex. |

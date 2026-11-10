# Audit: System Integration — fcitx5, Distro Portability, Init/Session, FFI, File Coupling

**Date:** 2026-11-10
**Scope:** Whole-implementation audit of how SmartKey fits the systems it targets: the fcitx5 addon API and event model, distro packaging & portability, init/session integration, the Rust↔C++ FFI boundary, and cross-process file coupling between the fcitx5 addon and the CLI.
**Repo state at audit:** `v0.1.1-5-gbe2c346-dirty`
**Companion doc:** `docs/cli-audit.md` (CLI-surface findings — cross-referenced where issues overlap)
**Status:** **Findings only — no fixes applied, no design decisions made.**

Severity legend: **P0** = breaks or destroys (data loss / load failure on target platforms) · **P1** = wrong behavior in realistic scenarios · **P2** = robustness / portability gap · **P3** = polish.

---

## 0. Executive summary

The integration skeleton is solid and in several places genuinely better than typical for a beta: idiomatic fcitx5 addon registration with a real `AddonManager` smoke test, CMake driven by fcitx5's own install variables (multiarch/lib64 resolve by construction), a fully verified panic-safe FFI (23/23 exports wrapped, zero header/impl ABI mismatches), graceful degradation everywhere fcitx5 is unreachable, and an arch-neutral Rust core.

The risks concentrate in four places:

1. **The "generic Linux" release artifact does not run on its own advertised targets** — it was built against glibc 2.39 and the build-host's fcitx5 5.1.23 sonames (P0/P1).
2. **Two P0 data-integrity flaws in the shared-file layer** — a cross-process shared `.tmp` rename race that can silently wipe the entire learner brain, and a non-atomic, never-fsynced `rules.toml` write that can destroy user config on crash (P0).
3. **Two P1 correctness bugs in the key-replacement path** — a char-vs-byte unit mismatch against fcitx5's byte-based `deleteSurroundingText` (corrupts multibyte words, prime victim: the Vietnamese lotus rescue), and no reconciliation between the session's shadow buffer and the client's actual text after non-key cursor movements (P1).
4. **Silent-failure UX**: broken TOML silently re-enables a disabled SmartKey with preset defaults, silent write suppression while `parse_locked`, swallowed IO errors everywhere, and no IM-environment validation anywhere in the onboarding path (P1/P2).

There is no CI, which is what allowed the P0 glibc artifact issue through (P2).

---

## 1. fcitx5 addon integration

### Done correctly

- **Registration/teardown**: `FCITX_ADDON_FACTORY_V2_BACKWARDS` exports both legacy and V2 factory symbols — the correct macro for a standalone `Category=Module` addon across 5.0.x/5.1.x loaders (smartkey-addon.h:211–216, smartkey-addon.cpp:521). Teardown order is right: watchers cleared → factory unregistered → engine freed (smartkey-addon.cpp:111–120). Per-IC `FactoryFor<SmartKeyState>` + `registerProperty` is the idiomatic state pattern.
- **Stepping aside**: `instance_->inputMethod(ic)` checked per event; `keyboard-*` processes, `lotus` bridges, everything else untouched (smartkey-addon.cpp:156–172, 333–336). No private fcitx5 internals used anywhere; all API surface is public.
- **Event semantics**: releases and modifier-only keys ignored; Ctrl/Alt/Super combos feed `NAV_OR_CLICK` but are never consumed; `filterAndAccept()` only on an actual Replace; the replacement insert includes the trailing boundary chars so no double-commit (smartkey-addon.cpp:248–265, 301, 515; session.rs:1566–1576). Compose/dead-key sequences skipped via `isComposing` (:427).
- **Hot reload**: `reloadConfig()` reloads dicts + resets all ICs; per-IC `reset()` triggers `store.reload_if_modified()` (session.rs:556–570), making the "rules.toml hot-reloads on focus change" claim true. Smoke test loads the real `AddonManager` with a sandboxed `XDG_DATA_HOME` (tests/addon_load_smoke.cpp:125–142).
- **Addon conf discovery** proven live: `~/.local/share/fcitx5/addon/smartkey.conf` with an absolute `Library=` path; CMake uses `FCITX_INSTALL_ADDONDIR`/`FCITX_INSTALL_PKGDATADIR` (see §2).

### Findings

| Sev | Finding | Evidence |
|---|---|---|
| **P1** | **Char-vs-byte unit mismatch in `applyReplaceAction`.** `act.delete_chars` counts Unicode scalar values (session.rs:386–388), but fcitx5's `SurroundingText::cursor()` and `deleteSurroundingText(offset, size)` are **byte**-based (fcitx5 `text.h:34–35`, `inputcontext.h:195`). smartkey-addon.cpp:304–308 compares bytes to chars and passes a char count as a byte size → any replaced word containing multibyte characters **under-deletes**, leaving `orig + replacement` in the buffer. Prime victim: the Vietnamese lotus rescue path (`delete_chars = vi_sim.screen_chars`, session.rs:1571–1572) over diacritic words like `trờiiii`. Ironically the `forwardKey(BackSpace)` fallback (:310–314) has the correct units. | smartkey-addon.cpp:304–308 |
| **P1** | **Shadow-buffer divergence on non-key cursor changes.** The session buffer is only fed key events; there is no surrounding-text/cursor watcher (watchers = KeyEvent×2, FocusOut, Reset, SwitchInputMethod, smartkey-addon.cpp:83–108) and no FFI to reconcile buffer vs client text. Mouse-click repositioning mid-word (no key event, no focus change — typical in browsers) leaves stale buffer content; the next boundary key then deletes at the *client's* cursor — mixed deletion, i.e. text corruption. Partially mitigated by toolkit `InputContextReset` and keyboard nav (`NAV_OR_CLICK`), but the design gap is app-dependent. | smartkey-addon.cpp:83–108; no reconciliation export in crates/smartkey-ffi/src/lib.rs |
| **P1 (conditional)** | **Empty `program()` on Wayland degrades everything to PROSE.** When the compositor provides no toplevel metadata, `ic->program()` is empty and the fallback is `setReason("prose")` (smartkey-addon.cpp:216–217) — the GTK4-dbus guard does not cover the wayland frontend, so autocorrect activates inside terminals. fcitx5 5.1.23's waylandim supports `zwlr_foreign_toplevel` / `ext_foreign_toplevel_list` / `zcosmic_toplevel_info` (KWin, wlroots compositors, GNOME 48+ covered), but **GNOME ≤ 47 or older fcitx5 builds on GNOME leave `program()` empty**, and only capability flags protect the user. No defensive "unknown app on wayland → bypass" option exists. | smartkey-addon.cpp:216–217 |
| P2 | **Declared version floor is wrong.** `find_package(Fcitx5Core 5.0.0)` and `Dependencies 0=core:5.0.0`, but the code uses `CapabilityFlag::Disable` — `@since 5.0.20`. Building against 5.0.0–5.0.19 headers fails to compile. Floor should be `core:5.0.20` (and see §2 for the conf group-name bug that makes the declared floor inert anyway). | CMakeLists.txt:17–18; smartkey.conf.in:12; capabilityflags.h:70–79 |
| P2 | **`InputContextDestroy` not handled** — IC destroyed mid-word (window closed while typing) skips the pending-unknown flush; the property destructor just frees the session. Learning silently dropped. Related dead surface: `SMARTKEY_KEY_FOCUS_LOST` FFI (lib.rs:29 → `KeyInput::FocusLost` → session.rs:675–678) is never sent by the addon. | smartkey-addon.h:140–145; smartkey-addon.cpp:100–108 |
| P2 | **Hardcoded app-classification gaps hit IDEs.** Flatpak/JetBrains ids whose distinguishing segment isn't in the list fall to PROSE: `com.jetbrains.Datagrip`, `org.jetbrains.PhpStorm` (the substring is `"jetbrains-"` with a hyphen — doesn't match), `com.google.AndroidStudio`; same for `com.microsoft.edge`, Arc, Opera in the browser list. For IDEs this is the product's worst-case failure (autocorrect inside code). A `rules.toml` app-profile override list would close it. | smartkey-addon.h:92–123 |
| P3 | Auto-repeat (`KeyState::Repeat`) not distinguished — held keys feed the undo-window state machine. KP_Space/KP_Separator boundary keys unhandled (hit the `cp < 0x20` ignore). C++/Rust boundary-char lists drifted (`“` opener mismatch). Boundary consumption bypasses punctuation-transform modules. | smartkey-addon.cpp:280–294; session.rs:592–601; smartkey-addon.h:130–135 |
| P3 | `Configurable=False` — no fcitx5 Configuration object (deliberate; `fcitx5-configtool` users get nothing). `save()` calls `reloadConfig()` (non-idiomatic, harmless). | smartkey.conf.in:9; smartkey-addon.cpp:122–141 |

## 2. Distro portability & packaging

### Done correctly

- **CMake is genuinely portable**: `find_package(Fcitx5Core/Fcitx5Utils)` + `FCITX_INSTALL_ADDONDIR`/`FCITX_INSTALL_PKGDATADIR` — no hardcoded lib paths; Debian multiarch (`/usr/lib/<triplet>/fcitx5`) and Fedora `lib64` resolve by construction (verified against installed Fcitx5UtilsConfig.cmake). The `~/.local` install trick (absolute `Library=` in the generated conf) is correct use of fcitx5 StandardPaths, and proven live on this machine.
- **PKGBUILD quality**: `arch=('x86_64' 'aarch64')`, SPDX license + LICENSE install, committed `Cargo.lock` with `cargo fetch --locked` / `build --frozen`, `check()` runs ctest, correct `options`, LICENSE/README installed.
- **Version consistency**: `0.1.1` identical across PKGBUILD / Cargo.toml / CMake / package-portable.sh / generated conf / FFI header; README artifact names match `dist/` output byte-for-byte.
- **Scripts**: bash-declared, `set -euo pipefail`, symmetric uninstall, legacy `~/.cargo/bin/smartkey` cleanup, fcitx5 restart only when tools exist.

### Findings

| Sev | Finding | Evidence |
|---|---|---|
| **P0** | **The "generic Linux" bundle requires glibc ≥ 2.39** — both artifacts carry versioned refs to `GLIBC_2.39` (`pidfd_spawnp`, `pidfd_getpid` from a modern Rust std, plus libc refs in the .so). Weak versioned refs still cause a fatal `version 'GLIBC_2.39' not found` on older glibc. **Debian 12 (2.36), Ubuntu 22.04 (2.35), Fedora ≤ 38 → the CLI binary and the addon both fail to load**, on the exact distros the README names as supported (README.md:70–73). Fix: build in an oldest-supported-glibc container, or ship a musl-static CLI. | objdump of `target/release/smartkey` + `build/libsmartkey.so`; built by scripts/package-portable.sh:17 |
| **P1** | **The bundle's prebuilt `libsmartkey.so` is built against build-host fcitx5 sonames** (`libFcitx5Core.so.7`, `libFcitx5Config.so.6`, `libFcitx5Utils.so.2` — fcitx5 5.1.23). Distros on fcitx5 5.0.x sonames cannot dlopen it. A glibc fix alone doesn't make the bundle portable: either don't prebuild the addon in the bundle, or state the fcitx5-ABI floor explicitly. | NEEDED list of adapters/fcitx5-smartkey/build/libsmartkey.so |
| P1/P2 | **Addon dependency floor is silently inert — wrong group name.** `smartkey.conf.in` and the embedded portable conf use `[Dependencies]` / `0=core:5.0.0`; upstream fcitx5 parses **`[Addon/Dependencies]`** (verified in fcitx5 `addoninfo.cpp`; on-disk upstream confs like clipboard.conf use the `Addon/`-prefixed group). fcitx5 therefore loads the addon without enforcing any version floor. One-line fix in two files. | smartkey.conf.in:11–12; scripts/package-portable.sh:88–89 |
| P2 | **PKGBUILD dependency gaps**: `curl` missing (CLI shells out for `dict install/update`, main.rs:914/966 — fails on minimal installs); `systemd` (owns `busctl`) absent but acceptable given the fallback; no fcitx5 version floor. Also `sha256sums=('SKIP')` + relative source: fine for the local build flow, non-publishable to AUR as-is. | packaging/arch/PKGBUILD:9, 12–13 |
| P2 | **No checksums/signatures** for the portable bundle (no `.sha256`, no sig; PKGBUILD uses SKIP). `releases/latest/download/...` URLs work but nothing verifies integrity. | scripts/package-portable.sh (ends at compression, :133–138) |
| P2 | **No CI at all** (`.github/` absent). Nothing validates the aarch64 claim in the PKGBUILD, older-glibc builds, or the README instructions — this is what let the P0 through. | repo root |
| P2 | **Ubuntu 20.04 cannot build the addon** (CMake 3.16 < required 3.20; GCC 9 < C++20 needs). Debian 12 / Ubuntu 22.04 are fine. Matters for anyone rebuilding the "generic" bundle on older distros. | CMakeLists.txt:1–6 |
| P3 | README install requires `tar --zstd` (busybox tar lacks it; no fallback documented). No aarch64 artifacts despite `arch=('aarch64')`. Binary not stripped in the bundle (only the .so). `fetch_sources.sh` pins `crate-ci/typos@master` (unreproducible) and uses GNU-only `gunzip -kf`. Version hand-duplicated in ~6 places; README version hardcoded per release. | README.md:70–73; scripts/fetch_sources.sh:15–19 |

## 3. Init & session integration

### Done correctly

- **No autostart/unit shipped, and none needed** — fcitx5 itself ships `/etc/xdg/autostart/org.fcitx.Fcitx5.desktop`; the project correctly delegates autostart and never starts fcitx5 uninvited (restart guidance is explicit user-facing `fcitx5 -r -d`).
- **No init-system hard dependency**: nothing links or calls systemd; every `busctl` call is guarded and falls back to `fcitx5-remote -r`, and shell scripts check `command -v` before both (install-local.sh:45; package-portable.sh:98,124). On Void/Artix/Alpine behavior degrades to the fallback silently and correctly.
- **DBus coordinates correct** (`org.fcitx.Fcitx5 /controller org.fcitx.Fcitx.Controller1.ReloadConfig`); `fcitx5-remote --check` used for daemon detection; with fcitx5 down, `status` degrades honestly.
- **DE-agnostic**: no compositor/tray/notification/dconf code anywhere; all frontend diversity is delegated to fcitx5.

### Findings

| Sev | Finding | Evidence |
|---|---|---|
| P2 | **IM environment variables are never set or checked anywhere.** `GTK_IM_MODULE`/`QT_IM_MODULE`/`XMODIFIERS` appear only in the engineering postmortem doc and one code comment — not in README install sections, install.sh, install-local.sh, or PKGBUILD. A user whose apps aren't routed through fcitx5 gets **silent no-op autocorrect** ("fcitx5 is running" but nothing connects). This is the largest real-world failure mode, and `smartkey status` cannot see it. | README.md:53–112; docs/terminal-duplication-and-wayland-app-id.md:130–141 |
| P2 | `busctl` (a systemd tool) is tried **first** before `fcitx5-remote -r`. Functionally fine today (fallback works), but `fcitx5-remote` or `gdbus` alone would drop the systemd-adjacent dependency entirely. | main.rs:58–85 |
| P3 | README never states the precondition "fcitx5 must already be installed and autostarting" before `smartkey status` means anything. | README.md:53–66 |
| P3 | Postmortem doc hardcodes a specific user's home path and a niri-specific fix recipe; the reusable guidance (unset `GTK_IM_MODULE`/`QT_IM_MODULE` on Wayland, keep `XMODIFIERS`) belongs in README/setup. | docs/terminal-duplication-and-wayland-app-id.md:47–52, 145–148 |

## 4. XDG & environment correctness

### Done correctly

- Identical XDG chains in Rust and C++ (`XDG_CONFIG_HOME → ~/.config → ./.smartkey`; dict: `SMARTKEY_DICT_DIR → XDG_DATA_HOME → ~/.local/share → /usr/local/share → /usr/share`; state: `XDG_STATE_HOME → ~/.local/state`) — consistent between addon and CLI, which is the property that matters.
- `EventLog` never resolves `$HOME` itself (dir injected by host); unwritable log dir silently disables logging; `learner_state.json` flush is (nominally) atomic tmp+rename; user's mid-edit TOML syntax error sets `parse_locked` instead of clobbering the file.
- Addon degrades inertly if `~/.config/smartkey` is unwritable (`smartkey_engine_new` → null → `FCITX_ERROR`, fcitx5 itself still loads).

### Findings

| Sev | Finding | Evidence |
|---|---|---|
| P2 | **Debian/Ubuntu multiarch detection gaps**: `detect_installed_addon` misses `/usr/lib/x86_64-linux-gnu/fcitx5/` (the Debian/Ubuntu `FCITX_INSTALL_ADDONDIR`), so `smartkey status` reports "not found" on correct installs; `cmd_plugin`'s lotus check misses `/usr/lib64`, multiarch, *and* `~/.local/lib/fcitx5/liblotus.so` — the exact location the project's own user-local flow installs to → false "fcitx5-lotus not detected". (Also in cli-audit.md §1 as a path bug.) | main.rs:96–114, 1045–1046 |
| P2 | **`HOME` unset → cwd-relative `.smartkey` fallback diverges**: the addon resolves relative to *fcitx5's* cwd, the CLI to the *shell's* cwd — two different `rules.toml`s, silently. XDG spec says fall back to passwd home, not cwd. Same divergence if the processes see different `XDG_*` values. | main.rs:20–32; lexicon.rs:581; smartkey-addon.cpp:40 |
| P3 | Read commands have write side effects: `status/rules/log/explain` create `rules.toml` if missing (load_or_init); on read-only HOME, `status` errors instead of reporting. `sudo smartkeyctl status` silently initializes `/root/.config/smartkey`. Flatpak-sandboxed fcitx5 (`~/.var/app/.../config`) diverges from the CLI's `~/.config` — undocumented. | store.rs:316–330; main.rs:20–32 |
| P3 | `curl` hardcoded for `dict install/update` with no timeout/proxy knobs (absence handled gracefully). | main.rs:960–968 |

## 5. FFI layer (crates/smartkey-ffi ↔ include/smartkey.h)

### Verified correct (exhaustively)

- **Panic safety: 23/23 exports wrapped** in `catch_unwind` with sentinel returns; no `panic=abort` in any profile; no export escapes the wrapper. (Since Rust 1.81 a panic out of `extern "C"` aborts anyway — defense in depth, not relied upon.)
- **ABI match: zero mismatches across all 23 functions** — types (`uint8_t/uint32_t/uint64_t/size_t/const char*/bool`), `SmartKeyAction` field order/layout, and all 17 constants agree; header version "v0.1.1" matches workspace.
- **Pointer hygiene**: every `*const c_char` null-checked then UTF-8-validated (`opt_cstr`, non-UTF-8 → None, never panics); returned `insert_utf8` lives in a session-owned scratch buffer, documented lifetime in smartkey.h:50–51, and the addon copies immediately (`commitString(std::string(...))`, smartkey-addon.cpp:319). No dangling path exists.
- **Symbol hygiene**: only the 23 `#[no_mangle]` exports in the workspace; `--exclude-libs,ALL` on the C++ link prevents Rust symbol collisions with other Rust addons (e.g. lotus).
- **Destructor ordering** in the addon is safe (watchers cleared → factory unregistered → engine freed; sessions die first).
- **Thread-safety by construction**: engine shared by all ICs, sessions per-IC, all calls serialized by fcitx5's single main event-loop dispatch; `SmartKeyEngineHandle` is neither Send nor Sync, which structurally blocks unsafe cross-thread use.
- **JNI path**: no jni crate, no `Java_*` exports — by design the host (HeliBoard) owns JNI glue and calls the plain C word-level hooks (`on_mobile_*`, real implementations, smoke-tested). Maturity: real Rust logic, out-of-repo glue.

### Findings

| Sev | Finding | Evidence |
|---|---|---|
| P2 | **`AssertUnwindSafe` over `&mut` state leaves handles corrupted-but-reusable**: a caught panic mid-mutation returns the sentinel, but C++ keeps using a handle whose session invariants may be broken, with no reset/poison path. A one-shot poisoned flag per handle would be safer. | lib.rs:133, 204, 301 (pattern) |
| P2 | **Non-UTF-8/empty path silently degrades to an in-memory engine** (`build_store` treats null/non-UTF-8/empty `rules_path` as in-memory): a mangled path yields a "working" engine that never persists anything, no error signal. Prefer returning null. | lib.rs:97–106, 85 |
| P3 | No double-free/use-after-free mitigation beyond null checks (standard for a C ABI; current destructor ordering makes the real callers safe). `opt_cstr` signature overclaims `&'static str`. The single-threaded engine contract exists nowhere in comments/docs — any future threaded caller is an untripwired data race. | lib.rs:81, 168–194 |

## 6. Cross-process file coupling (addon ↔ CLI, files only)

The fcitx5 addon process and the CLI share `rules.toml` + `learner_state.json` with **mtime-compare + full-reload when a change is detected, last-writer-wins when not**; state maps are never merged. The event log is safe by contrast (O_APPEND line-atomic writes on Linux, day-partitioned, racing prunes benign).

### Findings

| Sev | Finding | Evidence |
|---|---|---|
| **P0** | **Shared `.tmp` rename race can silently wipe the learner brain.** Both processes write `learner_state.json` via the *same* tmp path `path.with_extension("json.tmp")`. Interleaving A-rename with B-mid-write can persist a **truncated JSON** (B's fd writes into the renamed final file at its own offsets); on next load, `unwrap_or_default()` resets the **entire learner state** (staged counters, graduation progress, recent events) with no quarantine/backup. A dead process also leaves a stale `.tmp` forever. | store.rs:565–567, 343–350 |
| **P0** | **`flush_rules` is a bare in-place `fs::write` (truncate-then-write), and there is not a single fsync in the crates.** A crash/SIGKILL/power-loss between truncate and full write leaves `rules.toml` empty or partial — permanent destruction of user config (comments, personal dict, learned rules), or silent full fallback to the preset template. | store.rs:504; grep: no `sync_all` anywhere |
| P1 | **Lost-update windows.** (a) TOCTOU between the pre-write mtime stat and the write itself — "learn a correction while `smartkeyctl toggle` runs" is exactly the invited collision; `flush_rules`/`flush_state` also re-stat *after* their own write, so an intervening external write is recorded as "mine" and skipped forever after. (b) `record_untouched_unknown` flushes with **no** pre-write sync, and `session.reset` calls `reload_if_modified` **after** `flush_pending_unknown` — any CLI write since the addon's last sync is clobbered; `record_manual_edit_inner` mutates-then-syncs, so the sync can discard the just-accumulated increments. | store.rs:458–508; learner.rs:623–697; session.rs:570–584; learner.rs:312–620 |
| P1 | **`parse_locked` silently no-ops rule writes and the CLI lies.** While the user has a TOML syntax error mid-edit, `flush_rules` returns `Ok(())` and all callers ignore it: `smartkeyctl toggle` prints "saved to rules.toml" while nothing was written; learned-rule graduations are dropped and later erased when the reload replaces the in-memory doc. State writes are *not* blocked, so only rules are lost. | store.rs:496–507; 20+ `let _ = self.flush_*()` sites; main.rs:261–271 |
| P1 | **Broken `rules.toml` re-enables SmartKey with preset defaults.** The load fallback parses `preset.initial_toml()` whose defaults are `enabled = true`, `vi_lotus = true` — so a user who ran `smartkeyctl disable` and then breaks the TOML in an editor gets autocorrect **silently turned back on** until the file parses again. | store.rs:336–340, 53–68; session.rs:93 |
| P2 | **mtime precision assumptions**: exact-inequality compare fails on coarse-mtime filesystems (FAT/exFAT 2s, HFS+ 1s), NFS attribute caching, and writers that preserve mtime — **Syncthing is explicitly named as a supported external writer** in the code comments and can sync files back with original mtimes, making external edits invisible. | store.rs:446–447, 463, 485 |
| P2 | **All IO errors swallowed**: `let _ = self.flush_state()` at ~20 sites plus `EventLog` open/write errors — on ENOSPC or read-only HOME mid-session, learning silently stops persisting and the user finds out days later. | store.rs:589, learner.rs (18 flush sites); event_log.rs:207–209, 226–228 |
| P3 | Write frequency acceptable (per committed-word event, not per keystroke); rules-vs-state write-path inconsistency (direct write vs tmp+rename) should converge on one shape. | store.rs:496–508, 559–570 |

---

## 7. Consolidated priority list (cross-area)

**P0 — fix before next release**
1. Rebuild the portable bundle against an old-glibc baseline (oldest-supported container or musl-static CLI) — it currently fails on its own advertised targets (§2).
2. Decide the fcitx5-ABI policy for the prebuilt `.so` in the bundle (§2).
3. Unique tmp names (pid/nonce) + fsync-before-rename for both `flush_state` and `flush_rules`; quarantine unparseable `learner_state.json`/`rules.toml` instead of default-reset/preset-fallback (§6).

**P1 — correctness**
4. Byte-vs-char conversion in `applyReplaceAction` before calling `deleteSurroundingText` (§1).
5. Buffer reconciliation with surrounding text (watcher or FFI) for mouse-cursor moves (§1).
6. Empty-`program()` defense for compositors without toplevel metadata (§1).
7. Fix lost-update windows + move reload before flush in `session.reset` (§6); surface `parse_locked` suppression so the CLI can't claim success; make the broken-TOML fallback respect `enabled=false` (§6).

**P2 — portability & robustness**
8. `[Dependencies]` → `[Addon/Dependencies]` in smartkey.conf.in + embedded conf; raise floor to `core:5.0.20` (§1/§2).
9. Add `curl` to PKGBUILD depends (or bundle an HTTP client); multiarch/lib64/`~/.local` paths in `detect_installed_addon` and `cmd_plugin` (§2/§4).
10. IM-environment validation in `smartkey status` / install scripts (§3).
11. Add CI: build matrix incl. Debian 12 container (catches glibc floor), aarch64, ctest + cargo test (§2).
12. App-profile override list in `rules.toml` for flatpak/JetBrains IDE ids (§1); FFI handle poison flag + path-error signal (§5); flock or re-stat reconciliation for cross-process writes (§6).

**P3 — polish**
13. Dead `SMARTKEY_KEY_FOCUS_LOST` FFI; `InputContextDestroy` flush; auto-repeat; KP boundary keys; boundary-list drift; `Configurable` note; AUR-readiness of PKGBUILD; checksums for bundles; README preconditions + IM-env guidance; version single-sourcing; HOME-unset passwd fallback; read-only-HOME behavior of read commands.

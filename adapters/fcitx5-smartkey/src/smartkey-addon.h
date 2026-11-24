/*
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#ifndef FCITX5_SMARTKEY_ADDON_H
#define FCITX5_SMARTKEY_ADDON_H

#include <algorithm>
#include <atomic>
#include <cctype>
#include <cerrno>
#include <chrono>
#include <cstdint>
#include <cstdio>
#include <fcntl.h>
#include <filesystem>
#include <initializer_list>
#include <memory>
#include <string>
#include <string_view>
#include <sys/stat.h>
#include <unistd.h>
#include <utility>
#include <vector>

#include <fcitx-config/configuration.h>
#include <fcitx-config/iniparser.h>
#include <fcitx-config/option.h>
#include <fcitx-config/rawconfig.h>
#include <fcitx-utils/capabilityflags.h>
#include <fcitx-utils/event.h>
#include <fcitx-utils/eventloopinterface.h>
#include <fcitx-utils/handlertable.h>
#include <fcitx/addonfactory.h>
#include <fcitx/addoninstance.h>
#include <fcitx/addonmanager.h>
#include <fcitx/event.h>
#include <fcitx/inputcontext.h>
#include <fcitx/inputcontextproperty.h>
#include <fcitx/instance.h>

#include "smartkey.h"

namespace fcitx::smartkey {

inline std::string normalizeProgramId(std::string_view raw) {
    while (!raw.empty() &&
           (raw.front() == ' ' || raw.front() == '\t' ||
            raw.front() == '\r' || raw.front() == '\n')) {
        raw.remove_prefix(1);
    }
    while (!raw.empty() &&
           (raw.back() == ' ' || raw.back() == '\t' ||
            raw.back() == '\r' || raw.back() == '\n')) {
        raw.remove_suffix(1);
    }
    if (const auto slash = raw.find_last_of("/\\");
        slash != std::string_view::npos) {
        raw.remove_prefix(slash + 1);
    }
    std::string lower(raw);
    std::transform(lower.begin(), lower.end(), lower.begin(),
                   [](unsigned char c) { return static_cast<char>(std::tolower(c)); });
    if (lower.size() > 4 && lower.ends_with(".exe")) {
        lower.resize(lower.size() - 4);
    }
    return lower;
}

/// Extracts and normalizes the primary language subtag for `keyboard-*` IMs.
/// Returns `""` for non-keyboard IMs, empty language codes (e.g. `keyboard-brai`),
/// or `"vi"` (`keyboard-vn*`, which is not `fcitx5-lotus` Telex mode).
inline std::string normalizeKeyboardLangCode(std::string_view imName,
                                             std::string_view rawLangCode) {
    if (!imName.starts_with("keyboard-")) {
        return {};
    }
    const std::size_t sep = rawLangCode.find_first_of("_-");
    const std::string_view primary = rawLangCode.substr(0, sep);
    if (primary.empty()) {
        return {};
    }
    std::string lang;
    lang.reserve(primary.size());
    for (const char ch : primary) {
        if (ch >= 'A' && ch <= 'Z') {
            lang.push_back(static_cast<char>(ch - 'A' + 'a'));
        } else {
            lang.push_back(ch);
        }
    }
    if (lang == "vi") {
        return {};
    }
    return lang;
}

inline bool matchesExactOrAppSegment(
    std::string_view prog,
    std::initializer_list<std::string_view> names) {
    for (const auto name : names) {
        if (prog == name) {
            return true;
        }
    }
    if (prog.find('.') == std::string_view::npos) {
        return false;
    }
    // Check dot-separated reverse-DNS segments after the leading TLD segment
    // (e.g. "com.mitchellh.ghostty" -> "mitchellh", "ghostty";
    //  "org.telegram.desktop" -> "telegram", "desktop").
    std::size_t pos = prog.find('.');
    while (pos != std::string_view::npos) {
        const std::size_t start = pos + 1;
        const std::size_t next = prog.find('.', start);
        const std::string_view seg =
            (next == std::string_view::npos)
                ? prog.substr(start)
                : prog.substr(start, next - start);
        if (seg.size() >= 3) {
            for (const auto name : names) {
                if (seg == name) {
                    return true;
                }
            }
        }
        pos = next;
    }
    return false;
}

inline bool matchesAnySubstring(
    std::string_view prog,
    std::initializer_list<std::string_view> needles) {
    for (const auto needle : needles) {
        if (prog.find(needle) != std::string_view::npos) {
            return true;
        }
    }
    return false;
}

inline uint8_t classifyProgramName(std::string_view rawProgram) {
    const std::string prog = normalizeProgramId(rawProgram);
    if (prog.empty()) {
        return SMARTKEY_PROFILE_PROSE;
    }

    // Terminals and code-centric editors/IDEs: 100% hands-off
    if (matchesExactOrAppSegment(prog, {
            "alacritty", "kitty", "foot", "footclient", "ghostty",
            "wezterm", "wezterm-gui", "konsole", "gnome-terminal",
            "gnome-terminal-server", "kgx", "org.gnome.console", "ptyxis",
            "blackbox", "xterm", "urxvt", "st", "tilix", "terminator",
            "xfce4-terminal", "yakuake", "warp", "tabby", "rio",
            "code", "code-oss", "codium", "vscodium", "cursor", "windsurf",
            "zcode", "zed", "zed-editor", "nvim", "nvim-qt", "neovide", "vim",
            "gvim", "emacs", "helix", "sublime_text", "sublimetext", "subl",
            "idea", "clion", "pycharm", "rustrover", "goland", "webstorm",
            "datagrip", "phpstorm", "rider", "rubymine", "aqua", "fleet",
            "mps", "android-studio", "androidstudio", "rstudio", "qtcreator",
            "geany", "lapce", "lite-xl", "kakoune", "micro",
            "cmd", "conhost", "openconsole", "powershell", "pwsh",
            "wt", "windowsterminal", "mintty", "devenv", "notepad++",
            "unikey", "unikeynt", "evkey", "evkey64", "evkey32",
            "openkey", "openkey64", "openkey32"
        }) ||
        matchesAnySubstring(prog, {"terminal", "jetbrains-", "jetbrains.", "androidstudio"})) {
        return SMARTKEY_PROFILE_BYPASS;
    }

    if (matchesExactOrAppSegment(prog, {
            "firefox", "firefox-developer-edition", "librewolf", "waterfox",
            "zen", "zen-bin", "zen_browser", "chromium", "chrome",
            "google-chrome", "google-chrome-stable", "brave", "brave-browser",
            "vivaldi", "vivaldi-stable", "epiphany", "microsoft-edge",
            "msedge", "edge", "opera", "arc", "thorium", "floorp",
            "mullvad-browser", "ungoogled-chromium", "falkon", "qutebrowser"
        }) ||
        matchesAnySubstring(prog, {"firefox", "chromium", "chrome", "msedge"})) {
        return SMARTKEY_PROFILE_BROWSER;
    }

    if (matchesExactOrAppSegment(prog, {
            "discord", "vesktop", "legcord", "webcord", "telegram",
            "telegram-desktop", "org.telegram.desktop", "slack", "signal",
            "signal-desktop", "element", "element-desktop", "riot", "fractal",
            "caprine", "teams", "ms-teams", "zalo"
        }) ||
        matchesAnySubstring(prog, {"discord", "telegram", "slack", "signal"})) {
        return SMARTKEY_PROFILE_CHAT;
    }

    return SMARTKEY_PROFILE_PROSE;
}

inline bool isSmartKeyBoundaryChar(uint32_t cp) {
    return cp == ' ' || cp == '\t' || cp == ',' || cp == '.' || cp == '!' ||
           cp == '?' || cp == ';' || cp == ':' || cp == ')' || cp == ']' ||
           cp == '}' || cp == '"' || cp == '(' || cp == '[' || cp == '{' ||
           cp == '`' || cp == 0x201C || cp == 0x201D;
}

/// Returns true only for keys capable of triggering a mid-word `fcitx5-lotus` uinput burst
/// (Telex vowel/consonant marks, tones, undo `z`, `w`, and bracket-transform keys).
inline bool isTelexBurstTriggerChar(uint32_t cp) {
    if (cp == '[' || cp == ']' || cp == '{' || cp == '}') {
        return true;
    }
    if (cp >= 'A' && cp <= 'Z') {
        cp = cp - 'A' + 'a';
    }
    return cp == 'a' || cp == 'e' || cp == 'o' || cp == 'd' || cp == 'w' ||
           cp == 's' || cp == 'f' || cp == 'r' || cp == 'x' || cp == 'j' ||
           cp == 'z';
}

/// Returns true for key events synthesized by `fcitx5-lotus`'s Wayland virtual keyboard
/// (`LotusWlrVirtualKeyboard`): either non-boundary non-ASCII Unicode runes (precomposed
/// Vietnamese characters) or ASCII letters / space emitted on number-row keycodes `10..21`
/// (`kTextKeys[0..11] + 8` in `lotus-wlr-virtual-keyboard.cpp`).
inline bool isSyntheticVkKey(uint32_t keycode, uint32_t cp) {
    if (cp >= 0x80 && !isSmartKeyBoundaryChar(cp)) {
        return true;
    }
    if (keycode >= 10 && keycode <= 21) {
        return (cp >= 'a' && cp <= 'z') || (cp >= 'A' && cp <= 'Z') ||
               cp == ' ';
    }
    return false;
}

inline uint32_t countUtf8Chars(std::string_view s) {
    uint32_t count = 0;
    for (unsigned char b : s) {
        if ((b & 0xC0) != 0x80) {
            ++count;
        }
    }
    return count;
}

inline std::size_t stepUtf8Codepoint(std::string_view text, std::size_t idx) {
    if (idx >= text.size()) {
        return text.size();
    }
    const unsigned char b = static_cast<unsigned char>(text[idx]);
    std::size_t len = 1;
    if (b < 0x80) {
        len = 1;
    } else if ((b & 0xE0) == 0xC0) {
        len = 2;
    } else if ((b & 0xF0) == 0xE0) {
        len = 3;
    } else if ((b & 0xF8) == 0xF0) {
        len = 4;
    }
    if (idx + len > text.size()) {
        return text.size();
    }
    for (std::size_t k = 1; k < len; ++k) {
        if ((static_cast<unsigned char>(text[idx + k]) & 0xC0) != 0x80) {
            return idx + 1;
        }
    }
    return idx + len;
}

inline bool isWaylandFrontend(std::string_view frontend) {
    return frontend == "wayland" || frontend == "wayland_v2";
}

/// On `frontend:dbus`, only IM modules that advertise `CapabilityFlag::KeyEventOrderFix`
/// (GTK3, Qt5/Qt6) actually deliver `ForwardKey` events; GTK4 (`libim-fcitx5.so`) and
/// SDL drop `ForwardKey` as a no-op stub.
inline bool canForwardBackspace(std::string_view frontend,
                                CapabilityFlags flags) {
    if (frontend == "dbus") {
        return flags.test(CapabilityFlag::KeyEventOrderFix);
    }
    return true;
}

enum class DeleteCheckVerdict : uint8_t {
    NoCheck,
    StalePreCommit,
    Confirmed,
    DroppedDeletion,
    Inconclusive,
};

/// Tracks whether `InputContext::surroundingText()` has received a fresh
/// client update since the last client-mutating key/commit event, verifies
/// that `deleteSurroundingText` deletions actually landed, and latches a
/// per-`InputContext` circuit breaker (`surroundingUntrusted`) when an app
/// drops a deletion or ignores consecutive deletions.
struct SurroundingSyncTracker {
    static constexpr uint32_t kPrefixTailCap = 16;
    static constexpr uint32_t kMaxUnansweredDeletes = 3;

    struct PendingDeleteCheck {
        bool active = false;
        bool atBufferStart = false;
        uint32_t preCursorChars = 0;
        uint32_t deleteChars = 0;
        uint32_t prefixTailChars = 0;
        uint32_t expectedSuccessCursor = 0;
        uint32_t expectedDroppedCursor = 0;
        std::string prefixTailUtf8;
        std::string preTailUtf8;
        std::string expectedSuccessSuffix;
        std::string expectedDroppedSuffix;

        void clear() {
            active = false;
            atBufferStart = false;
            preCursorChars = 0;
            deleteChars = 0;
            prefixTailChars = 0;
            expectedSuccessCursor = 0;
            expectedDroppedCursor = 0;
            prefixTailUtf8.clear();
            preTailUtf8.clear();
            expectedSuccessSuffix.clear();
            expectedDroppedSuffix.clear();
        }
    };

    bool surroundingSynced = false;
    bool surroundingUntrusted = false;
    bool awaitingUpdateSinceDelete = false;
    uint32_t unansweredDeletes = 0;
    PendingDeleteCheck pendingCheck_{};

    void markClientMutated() {
        surroundingSynced = false;
        pendingCheck_.clear();
    }
    void markSurroundingUpdated() {
        surroundingSynced = true;
        awaitingUpdateSinceDelete = false;
        unansweredDeletes = 0;
    }
    void reset() {
        surroundingSynced = false;
        awaitingUpdateSinceDelete = false;
        pendingCheck_.clear();
    }
    void resetTrust() {
        reset();
        surroundingUntrusted = false;
        unansweredDeletes = 0;
    }

    bool canUseDeleteSurrounding(bool hasSurroundingCap,
                                 bool isSurroundingValid, uint32_t cursor,
                                 uint32_t deleteChars,
                                 std::string_view frontend) const {
        return !surroundingUntrusted && hasSurroundingCap &&
               isSurroundingValid && cursor >= deleteChars &&
               (!isWaylandFrontend(frontend) || surroundingSynced);
    }

    void checkUnansweredBeforeDelete(bool fallbackAvailable) {
        if (awaitingUpdateSinceDelete) {
            ++unansweredDeletes;
            awaitingUpdateSinceDelete = false;
            if (fallbackAvailable &&
                unansweredDeletes >= kMaxUnansweredDeletes) {
                surroundingUntrusted = true;
            }
        }
    }

    void armDeleteCheck(std::string_view preText, uint32_t preCursorChars,
                        uint32_t deleteChars, std::string_view insertedUtf8) {
        surroundingSynced = false;
        awaitingUpdateSinceDelete = true;
        pendingCheck_.clear();
        if (deleteChars == 0 || insertedUtf8.empty() ||
            preCursorChars < deleteChars || preCursorChars > 16384) {
            return;
        }
        const uint32_t delStartCp = preCursorChars - deleteChars;
        const uint32_t prefixTailStartCp =
            (delStartCp > kPrefixTailCap) ? (delStartCp - kPrefixTailCap) : 0;

        std::size_t byteIdx = 0;
        std::size_t prefixTailStartByte = 0;
        std::size_t delStartByte = 0;
        for (uint32_t cp = 0; cp < preCursorChars; ++cp) {
            if (cp == prefixTailStartCp) {
                prefixTailStartByte = byteIdx;
            }
            if (cp == delStartCp) {
                delStartByte = byteIdx;
            }
            if (byteIdx >= preText.size()) {
                return;
            }
            byteIdx = stepUtf8Codepoint(preText, byteIdx);
        }
        const std::size_t preCursorByte = byteIdx;

        pendingCheck_.active = true;
        pendingCheck_.atBufferStart = (delStartCp == 0);
        pendingCheck_.preCursorChars = preCursorChars;
        pendingCheck_.deleteChars = deleteChars;
        pendingCheck_.prefixTailChars = delStartCp - prefixTailStartCp;
        pendingCheck_.prefixTailUtf8 = std::string(preText.substr(
            prefixTailStartByte, delStartByte - prefixTailStartByte));
        const std::string_view deletedUtf8 =
            preText.substr(delStartByte, preCursorByte - delStartByte);
        pendingCheck_.preTailUtf8 = pendingCheck_.prefixTailUtf8;
        pendingCheck_.preTailUtf8.append(deletedUtf8);
        pendingCheck_.expectedSuccessSuffix = pendingCheck_.prefixTailUtf8;
        pendingCheck_.expectedSuccessSuffix.append(insertedUtf8);
        pendingCheck_.expectedDroppedSuffix = pendingCheck_.preTailUtf8;
        pendingCheck_.expectedDroppedSuffix.append(insertedUtf8);

        const uint32_t insertedChars = countUtf8Chars(insertedUtf8);
        pendingCheck_.expectedSuccessCursor = delStartCp + insertedChars;
        pendingCheck_.expectedDroppedCursor = preCursorChars + insertedChars;
    }

    DeleteCheckVerdict verifyPendingDelete(std::string_view text,
                                           uint32_t cursorChars,
                                           uint32_t anchorChars) {
        if (!pendingCheck_.active) {
            return DeleteCheckVerdict::NoCheck;
        }
        if (cursorChars > 16384) {
            pendingCheck_.clear();
            return DeleteCheckVerdict::Inconclusive;
        }
        std::size_t cursorByte = 0;
        for (uint32_t cp = 0; cp < cursorChars; ++cp) {
            if (cursorByte >= text.size()) {
                pendingCheck_.clear();
                return DeleteCheckVerdict::Inconclusive;
            }
            cursorByte = stepUtf8Codepoint(text, cursorByte);
        }
        const std::string_view before = text.substr(0, cursorByte);

        if (anchorChars == cursorChars) {
            const bool droppedMatch =
                pendingCheck_.atBufferStart
                    ? (cursorChars == pendingCheck_.expectedDroppedCursor &&
                       before == pendingCheck_.expectedDroppedSuffix)
                    : (before.ends_with(pendingCheck_.expectedDroppedSuffix) &&
                       (cursorChars == pendingCheck_.expectedDroppedCursor ||
                        pendingCheck_.prefixTailChars >= 4));
            if (droppedMatch) {
                pendingCheck_.clear();
                surroundingUntrusted = true;
                surroundingSynced = false;
                awaitingUpdateSinceDelete = false;
                unansweredDeletes = 0;
                return DeleteCheckVerdict::DroppedDeletion;
            }
        }

        const bool confirmedMatch =
            cursorChars == pendingCheck_.expectedSuccessCursor &&
            before.ends_with(pendingCheck_.expectedSuccessSuffix);

        if (!confirmedMatch &&
            cursorChars == pendingCheck_.preCursorChars &&
            before.ends_with(pendingCheck_.preTailUtf8)) {
            return DeleteCheckVerdict::StalePreCommit;
        }

        const uint32_t deletedCursor =
            pendingCheck_.preCursorChars - pendingCheck_.deleteChars;
        if (cursorChars == deletedCursor &&
            pendingCheck_.expectedSuccessCursor != cursorChars &&
            before.ends_with(pendingCheck_.prefixTailUtf8)) {
            pendingCheck_.clear();
            awaitingUpdateSinceDelete = false;
            unansweredDeletes = 0;
            return DeleteCheckVerdict::StalePreCommit;
        }

        if (confirmedMatch) {
            pendingCheck_.clear();
            awaitingUpdateSinceDelete = false;
            unansweredDeletes = 0;
            surroundingSynced = true;
            return DeleteCheckVerdict::Confirmed;
        }

        pendingCheck_.clear();
        return DeleteCheckVerdict::Inconclusive;
    }
};

/// Suppresses client-emitted `InputContextReset` echoes triggered by SmartKey's own
/// BackSpace + commit replacement burst (common on GTK/Gecko Wayland frontends during undo).
struct SelfReplaceResetGuard {
    static constexpr uint64_t kGuardMs = 150;

    uint64_t deadlineMs = 0;

    void markSelfReplace(uint64_t nowMs) { deadlineMs = nowMs + kGuardMs; }
    void clear() { deadlineMs = 0; }
    bool shouldIgnoreClientReset(bool isSelfCommitting,
                                 bool hasPendingDelayedCommit,
                                 uint64_t nowMs) const {
        return isSelfCommitting || hasPendingDelayedCommit ||
               (deadlineMs != 0 && nowMs <= deadlineMs);
    }
};

/// Pure state machine tracking `fcitx5-lotus` uinput replacement bursts, synthetic echo suppression,
/// and pending filtered word-break boundaries (`CommitStringEvent` deferred evaluation).
struct LotusBridgeTracker {
    /// Broad "Lotus may still be mutating the client" window, used ONLY for surrounding-text
    /// reconciliation guards, VK-echo bookkeeping, and boundary queuing. Exceeds `fcitx5-lotus`'s
    /// `kReplacementAckSlackMs = 200` + preDelay (<=20ms) + postDelay (10ms).
    static constexpr uint64_t kBurstInitialWindowMs = 350;
    static constexpr uint64_t kBurstEchoGraceMs = 300;

    /// Narrow window in which a `BackSpace`/`Left`/`Delete` may be a synthetic Lotus echo.
    /// Calibrated on 180 traced uinput bursts: trigger -> first echo <= 19 ms (with
    /// `DefaultCommitInterval=10`; the default 20 ms preDelay adds <= 10 ms), echo -> echo /
    /// `Pause` gaps <= 9 ms. Traced user BackSpaces arrive >= 144 ms after a letter and autorepeat
    /// at 40 ms; nothing here measures that, the separation is empirical. Ordinary letters must
    /// NOT clear the echo deadline (key rollover < 20 ms would leak the echoes). The old
    /// 350/300 ms window swallowed user BackSpaces and desynced the shadow buffer
    /// (`kiểu s<BS>cos` -> buffer `scos` -> autocorrect `sóc` deleting 4 chars).
    /// Raise these only if Lotus `Default*Interval`/`PostDelay` are configured far above defaults.
    static constexpr uint64_t kEchoBsFirstWindowMs = 45;
    static constexpr uint64_t kEchoBsChainGapMs = 30;

    uint64_t burstDeadlineMs = 0;
    uint64_t echoBsDeadlineMs = 0;
    uint64_t lastUserBackspaceMs = 0;
    uint32_t pendingBoundaryCp = 0;
    bool hasPendingBoundary = false;
    bool suppressPostKey = false;
    bool sawLotusActivityInWord = false;
    bool selfCommitting = false;
    std::vector<uint32_t> pendingPostBoundaryChars;
    std::string vkBoundaryCommitUtf8;

    /// A key that may make Lotus start a replacement burst (Telex trigger, boundary, ...).
    void armBurst(uint64_t nowMs) {
        burstDeadlineMs = nowMs + kBurstInitialWindowMs;
        echoBsDeadlineMs = nowMs + kEchoBsFirstWindowMs;
    }
    /// Evidence that a burst is in flight (echo, `Pause` anchor, VK rune, intermediate commit).
    /// Extends the echo window only by the short inter-echo gap — never self-sustaining for
    /// human-paced (>= 40 ms autorepeat) BackSpaces.
    void rearmBurstEcho(uint64_t nowMs) {
        burstDeadlineMs = nowMs + kBurstEchoGraceMs;
        echoBsDeadlineMs = nowMs + kEchoBsChainGapMs;
    }
    void clearBurst() {
        burstDeadlineMs = 0;
        echoBsDeadlineMs = 0;
    }
    bool isInBurst(uint64_t nowMs) const {
        return burstDeadlineMs != 0 && nowMs <= burstDeadlineMs;
    }
    bool expectsEchoBackspace(uint64_t nowMs) const {
        return echoBsDeadlineMs != 0 && nowMs <= echoBsDeadlineMs;
    }

    /// Diagnostics: a `Pause` anchor shortly after a BackSpace we classified as the user's means
    /// that BackSpace may have been a late Lotus echo (trace label `late_echo_suspect`).
    static constexpr uint64_t kLateEchoLookbackMs = 120;
    void noteUserBackspace(uint64_t nowMs) { lastUserBackspaceMs = nowMs; }
    void clearUserBackspace() { lastUserBackspaceMs = 0; }
    bool takeLateEchoSuspect(uint64_t nowMs) {
        const bool suspect = lastUserBackspaceMs != 0 &&
                             nowMs >= lastUserBackspaceMs &&
                             nowMs - lastUserBackspaceMs <= kLateEchoLookbackMs;
        lastUserBackspaceMs = 0;
        return suspect;
    }

    void setSuppressPost(bool v) { suppressPostKey = v; }
    bool takeSuppressPost() {
        const bool v = suppressPostKey;
        suppressPostKey = false;
        return v;
    }

    void setBoundary(uint32_t cp) {
        pendingBoundaryCp = cp;
        hasPendingBoundary = true;
    }
    uint32_t boundaryCp() const { return pendingBoundaryCp; }
    uint32_t takeBoundary() {
        const uint32_t cp = pendingBoundaryCp;
        pendingBoundaryCp = 0;
        hasPendingBoundary = false;
        return cp;
    }
    void clearBoundary() {
        pendingBoundaryCp = 0;
        hasPendingBoundary = false;
        pendingPostBoundaryChars.clear();
        vkBoundaryCommitUtf8.clear();
    }

    void markActivity() { sawLotusActivityInWord = true; }
    bool hasActivityInWord() const { return sawLotusActivityInWord; }
    bool takeActivityInWord() {
        const bool v = sawLotusActivityInWord;
        sawLotusActivityInWord = false;
        return v;
    }

    void pushPostBoundaryChar(uint32_t cp) {
        pendingPostBoundaryChars.push_back(cp);
    }
    std::vector<uint32_t> takePostBoundaryChars() {
        auto out = std::move(pendingPostBoundaryChars);
        pendingPostBoundaryChars.clear();
        return out;
    }

    void appendVkBoundaryCommit(std::string_view s) {
        vkBoundaryCommitUtf8.append(s);
    }
    const std::string &vkBoundaryCommit() const {
        return vkBoundaryCommitUtf8;
    }
    std::string takeVkBoundaryCommit() {
        auto out = std::move(vkBoundaryCommitUtf8);
        vkBoundaryCommitUtf8.clear();
        return out;
    }

    void reset() {
        burstDeadlineMs = 0;
        echoBsDeadlineMs = 0;
        lastUserBackspaceMs = 0;
        pendingBoundaryCp = 0;
        hasPendingBoundary = false;
        suppressPostKey = false;
        sawLotusActivityInWord = false;
        pendingPostBoundaryChars.clear();
        vkBoundaryCommitUtf8.clear();
    }
};

/// Appends RFC 8259 JSON-escaped UTF-8 to `out`, substituting `\ufffd` for invalid UTF-8 bytes
/// so every emitted trace line is guaranteed valid UTF-8 JSON.
inline void appendJsonEscaped(std::string &out, std::string_view sv) {
    const std::size_t n = sv.size();
    std::size_t i = 0;
    while (i < n) {
        const unsigned char b0 = static_cast<unsigned char>(sv[i]);
        if (b0 < 0x80) {
            switch (b0) {
            case '"':
                out.append("\\\"");
                break;
            case '\\':
                out.append("\\\\");
                break;
            case '\b':
                out.append("\\b");
                break;
            case '\f':
                out.append("\\f");
                break;
            case '\n':
                out.append("\\n");
                break;
            case '\r':
                out.append("\\r");
                break;
            case '\t':
                out.append("\\t");
                break;
            default:
                if (b0 < 0x20 || b0 == 0x7F) {
                    char buf[7]{};
                    std::snprintf(buf, sizeof(buf), "\\u%04x", b0);
                    out.append(buf, 6);
                } else {
                    out.push_back(static_cast<char>(b0));
                }
                break;
            }
            ++i;
            continue;
        }
        if (b0 >= 0xC2 && b0 <= 0xDF && i + 1 < n) {
            const unsigned char b1 = static_cast<unsigned char>(sv[i + 1]);
            if ((b1 & 0xC0) == 0x80) {
                out.append(sv.data() + i, 2);
                i += 2;
                continue;
            }
        } else if (b0 >= 0xE0 && b0 <= 0xEF && i + 2 < n) {
            const unsigned char b1 = static_cast<unsigned char>(sv[i + 1]);
            const unsigned char b2 = static_cast<unsigned char>(sv[i + 2]);
            const bool validLead = (b0 != 0xE0 || b1 >= 0xA0) &&
                                   (b0 != 0xED || b1 <= 0x9F);
            if (validLead && (b1 & 0xC0) == 0x80 && (b2 & 0xC0) == 0x80) {
                out.append(sv.data() + i, 3);
                i += 3;
                continue;
            }
        } else if (b0 >= 0xF0 && b0 <= 0xF4 && i + 3 < n) {
            const unsigned char b1 = static_cast<unsigned char>(sv[i + 1]);
            const unsigned char b2 = static_cast<unsigned char>(sv[i + 2]);
            const unsigned char b3 = static_cast<unsigned char>(sv[i + 3]);
            const bool validLead = (b0 != 0xF0 || b1 >= 0x90) &&
                                   (b0 != 0xF4 || b1 <= 0x8F);
            if (validLead && (b1 & 0xC0) == 0x80 && (b2 & 0xC0) == 0x80 &&
                (b3 & 0xC0) == 0x80) {
                out.append(sv.data() + i, 4);
                i += 4;
                continue;
            }
        }
        out.append("\\ufffd");
        ++i;
    }
}

inline void appendJsonString(std::string &out, std::string_view sv) {
    out.push_back('"');
    appendJsonEscaped(out, sv);
    out.push_back('"');
}

/// Extracts a bounded Unicode-scalar window around `cursorChars` (Fcitx5 SurroundingText uses
/// Unicode scalar counts, not UTF-8 byte offsets), marking the cursor with `|`.
inline std::string extractSurroundingSlice(std::string_view text,
                                           uint32_t cursorChars,
                                           uint32_t anchorChars,
                                           uint32_t maxBefore = 24,
                                           uint32_t maxAfter = 8) {
    if (cursorChars > 16384) {
        return "<cursor=" + std::to_string(cursorChars) +
               " bytes=" + std::to_string(text.size()) + ">";
    }

    const std::size_t ringCap = static_cast<std::size_t>(maxBefore) + 1;
    std::size_t stackRing[65]{};
    std::vector<std::size_t> heapRing;
    std::size_t *ring = stackRing;
    if (ringCap > 65) {
        heapRing.resize(ringCap, 0);
        ring = heapRing.data();
    }

    std::size_t cursorByte = 0;
    for (uint32_t cp = 0; cp < cursorChars; ++cp) {
        if (cursorByte >= text.size()) {
            return "<invalid-cursor>";
        }
        cursorByte = stepUtf8Codepoint(text, cursorByte);
        ring[(cp + 1) % ringCap] = cursorByte;
    }

    const bool truncatedBefore = cursorChars > maxBefore;
    const std::size_t beforeStartByte =
        truncatedBefore ? ring[(cursorChars - maxBefore) % ringCap] : 0;

    std::size_t afterEndByte = cursorByte;
    for (uint32_t a = 0; a < maxAfter && afterEndByte < text.size(); ++a) {
        afterEndByte = stepUtf8Codepoint(text, afterEndByte);
    }
    const bool truncatedAfter = afterEndByte < text.size();

    std::string out;
    out.reserve((cursorByte - beforeStartByte) + (afterEndByte - cursorByte) + 24);
    if (truncatedBefore) {
        out.append("\xE2\x80\xA6");
    }
    out.append(text.substr(beforeStartByte, cursorByte - beforeStartByte));
    out.push_back('|');
    out.append(text.substr(cursorByte, afterEndByte - cursorByte));
    if (truncatedAfter) {
        out.append("\xE2\x80\xA6");
    }
    if (anchorChars != cursorChars) {
        out.append(" [sel=");
        out.append(std::to_string(anchorChars));
        out.push_back(']');
    }
    return out;
}

inline std::string formatSessionCompact(const SmartKeySessionSnapshot &snap) {
    const std::string_view mode =
        snap.mode_utf8 != nullptr ? std::string_view(snap.mode_utf8) : "Unknown";
    const std::string_view buf =
        (snap.buf_utf8 != nullptr && snap.buf_byte_len > 0)
            ? std::string_view(snap.buf_utf8, snap.buf_byte_len)
            : std::string_view{};
    const std::string_view aux =
        (snap.aux_utf8 != nullptr && snap.aux_byte_len > 0)
            ? std::string_view(snap.aux_utf8, snap.aux_byte_len)
            : std::string_view{};
    const std::string_view prev =
        (snap.prev_utf8 != nullptr && snap.prev_byte_len > 0)
            ? std::string_view(snap.prev_utf8, snap.prev_byte_len)
            : std::string_view{};

    std::string out;
    out.reserve(mode.size() + buf.size() + aux.size() + prev.size() + 24);
    if (mode == "Idle") {
        out.append("Idle");
    } else if (mode == "Composing") {
        out.append("Composing(");
        out.append(buf);
        if (!aux.empty()) {
            out.append(",peak=");
            out.append(aux);
        }
        out.push_back(')');
    } else if (mode == "PendingDot" || mode == "PendingSemi") {
        out.append(mode);
        out.push_back('(');
        out.append(buf);
        out.push_back(')');
    } else if (mode == "AfterWord") {
        out.append("AfterWord(");
        out.append(buf);
        out.append(",sp=");
        out.append(std::to_string(snap.trailing_count));
        out.push_back(')');
    } else if (mode == "JustAutocorrected") {
        out.append("JustAutocorrected(");
        out.append(buf);
        out.append("<-");
        out.append(aux);
        out.append(",tb=");
        out.append(std::to_string(snap.trailing_count));
        out.push_back(')');
    } else if (mode == "EditingRestored" || mode == "EditingPrevious") {
        out.append(mode);
        out.push_back('(');
        out.append(buf);
        out.append("<-");
        out.append(aux);
        out.push_back(')');
    } else {
        out.append(mode);
    }
    if (!prev.empty()) {
        out.append("[prev=");
        out.append(prev);
        out.push_back(']');
    }
    if (!snap.lotus_composing) {
        out.append("[lc=0]");
    }
    return out;
}

struct SurroundingTraceDedup {
    bool valid = false;
    uint32_t cursor = 0;
    uint32_t anchor = 0;
    bool synced = false;
    std::string slice;

    bool shouldEmit(uint32_t c, uint32_t a, bool sy, std::string_view sl) {
        if (valid && cursor == c && anchor == a && synced == sy && slice == sl) {
            return false;
        }
        valid = true;
        cursor = c;
        anchor = a;
        synced = sy;
        slice.assign(sl);
        return true;
    }

    void reset() {
        valid = false;
        slice.clear();
    }
};

class TraceLogger {
public:
    static constexpr uint64_t kDefaultMaxFileBytes = 16ULL * 1024ULL * 1024ULL;

    TraceLogger() { lineBuf_.reserve(512); }
    ~TraceLogger() { closeIfOpen(); }

    TraceLogger(const TraceLogger &) = delete;
    TraceLogger &operator=(const TraceLogger &) = delete;

    void setLogDir(std::filesystem::path dir) {
        closeIfOpen();
        logDir_ = std::move(dir);
    }

    void setMaxFileBytes(uint64_t maxBytes) {
        maxFileBytes_ = maxBytes > 0 ? maxBytes : 1;
    }

    bool isOpen() const { return fd_ >= 0; }

    void closeIfOpen() {
        if (fd_ >= 0) {
            ::close(fd_);
            fd_ = -1;
        }
        openUtcDay_ = -1;
        bytesWritten_ = 0;
    }

    static std::string formatUtcDate(int64_t utcDays) {
        const int64_t z = utcDays + 719468;
        const int64_t era = (z >= 0 ? z : z - 146096) / 146097;
        const uint32_t doe = static_cast<uint32_t>(z - era * 146097);
        const uint32_t yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
        const int64_t y = static_cast<int64_t>(yoe) + era * 400;
        const uint32_t doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        const uint32_t mp = (5 * doy + 2) / 153;
        const uint32_t d = doy - (153 * mp + 2) / 5 + 1;
        const uint32_t m = mp < 10 ? mp + 3 : mp - 9;
        const int year = static_cast<int>(m <= 2 ? y + 1 : y);
        char buf[36]{};
        std::snprintf(buf, sizeof(buf), "%04d-%02u-%02u", year, m, d);
        return std::string(buf);
    }

    uint64_t nextDeltaMs(uint64_t monoMs) {
        if (!hasLastMono_) {
            hasLastMono_ = true;
            lastMonoMs_ = monoMs;
            return 0;
        }
        const uint64_t delta = monoMs >= lastMonoMs_ ? (monoMs - lastMonoMs_) : 0;
        lastMonoMs_ = monoMs;
        return delta;
    }

    std::string &beginLine() {
        lineBuf_.clear();
        return lineBuf_;
    }

    void commitLine(uint64_t epochMs) {
        lineBuf_.push_back('\n');
        ensureOpen(epochMs);
        if (fd_ < 0) {
            return;
        }
        const char *ptr = lineBuf_.data();
        std::size_t remaining = lineBuf_.size();
        while (remaining > 0) {
            const ssize_t n = ::write(fd_, ptr, remaining);
            if (n > 0) {
                const auto written = static_cast<std::size_t>(n);
                ptr += written;
                remaining -= written;
                bytesWritten_ += written;
            } else if (n < 0 && errno == EINTR) {
                continue;
            } else {
                closeIfOpen();
                break;
            }
        }
    }

private:
    std::filesystem::path pathForDay(int64_t utcDay, bool rotated) const {
        std::string name = "trace-";
        name += formatUtcDate(utcDay);
        name += rotated ? ".jsonl.1" : ".jsonl";
        return logDir_ / name;
    }

    void rotateActiveFile(int64_t utcDay) {
        closeIfOpen();
        const auto activePath = pathForDay(utcDay, false);
        const auto rotatedPath = pathForDay(utcDay, true);
        std::error_code ec;
        std::filesystem::rename(activePath, rotatedPath, ec);
        fd_ = ::open(activePath.c_str(),
                     O_CREAT | O_WRONLY | O_TRUNC | O_APPEND | O_CLOEXEC,
                     0600);
        if (fd_ >= 0) {
            (void)::fchmod(fd_, 0600);
            openUtcDay_ = utcDay;
            bytesWritten_ = 0;
        }
    }

    void ensureOpen(uint64_t epochMs) {
        if (logDir_.empty()) {
            return;
        }
        const int64_t utcDay = static_cast<int64_t>(epochMs / 86400000ULL);
        if (fd_ >= 0 && openUtcDay_ != utcDay) {
            closeIfOpen();
        }
        if (fd_ >= 0 && bytesWritten_ >= maxFileBytes_) {
            rotateActiveFile(utcDay);
        }
        if (fd_ >= 0) {
            return;
        }

        std::error_code ec;
        std::filesystem::create_directories(logDir_, ec);
        (void)::chmod(logDir_.c_str(), 0700);
        const auto activePath = pathForDay(utcDay, false);
        fd_ = ::open(activePath.c_str(),
                     O_CREAT | O_WRONLY | O_APPEND | O_CLOEXEC, 0600);
        if (fd_ < 0) {
            return;
        }
        (void)::fchmod(fd_, 0600);
        struct stat st{};
        if (::fstat(fd_, &st) == 0 && st.st_size > 0) {
            bytesWritten_ = static_cast<uint64_t>(st.st_size);
        } else {
            bytesWritten_ = 0;
        }
        openUtcDay_ = utcDay;
        if (bytesWritten_ >= maxFileBytes_) {
            rotateActiveFile(utcDay);
        }
    }

    std::filesystem::path logDir_;
    int fd_ = -1;
    int64_t openUtcDay_ = -1;
    uint64_t bytesWritten_ = 0;
    uint64_t maxFileBytes_ = kDefaultMaxFileBytes;
    uint64_t lastMonoMs_ = 0;
    bool hasLastMono_ = false;
    std::string lineBuf_;
};

class SmartKeyState final : public InputContextProperty {
public:
    SmartKeyState() : session_(smartkey_session_new()) {
        static std::atomic<uint32_t> nextId{1};
        icId_ = nextId.fetch_add(1, std::memory_order_relaxed);
    }
    ~SmartKeyState() override {
        cancelPendingDelayedCommit();
        if (session_ != nullptr) {
            smartkey_session_free(session_);
            session_ = nullptr;
        }
    }

    SmartKeyState(const SmartKeyState &) = delete;
    SmartKeyState &operator=(const SmartKeyState &) = delete;

    SmartKeySessionHandle *session() const { return session_; }
    uint32_t icId() const { return icId_; }

    void armLotusBurst(uint64_t nowMs) { lotus_.armBurst(nowMs); }
    void rearmLotusBurstEcho(uint64_t nowMs) { lotus_.rearmBurstEcho(nowMs); }
    void clearLotusBurst() { lotus_.clearBurst(); }
    bool isInLotusBurst(uint64_t nowMs) const { return lotus_.isInBurst(nowMs); }
    bool expectsLotusEchoBackspace(uint64_t nowMs) const {
        return lotus_.expectsEchoBackspace(nowMs);
    }
    void noteLotusUserBackspace(uint64_t nowMs) { lotus_.noteUserBackspace(nowMs); }
    void clearLotusUserBackspace() { lotus_.clearUserBackspace(); }
    bool takeLotusLateEchoSuspect(uint64_t nowMs) {
        return lotus_.takeLateEchoSuspect(nowMs);
    }

    void setSuppressPostKey(bool v) { lotus_.setSuppressPost(v); }
    bool takeSuppressPostKey() { return lotus_.takeSuppressPost(); }

    void setPendingBoundary(uint32_t cp) { lotus_.setBoundary(cp); }
    bool hasPendingBoundary() const { return lotus_.hasPendingBoundary; }
    uint32_t pendingBoundaryCp() const { return lotus_.boundaryCp(); }
    uint32_t takePendingBoundary() { return lotus_.takeBoundary(); }
    void clearPendingBoundary() { lotus_.clearBoundary(); }

    void markLotusActivity() {
        lotus_.markActivity();
        surroundingSync_.markClientMutated();
    }
    bool hasLotusActivityInWord() const { return lotus_.hasActivityInWord(); }
    bool takeLotusActivityInWord() { return lotus_.takeActivityInWord(); }

    void pushPostBoundaryChar(uint32_t cp) { lotus_.pushPostBoundaryChar(cp); }
    std::size_t postBoundaryQueueLen() const {
        return lotus_.pendingPostBoundaryChars.size();
    }
    std::vector<uint32_t> takePostBoundaryChars() {
        return lotus_.takePostBoundaryChars();
    }

    void appendVkBoundaryCommit(std::string_view s) {
        lotus_.appendVkBoundaryCommit(s);
    }
    const std::string &vkBoundaryCommit() const {
        return lotus_.vkBoundaryCommit();
    }
    std::string takeVkBoundaryCommit() {
        return lotus_.takeVkBoundaryCommit();
    }

    bool isSelfCommitting() const { return lotus_.selfCommitting; }
    void setSelfCommitting(bool v) { lotus_.selfCommitting = v; }

    void resetLotusTracking() { lotus_.reset(); }

    void markClientMutated() { surroundingSync_.markClientMutated(); }
    void markSurroundingUpdated() { surroundingSync_.markSurroundingUpdated(); }
    void resetSurroundingSync() {
        surroundingSync_.reset();
        surroundDedup_.reset();
    }
    void resetSurroundingTrust() {
        surroundingSync_.resetTrust();
        surroundDedup_.reset();
    }
    bool isSurroundingSynced() const {
        return surroundingSync_.surroundingSynced;
    }
    bool isSurroundingUntrusted() const {
        return surroundingSync_.surroundingUntrusted;
    }
    void checkUnansweredBeforeDelete(bool fallbackAvailable) {
        surroundingSync_.checkUnansweredBeforeDelete(fallbackAvailable);
    }
    void armSurroundingDeleteCheck(std::string_view preText,
                                   uint32_t preCursorChars,
                                   uint32_t deleteChars,
                                   std::string_view insertedUtf8) {
        surroundingSync_.armDeleteCheck(preText, preCursorChars, deleteChars,
                                        insertedUtf8);
    }
    DeleteCheckVerdict verifyPendingSurroundingDelete(std::string_view text,
                                                      uint32_t cursorChars,
                                                      uint32_t anchorChars) {
        return surroundingSync_.verifyPendingDelete(text, cursorChars,
                                                    anchorChars);
    }
    bool shouldEmitSurroundTrace(uint32_t c, uint32_t a, bool sy,
                                 std::string_view sl) {
        return surroundDedup_.shouldEmit(c, a, sy, sl);
    }
    bool canUseDeleteSurrounding(bool hasSurroundingCap,
                                 bool isSurroundingValid, uint32_t cursor,
                                 uint32_t deleteChars,
                                 std::string_view frontend) const {
        return surroundingSync_.canUseDeleteSurrounding(
            hasSurroundingCap, isSurroundingValid, cursor, deleteChars,
            frontend);
    }

    void setPendingDelayedCommit(std::string text,
                                 std::unique_ptr<EventSourceTime> timer) {
        cancelPendingDelayedCommit();
        pendingDelayedCommit_ = std::move(text);
        delayedCommitTimer_ = std::move(timer);
    }
    bool hasPendingDelayedCommit() const {
        return !pendingDelayedCommit_.empty();
    }
    std::string takePendingDelayedCommit() {
        auto timer = std::move(delayedCommitTimer_);
        if (timer) {
            timer->setEnabled(false);
        }
        std::string out = std::move(pendingDelayedCommit_);
        pendingDelayedCommit_.clear();
        return out;
    }
    void cancelPendingDelayedCommit() {
        auto timer = std::move(delayedCommitTimer_);
        if (timer) {
            timer->setEnabled(false);
        }
        pendingDelayedCommit_.clear();
    }

    static constexpr uint64_t kSelfReplaceResetGuardMs =
        SelfReplaceResetGuard::kGuardMs;

    void markSelfReplace(uint64_t nowMs) {
        selfReplaceGuard_.markSelfReplace(nowMs);
    }
    void clearSelfReplaceGuard() { selfReplaceGuard_.clear(); }
    bool shouldIgnoreClientReset(uint64_t nowMs) const {
        return selfReplaceGuard_.shouldIgnoreClientReset(
            isSelfCommitting(), hasPendingDelayedCommit(), nowMs);
    }

    bool shouldLogProfileTransition(uint8_t profile, std::string_view reason) {
        if (lastLoggedProfile_ == profile && lastLoggedReason_ == reason) {
            return false;
        }
        lastLoggedProfile_ = profile;
        lastLoggedReason_ = std::string(reason);
        return true;
    }

private:
    SmartKeySessionHandle *session_ = nullptr;
    uint32_t icId_ = 0;
    LotusBridgeTracker lotus_{};
    SurroundingSyncTracker surroundingSync_{};
    SurroundingTraceDedup surroundDedup_{};
    SelfReplaceResetGuard selfReplaceGuard_{};
    std::string pendingDelayedCommit_;
    std::unique_ptr<EventSourceTime> delayedCommitTimer_;
    uint8_t lastLoggedProfile_ = 0xFF;
    std::string lastLoggedReason_;
};

FCITX_CONFIGURATION(
    SmartKeyConfig,
    KeyListOption toggleKey{this, "ToggleKey",
                            "Toggle SmartKey Autocorrect",
                            {}, KeyListConstrain()};);

class SmartKeyAddon final : public AddonInstance {
public:
    explicit SmartKeyAddon(Instance *instance);
    ~SmartKeyAddon() override;

    const Configuration *getConfig() const override { return &config_; }
    void setConfig(const RawConfig &config) override;
    void reloadConfig() override;
    void save() override;

private:
    void onKeyEvent(KeyEvent &keyEvent);
    void onPostKeyEvent(KeyEvent &keyEvent);
    void onCommitString(CommitStringEvent &commitEvent);
    void onUpdatePreedit(Event &event);
    void onSurroundingTextUpdated(Event &event);
    void focusOutContext(InputContext *ic);
    void onInputContextReset(InputContext *ic);
    void onSwitchInputMethod(InputContext *ic);
    void resetContext(InputContext *ic);
    std::string resolveDirectKeyboardLang(InputContext *ic) const;
    bool isLotusActive(InputContext *ic) const;
    uint8_t classifyProfile(InputContext *ic, std::string_view appName,
                            const char **outReason = nullptr) const;
    bool isTraceActive();
    bool canTraceContext(InputContext *ic, std::string_view appName);
    void flushPendingDelayedCommit(InputContext *ic, SmartKeyState *state);
    void disarmSessionJustAutocorrected(SmartKeyState *state, uint64_t nowMs);
    void applyDeleteAndCommit(InputContext *ic, SmartKeyState *state,
                              const SmartKeyAction &act);
    void applyReplaceAction(InputContext *ic, SmartKeyState *state,
                            KeyEvent &keyEvent,
                            const SmartKeyAction &act);
    void showToggleTip(bool enabled, bool ok);

    Instance *instance_ = nullptr;
    SmartKeyEngineHandle *engine_ = nullptr;
    SmartKeyConfig config_{};
    FactoryFor<SmartKeyState> factory_;
    TraceLogger trace_{};
    std::vector<std::unique_ptr<HandlerTableEntry<EventHandler>>> eventWatchers_;
};

class SmartKeyAddonFactory final : public AddonFactory {
public:
    AddonInstance *create(AddonManager *manager) override {
        return new SmartKeyAddon(manager->instance());
    }
};

} // namespace fcitx::smartkey

#endif // FCITX5_SMARTKEY_ADDON_H

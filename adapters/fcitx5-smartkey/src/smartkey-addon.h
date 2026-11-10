/*
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#ifndef FCITX5_SMARTKEY_ADDON_H
#define FCITX5_SMARTKEY_ADDON_H

#include <algorithm>
#include <cctype>
#include <cstdint>
#include <initializer_list>
#include <memory>
#include <string>
#include <string_view>
#include <vector>

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
    if (const auto slash = raw.rfind('/'); slash != std::string_view::npos) {
        raw.remove_prefix(slash + 1);
    }
    std::string lower(raw);
    std::transform(lower.begin(), lower.end(), lower.begin(),
                   [](unsigned char c) { return static_cast<char>(std::tolower(c)); });
    return lower;
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
            "zed", "zed-editor", "nvim", "nvim-qt", "neovide", "vim",
            "gvim", "emacs", "helix", "sublime_text", "sublimetext", "subl",
            "idea", "clion", "pycharm", "rustrover", "goland", "webstorm",
            "datagrip", "phpstorm", "rider", "rubymine", "aqua", "fleet",
            "mps", "android-studio", "androidstudio", "rstudio", "qtcreator",
            "geany", "lapce", "lite-xl", "kakoune", "micro"
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
            "caprine"
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

/// Pure state machine tracking `fcitx5-lotus` uinput replacement bursts, synthetic echo suppression,
/// and pending filtered word-break boundaries (`CommitStringEvent` deferred evaluation).
struct LotusBridgeTracker {
    /// Exceeds `fcitx5-lotus`'s `kReplacementAckSlackMs = 200` + preDelay (<=20ms) + postDelay (10ms).
    static constexpr uint64_t kBurstInitialWindowMs = 350;
    static constexpr uint64_t kBurstEchoGraceMs = 300;

    uint64_t burstDeadlineMs = 0;
    uint32_t pendingBoundaryCp = 0;
    bool hasPendingBoundary = false;
    bool suppressPostKey = false;
    bool sawLotusActivityInWord = false;
    bool selfCommitting = false;

    void armBurst(uint64_t nowMs) {
        burstDeadlineMs = nowMs + kBurstInitialWindowMs;
    }
    void rearmBurstEcho(uint64_t nowMs) {
        burstDeadlineMs = nowMs + kBurstEchoGraceMs;
    }
    void clearBurst() { burstDeadlineMs = 0; }
    bool isInBurst(uint64_t nowMs) const {
        return burstDeadlineMs != 0 && nowMs <= burstDeadlineMs;
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
    uint32_t takeBoundary() {
        const uint32_t cp = pendingBoundaryCp;
        pendingBoundaryCp = 0;
        hasPendingBoundary = false;
        return cp;
    }
    void clearBoundary() {
        pendingBoundaryCp = 0;
        hasPendingBoundary = false;
    }

    void markActivity() { sawLotusActivityInWord = true; }
    bool takeActivityInWord() {
        const bool v = sawLotusActivityInWord;
        sawLotusActivityInWord = false;
        return v;
    }

    void reset() {
        burstDeadlineMs = 0;
        pendingBoundaryCp = 0;
        hasPendingBoundary = false;
        suppressPostKey = false;
        sawLotusActivityInWord = false;
    }
};

class SmartKeyState final : public InputContextProperty {
public:
    SmartKeyState() : session_(smartkey_session_new()) {}
    ~SmartKeyState() override {
        if (session_ != nullptr) {
            smartkey_session_free(session_);
            session_ = nullptr;
        }
    }

    SmartKeyState(const SmartKeyState &) = delete;
    SmartKeyState &operator=(const SmartKeyState &) = delete;

    SmartKeySessionHandle *session() const { return session_; }

    void armLotusBurst(uint64_t nowMs) { lotus_.armBurst(nowMs); }
    void rearmLotusBurstEcho(uint64_t nowMs) { lotus_.rearmBurstEcho(nowMs); }
    void clearLotusBurst() { lotus_.clearBurst(); }
    bool isInLotusBurst(uint64_t nowMs) const { return lotus_.isInBurst(nowMs); }

    void setSuppressPostKey(bool v) { lotus_.setSuppressPost(v); }
    bool takeSuppressPostKey() { return lotus_.takeSuppressPost(); }

    void setPendingBoundary(uint32_t cp) { lotus_.setBoundary(cp); }
    bool hasPendingBoundary() const { return lotus_.hasPendingBoundary; }
    uint32_t takePendingBoundary() { return lotus_.takeBoundary(); }
    void clearPendingBoundary() { lotus_.clearBoundary(); }

    void markLotusActivity() { lotus_.markActivity(); }
    bool takeLotusActivityInWord() { return lotus_.takeActivityInWord(); }

    bool isSelfCommitting() const { return lotus_.selfCommitting; }
    void setSelfCommitting(bool v) { lotus_.selfCommitting = v; }

    void resetLotusTracking() { lotus_.reset(); }

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
    LotusBridgeTracker lotus_{};
    uint8_t lastLoggedProfile_ = 0xFF;
    std::string lastLoggedReason_;
};

class SmartKeyAddon final : public AddonInstance {
public:
    explicit SmartKeyAddon(Instance *instance);
    ~SmartKeyAddon() override;

    void reloadConfig() override;
    void save() override;

private:
    void onKeyEvent(KeyEvent &keyEvent);
    void onPostKeyEvent(KeyEvent &keyEvent);
    void onCommitString(CommitStringEvent &commitEvent);
    void onUpdatePreedit(Event &event);
    void onSurroundingTextUpdated(Event &event);
    void focusOutContext(InputContext *ic);
    void resetContext(InputContext *ic);
    bool isEnglishKeyboardActive(InputContext *ic) const;
    bool isLotusActive(InputContext *ic) const;
    uint8_t classifyProfile(InputContext *ic, std::string_view appName,
                            const char **outReason = nullptr) const;
    static void applyDeleteAndCommit(InputContext *ic, SmartKeyState *state,
                                     const SmartKeyAction &act);
    static void applyReplaceAction(InputContext *ic, SmartKeyState *state,
                                   KeyEvent &keyEvent,
                                   const SmartKeyAction &act);

    Instance *instance_ = nullptr;
    SmartKeyEngineHandle *engine_ = nullptr;
    FactoryFor<SmartKeyState> factory_;
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

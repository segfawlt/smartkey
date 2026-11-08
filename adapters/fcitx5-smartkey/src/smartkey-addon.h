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
            "idea", "clion", "pycharm", "rustrover", "goland", "webstorm"
        }) ||
        matchesAnySubstring(prog, {"terminal", "jetbrains-"})) {
        return SMARTKEY_PROFILE_BYPASS;
    }

    if (matchesExactOrAppSegment(prog, {
            "firefox", "firefox-developer-edition", "librewolf", "waterfox",
            "zen", "zen-bin", "zen_browser", "chromium", "chrome",
            "google-chrome", "google-chrome-stable", "brave", "brave-browser",
            "vivaldi", "vivaldi-stable", "epiphany"
        }) ||
        matchesAnySubstring(prog, {"firefox", "chromium", "chrome"})) {
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

    void armLotusBurst(uint64_t nowMs) { lotusBurstDeadlineMs_ = nowMs + 150; }
    void clearLotusBurst() { lotusBurstDeadlineMs_ = 0; }
    bool isInLotusBurst(uint64_t nowMs) const {
        return lotusBurstDeadlineMs_ != 0 && nowMs <= lotusBurstDeadlineMs_;
    }

    void setLotusFilteredBoundary(bool v) { lotusFilteredBoundary_ = v; }
    bool takeLotusFilteredBoundary() {
        const bool v = lotusFilteredBoundary_;
        lotusFilteredBoundary_ = false;
        return v;
    }

    void resetLotusTracking() {
        lotusBurstDeadlineMs_ = 0;
        lotusFilteredBoundary_ = false;
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
    uint64_t lotusBurstDeadlineMs_ = 0;
    bool lotusFilteredBoundary_ = false;
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
    void resetContext(InputContext *ic);
    bool isEnglishKeyboardActive(InputContext *ic) const;
    bool isLotusActive(InputContext *ic) const;
    static uint8_t classifyProfile(InputContext *ic, const char **outReason = nullptr);
    static void applyReplaceAction(InputContext *ic, KeyEvent &keyEvent,
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

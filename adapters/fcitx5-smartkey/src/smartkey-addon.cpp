/*
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#include "smartkey-addon.h"

#include <algorithm>
#include <cctype>
#include <chrono>
#include <cstdlib>
#include <filesystem>
#include <string>
#include <string_view>

#include <fcitx-utils/capabilityflags.h>
#include <fcitx-utils/key.h>
#include <fcitx-utils/keysym.h>
#include <fcitx-utils/log.h>
#include <fcitx/inputcontextmanager.h>

namespace fcitx::smartkey {
namespace {

uint64_t monotonicNowMs() {
    using namespace std::chrono;
    return static_cast<uint64_t>(
        duration_cast<milliseconds>(steady_clock::now().time_since_epoch())
            .count());
}

std::filesystem::path resolveConfigDir() {
    if (const char *xdg = std::getenv("XDG_CONFIG_HOME");
        xdg != nullptr && xdg[0] != '\0') {
        return std::filesystem::path(xdg) / "smartkey";
    }
    if (const char *home = std::getenv("HOME");
        home != nullptr && home[0] != '\0') {
        return std::filesystem::path(home) / ".config" / "smartkey";
    }
    return std::filesystem::path(".smartkey");
}

} // namespace

SmartKeyAddon::SmartKeyAddon(Instance *instance)
    : instance_(instance),
      factory_([](InputContext &) { return new SmartKeyState(); }) {
    const auto cfgDir = resolveConfigDir();
    const auto rulesPath = (cfgDir / "rules.toml").string();
    const auto statePath = (cfgDir / "learner_state.json").string();

    engine_ = smartkey_engine_new(rulesPath.c_str(), statePath.c_str(),
                                  SMARTKEY_PRESET_BALANCED);
    if (engine_ == nullptr) {
        FCITX_ERROR() << "SmartKey failed to initialize engine at " << rulesPath;
        return;
    }

    FCITX_INFO() << "SmartKey loaded (rules: " << rulesPath
                 << ", state: " << statePath << ")";

    if (instance_ == nullptr) {
        return;
    }

    instance_->inputContextManager().registerProperty("smartkeyState",
                                                      &factory_);

    eventWatchers_.emplace_back(instance_->watchEvent(
        EventType::InputContextKeyEvent, EventWatcherPhase::PreInputMethod,
        [this](Event &event) {
            onKeyEvent(static_cast<KeyEvent &>(event));
        }));

    auto resetWatcher = [this](Event &event) {
        auto &icEvent = static_cast<InputContextEvent &>(event);
        resetContext(icEvent.inputContext());
    };

    eventWatchers_.emplace_back(instance_->watchEvent(
        EventType::InputContextFocusOut, EventWatcherPhase::PreInputMethod,
        resetWatcher));
    eventWatchers_.emplace_back(instance_->watchEvent(
        EventType::InputContextReset, EventWatcherPhase::PreInputMethod,
        resetWatcher));
    eventWatchers_.emplace_back(instance_->watchEvent(
        EventType::InputContextSwitchInputMethod,
        EventWatcherPhase::PreInputMethod, resetWatcher));
}

SmartKeyAddon::~SmartKeyAddon() {
    eventWatchers_.clear();
    if (factory_.registered()) {
        factory_.unregister();
    }
    if (engine_ != nullptr) {
        smartkey_engine_free(engine_);
        engine_ = nullptr;
    }
}

void SmartKeyAddon::reloadConfig() {
    if (engine_ == nullptr || instance_ == nullptr) {
        return;
    }
    smartkey_engine_reload_dicts(engine_);
    bool reloadedViaContext = false;
    instance_->inputContextManager().foreach([this, &reloadedViaContext](InputContext *ic) {
        resetContext(ic);
        reloadedViaContext = true;
        return true;
    });
    if (!reloadedViaContext) {
        if (auto *tmp = smartkey_session_new()) {
            smartkey_session_reset(tmp, engine_);
            smartkey_session_free(tmp);
        }
    }
}

void SmartKeyAddon::save() { reloadConfig(); }

void SmartKeyAddon::resetContext(InputContext *ic) {
    if (ic == nullptr || engine_ == nullptr || !factory_.registered()) {
        return;
    }
    auto *state = ic->propertyFor(&factory_);
    if (state != nullptr && state->session() != nullptr) {
        smartkey_session_reset(state->session(), engine_);
    }
}

bool SmartKeyAddon::isEnglishKeyboardActive(InputContext *ic) const {
    if (instance_ == nullptr || ic == nullptr) {
        return false;
    }
    const std::string im = instance_->inputMethod(ic);
    // Active ONLY on raw keyboard layouts (e.g. `keyboard-us`); steps aside completely
    // whenever `bamboo`, `lotus`, `unikey`, `mozc`, `rime`, etc. is active via `Alt+Z`.
    return im.starts_with("keyboard-");
}

uint8_t SmartKeyAddon::classifyProfile(InputContext *ic) {
    const auto flags = ic->capabilityFlags();
    if (flags.testAny(CapabilityFlags{
            CapabilityFlag::PasswordOrSensitive,
            CapabilityFlag::Terminal,
            CapabilityFlag::Url,
            CapabilityFlag::Email,
            CapabilityFlag::Digit,
            CapabilityFlag::Number,
            CapabilityFlag::Dialable,
            CapabilityFlag::Disable})) {
        return SMARTKEY_PROFILE_BYPASS;
    }

    const std::string_view rawProg = ic->program();
    if (rawProg.empty()) {
        // GTK4's `libim-fcitx5.so` (`frontend:dbus` when `GTK_IM_MODULE=fcitx` is exported)
        // reports an empty program name and implements DBus `ForwardKey` as a hard no-op (`ret`).
        // If `SurroundingText` is also invalid (e.g. a GTK4 terminal or raw surface), replacement
        // cannot delete the original word and would duplicate text (`orig + replacement`).
        if (ic->frontendName() == std::string_view("dbus") &&
            (!flags.test(CapabilityFlag::SurroundingText) ||
             !ic->surroundingText().isValid())) {
            return SMARTKEY_PROFILE_BYPASS;
        }
        return SMARTKEY_PROFILE_PROSE;
    }

    return classifyProgramName(rawProg);
}

void SmartKeyAddon::onKeyEvent(KeyEvent &keyEvent) {
    if (engine_ == nullptr || keyEvent.isRelease()) {
        return;
    }

    auto *ic = keyEvent.inputContext();
    if (ic == nullptr || !isEnglishKeyboardActive(ic)) {
        return;
    }

    if (instance_->isComposing(ic)) {
        return;
    }

    auto *state = ic->propertyFor(&factory_);
    if (state == nullptr || state->session() == nullptr) {
        return;
    }

    const uint8_t profile = classifyProfile(ic);
    smartkey_session_set_profile(state->session(), profile);
    if (profile == SMARTKEY_PROFILE_BYPASS) {
        return;
    }

    const Key rawKey = keyEvent.rawKey();
    const Key normKey = keyEvent.key();
    if (rawKey.isModifier() || normKey.isModifier()) {
        return;
    }

    const auto mods = rawKey.states();
    const bool hasCtrl = mods.test(KeyState::Ctrl);
    const bool hasAltOrSuper = mods.testAny(KeyStates{
        KeyState::Alt, KeyState::Super, KeyState::Super2,
        KeyState::Hyper, KeyState::Hyper2, KeyState::Meta});

    const uint64_t nowMs = monotonicNowMs();
    uint8_t keyKind = SMARTKEY_KEY_CHAR;
    uint32_t codepoint = 0;

    if (rawKey.sym() == FcitxKey_BackSpace && hasCtrl && !hasAltOrSuper) {
        keyKind = SMARTKEY_KEY_CTRL_BACKSPACE;
    } else if (hasCtrl || hasAltOrSuper) {
        // Any shortcut chord (`Ctrl+V`, `Ctrl+A`, `Ctrl+Z`, `Ctrl+Left`, `Alt+...`) resets
        // our shadow buffer cleanly and passes straight through to the application.
        SmartKeyAction unused{};
        smartkey_session_process_key(state->session(), engine_,
                                     SMARTKEY_KEY_NAV_OR_CLICK, 0, nowMs,
                                     &unused);
        return;
    } else if (rawKey.sym() == FcitxKey_BackSpace) {
        keyKind = SMARTKEY_KEY_BACKSPACE;
    } else if (rawKey.sym() == FcitxKey_Return ||
               rawKey.sym() == FcitxKey_KP_Enter) {
        keyKind = SMARTKEY_KEY_ENTER;
    } else if (rawKey.sym() == FcitxKey_Tab ||
               rawKey.sym() == FcitxKey_ISO_Left_Tab ||
               rawKey.sym() == FcitxKey_KP_Tab) {
        keyKind = SMARTKEY_KEY_TAB;
    } else if (rawKey.isCursorMove() ||
               rawKey.sym() == FcitxKey_Delete ||
               rawKey.sym() == FcitxKey_KP_Delete ||
               rawKey.sym() == FcitxKey_Insert ||
               rawKey.sym() == FcitxKey_Escape) {
        keyKind = SMARTKEY_KEY_NAV_OR_CLICK;
    } else {
        const uint32_t cp = Key::keySymToUnicode(rawKey.sym());
        if (cp < 0x20 || cp == 0x7F) {
            return;
        }
        keyKind = SMARTKEY_KEY_CHAR;
        codepoint = cp;
    }

    SmartKeyAction act{};
    if (!smartkey_session_process_key(state->session(), engine_, keyKind,
                                      codepoint, nowMs, &act)) {
        return;
    }

    if (act.kind != SMARTKEY_ACTION_REPLACE) {
        return;
    }

    keyEvent.filterAndAccept();

    if (act.delete_chars > 0) {
        if (ic->capabilityFlags().test(CapabilityFlag::SurroundingText) &&
            ic->surroundingText().isValid() &&
            ic->surroundingText().cursor() >= act.delete_chars) {
            ic->deleteSurroundingText(-static_cast<int>(act.delete_chars),
                                      act.delete_chars);
        } else {
            const Key bsKey(FcitxKey_BackSpace);
            for (uint32_t i = 0; i < act.delete_chars; ++i) {
                ic->forwardKey(bsKey, false);
                ic->forwardKey(bsKey, true);
            }
        }
    }

    if (act.insert_utf8 != nullptr && act.insert_byte_len > 0) {
        ic->commitString(std::string(act.insert_utf8, act.insert_byte_len));
    }
}

} // namespace fcitx::smartkey

FCITX_ADDON_FACTORY_V2_BACKWARDS(smartkey, fcitx::smartkey::SmartKeyAddonFactory)

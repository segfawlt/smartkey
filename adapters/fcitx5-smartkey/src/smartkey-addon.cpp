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

    eventWatchers_.emplace_back(instance_->watchEvent(
        EventType::InputContextKeyEvent, EventWatcherPhase::PostInputMethod,
        [this](Event &event) {
            onPostKeyEvent(static_cast<KeyEvent &>(event));
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
    if (state != nullptr) {
        state->resetLotusTracking();
        if (state->session() != nullptr) {
            smartkey_session_reset(state->session(), engine_);
        }
    }
}

bool SmartKeyAddon::isEnglishKeyboardActive(InputContext *ic) const {
    if (instance_ == nullptr || ic == nullptr) {
        return false;
    }
    const std::string im = instance_->inputMethod(ic);
    return im.starts_with("keyboard-");
}

bool SmartKeyAddon::isLotusActive(InputContext *ic) const {
    if (instance_ == nullptr || ic == nullptr || engine_ == nullptr) {
        return false;
    }
    if (!smartkey_engine_is_vi_lotus_enabled(engine_)) {
        return false;
    }
    return instance_->inputMethod(ic) == "lotus";
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

namespace {

bool decodeKeyEvent(const KeyEvent &keyEvent, bool &isShortcutReset,
                    uint8_t &keyKind, uint32_t &codepoint) {
    isShortcutReset = false;
    keyKind = SMARTKEY_KEY_CHAR;
    codepoint = 0;

    const Key rawKey = keyEvent.rawKey();
    const Key normKey = keyEvent.key();
    if (rawKey.isModifier() || normKey.isModifier()) {
        return false;
    }

    const auto mods = rawKey.states();
    const bool hasCtrl = mods.test(KeyState::Ctrl);
    const bool hasAltOrSuper = mods.testAny(KeyStates{
        KeyState::Alt, KeyState::Super, KeyState::Super2,
        KeyState::Hyper, KeyState::Hyper2, KeyState::Meta});

    if (rawKey.sym() == FcitxKey_BackSpace && hasCtrl && !hasAltOrSuper) {
        keyKind = SMARTKEY_KEY_CTRL_BACKSPACE;
        return true;
    }
    if (hasCtrl || hasAltOrSuper) {
        isShortcutReset = true;
        return true;
    }
    if (rawKey.sym() == FcitxKey_BackSpace) {
        keyKind = SMARTKEY_KEY_BACKSPACE;
        return true;
    }
    if (rawKey.sym() == FcitxKey_Return || rawKey.sym() == FcitxKey_KP_Enter) {
        keyKind = SMARTKEY_KEY_ENTER;
        return true;
    }
    if (rawKey.sym() == FcitxKey_Tab ||
        rawKey.sym() == FcitxKey_ISO_Left_Tab ||
        rawKey.sym() == FcitxKey_KP_Tab) {
        keyKind = SMARTKEY_KEY_TAB;
        return true;
    }
    if (rawKey.isCursorMove() || rawKey.sym() == FcitxKey_Delete ||
        rawKey.sym() == FcitxKey_KP_Delete ||
        rawKey.sym() == FcitxKey_Insert ||
        rawKey.sym() == FcitxKey_Escape) {
        keyKind = SMARTKEY_KEY_NAV_OR_CLICK;
        return true;
    }

    const uint32_t cp = Key::keySymToUnicode(rawKey.sym());
    if (cp < 0x20 || cp == 0x7F) {
        return false;
    }
    keyKind = SMARTKEY_KEY_CHAR;
    codepoint = cp;
    return true;
}

} // namespace

void SmartKeyAddon::applyReplaceAction(InputContext *ic, KeyEvent &keyEvent,
                                       const SmartKeyAction &act) {
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

void SmartKeyAddon::onKeyEvent(KeyEvent &keyEvent) {
    if (engine_ == nullptr || keyEvent.isRelease()) {
        return;
    }

    auto *ic = keyEvent.inputContext();
    if (ic == nullptr) {
        return;
    }

    const bool englishActive = isEnglishKeyboardActive(ic);
    const bool lotusActive = !englishActive && isLotusActive(ic);
    if (!englishActive && !lotusActive) {
        return;
    }

    auto *state = ic->propertyFor(&factory_);
    if (state == nullptr || state->session() == nullptr) {
        return;
    }

    const uint8_t profile = classifyProfile(ic);
    smartkey_session_set_profile(state->session(), profile);
    if (profile == SMARTKEY_PROFILE_BYPASS) {
        state->resetLotusTracking();
        return;
    }

    const uint64_t nowMs = monotonicNowMs();

    if (lotusActive) {
        smartkey_session_set_lang(state->session(), "vi");
        const Key rawKey = keyEvent.rawKey();

        // `fcitx5-lotus-server` terminates every uinput replacement burst with `FcitxKey_Pause`.
        if (rawKey.sym() == FcitxKey_Pause) {
            state->clearLotusBurst();
            return;
        }

        // Ignore synthetic `BackSpace` / `Left` / `Delete` keys injected by `fcitx5-lotus-server`
        // during a mid-word or break-time uinput replacement burst.
        if (state->isInLotusBurst(nowMs) &&
            (rawKey.sym() == FcitxKey_BackSpace ||
             rawKey.sym() == FcitxKey_Left ||
             rawKey.sym() == FcitxKey_Delete)) {
            state->armLotusBurst(nowMs);
            return;
        }

        // If Lotus filtered the previous word-break key (`Space`/punct) in `InputMethod` phase
        // (so `onPostKeyEvent` was skipped), reset our composing state to stay in sync.
        if (state->takeLotusFilteredBoundary()) {
            SmartKeyAction unused{};
            smartkey_session_process_key(state->session(), engine_,
                                         SMARTKEY_KEY_NAV_OR_CLICK, 0, nowMs,
                                         &unused);
        }

        bool isShortcutReset = false;
        uint8_t keyKind = SMARTKEY_KEY_CHAR;
        uint32_t codepoint = 0;
        if (!decodeKeyEvent(keyEvent, isShortcutReset, keyKind, codepoint)) {
            return;
        }

        if (isShortcutReset) {
            state->resetLotusTracking();
            SmartKeyAction unused{};
            smartkey_session_process_key(state->session(), engine_,
                                         SMARTKEY_KEY_NAV_OR_CLICK, 0, nowMs,
                                         &unused);
            return;
        }

        // Record non-boundary letters (`a..z`, etc.) passively in `PreInputMethod` so SmartKey
        // sees every raw keystroke even when `LotusState::keyEvent` filters the key to run a
        // uinput burst. Never call `keyEvent.filterAndAccept()` here so Lotus is never starved.
        if (keyKind == SMARTKEY_KEY_CHAR && !isSmartKeyBoundaryChar(codepoint)) {
            state->armLotusBurst(nowMs);
            SmartKeyAction unused{};
            smartkey_session_process_key(state->session(), engine_,
                                         SMARTKEY_KEY_CHAR, codepoint, nowMs,
                                         &unused);
            return;
        }

        // For word-break/lookahead chars (`Space`, `,`, `.`, `!`, `?`, etc.), `BackSpace`,
        // `Enter`, `Tab`, and cursor nav: defer to `onPostKeyEvent` (`PostInputMethod`) so
        // `LotusState::keyEvent` runs first and clears `oldPreBuffer_`.
        state->armLotusBurst(nowMs);
        state->setLotusFilteredBoundary(true);
        return;
    }

    // `englishActive` (`keyboard-*`):
    if (instance_->isComposing(ic)) {
        return;
    }
    state->resetLotusTracking();
    smartkey_session_set_lang(state->session(), "en");

    bool isShortcutReset = false;
    uint8_t keyKind = SMARTKEY_KEY_CHAR;
    uint32_t codepoint = 0;
    if (!decodeKeyEvent(keyEvent, isShortcutReset, keyKind, codepoint)) {
        return;
    }

    if (isShortcutReset) {
        SmartKeyAction unused{};
        smartkey_session_process_key(state->session(), engine_,
                                     SMARTKEY_KEY_NAV_OR_CLICK, 0, nowMs,
                                     &unused);
        return;
    }

    SmartKeyAction act{};
    if (!smartkey_session_process_key(state->session(), engine_, keyKind,
                                      codepoint, nowMs, &act)) {
        return;
    }

    if (act.kind == SMARTKEY_ACTION_REPLACE) {
        applyReplaceAction(ic, keyEvent, act);
    }
}

void SmartKeyAddon::onPostKeyEvent(KeyEvent &keyEvent) {
    if (engine_ == nullptr || keyEvent.isRelease()) {
        return;
    }

    auto *ic = keyEvent.inputContext();
    if (ic == nullptr || !isLotusActive(ic)) {
        return;
    }

    auto *state = ic->propertyFor(&factory_);
    if (state == nullptr || state->session() == nullptr) {
        return;
    }

    // Reaching `PostInputMethod` proves `LotusState::keyEvent` did NOT filter this key event!
    state->clearLotusBurst();
    state->setLotusFilteredBoundary(false);

    const uint8_t profile = classifyProfile(ic);
    smartkey_session_set_profile(state->session(), profile);
    if (profile == SMARTKEY_PROFILE_BYPASS) {
        return;
    }

    smartkey_session_set_lang(state->session(), "vi");

    bool isShortcutReset = false;
    uint8_t keyKind = SMARTKEY_KEY_CHAR;
    uint32_t codepoint = 0;
    if (!decodeKeyEvent(keyEvent, isShortcutReset, keyKind, codepoint)) {
        return;
    }

    // Shortcut resets and non-boundary letters were already recorded in `onKeyEvent` (`PreInputMethod`).
    if (isShortcutReset ||
        (keyKind == SMARTKEY_KEY_CHAR && !isSmartKeyBoundaryChar(codepoint))) {
        return;
    }

    const uint64_t nowMs = monotonicNowMs();
    SmartKeyAction act{};
    if (!smartkey_session_process_key(state->session(), engine_, keyKind,
                                      codepoint, nowMs, &act)) {
        return;
    }

    if (act.kind == SMARTKEY_ACTION_REPLACE) {
        applyReplaceAction(ic, keyEvent, act);
    }
}

} // namespace fcitx::smartkey

FCITX_ADDON_FACTORY_V2_BACKWARDS(smartkey, fcitx::smartkey::SmartKeyAddonFactory)

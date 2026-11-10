/*
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#include "smartkey-addon.h"

#include <algorithm>
#include <cctype>
#include <chrono>
#include <cstdlib>
#include <filesystem>
#include <pwd.h>
#include <string>
#include <string_view>
#include <unistd.h>
#include <vector>

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

std::filesystem::path resolveHomeDir() {
    if (const char *home = std::getenv("HOME");
        home != nullptr && home[0] != '\0') {
        return std::filesystem::path(home);
    }
    const uid_t uid = ::getuid();
    long bufSize = ::sysconf(_SC_GETPW_R_SIZE_MAX);
    if (bufSize <= 0) {
        bufSize = 4096;
    }
    std::vector<char> buf(static_cast<std::size_t>(bufSize));
    struct passwd pwd{};
    struct passwd *result = nullptr;
    if (::getpwuid_r(uid, &pwd, buf.data(), buf.size(), &result) == 0 &&
        result != nullptr && pwd.pw_dir != nullptr && pwd.pw_dir[0] != '\0') {
        return std::filesystem::path(pwd.pw_dir);
    }
    return std::filesystem::temp_directory_path() /
           ("smartkey-uid-" + std::to_string(uid));
}

std::filesystem::path resolveConfigDir() {
    if (const char *xdg = std::getenv("XDG_CONFIG_HOME");
        xdg != nullptr && xdg[0] != '\0') {
        return std::filesystem::path(xdg) / "smartkey";
    }
    return resolveHomeDir() / ".config" / "smartkey";
}

std::filesystem::path resolveStateLogDir() {
    if (const char *xdg = std::getenv("XDG_STATE_HOME");
        xdg != nullptr && xdg[0] != '\0') {
        return std::filesystem::path(xdg) / "smartkey" / "logs";
    }
    return resolveHomeDir() / ".local" / "state" / "smartkey" / "logs";
}

struct ScopedSelfCommit {
    SmartKeyState *state;
    explicit ScopedSelfCommit(SmartKeyState *s) : state(s) {
        if (state != nullptr) {
            state->setSelfCommitting(true);
        }
    }
    ~ScopedSelfCommit() {
        if (state != nullptr) {
            state->setSelfCommitting(false);
        }
    }
    ScopedSelfCommit(const ScopedSelfCommit &) = delete;
    ScopedSelfCommit &operator=(const ScopedSelfCommit &) = delete;
};

} // namespace

SmartKeyAddon::SmartKeyAddon(Instance *instance)
    : instance_(instance),
      factory_([](InputContext &) { return new SmartKeyState(); }) {
    const auto cfgDir = resolveConfigDir();
    const auto rulesPath = (cfgDir / "rules.toml").string();
    const auto statePath = (cfgDir / "learner_state.json").string();
    const auto logDir = resolveStateLogDir().string();

    engine_ = smartkey_engine_new(rulesPath.c_str(), statePath.c_str(),
                                  SMARTKEY_PRESET_BALANCED);
    if (engine_ == nullptr) {
        FCITX_ERROR() << "SmartKey failed to initialize engine at " << rulesPath;
        return;
    }
    smartkey_engine_set_log_dir(engine_, logDir.c_str());

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

    eventWatchers_.emplace_back(instance_->watchEvent(
        EventType::InputContextCommitString, EventWatcherPhase::PostInputMethod,
        [this](Event &event) {
            onCommitString(static_cast<CommitStringEvent &>(event));
        }));

    eventWatchers_.emplace_back(instance_->watchEvent(
        EventType::InputContextUpdatePreedit, EventWatcherPhase::PostInputMethod,
        [this](Event &event) { onUpdatePreedit(event); }));

    eventWatchers_.emplace_back(instance_->watchEvent(
        EventType::InputContextSurroundingTextUpdated,
        EventWatcherPhase::PostInputMethod,
        [this](Event &event) { onSurroundingTextUpdated(event); }));

    eventWatchers_.emplace_back(instance_->watchEvent(
        EventType::InputContextFocusOut, EventWatcherPhase::PreInputMethod,
        [this](Event &event) {
            auto &icEvent = static_cast<InputContextEvent &>(event);
            focusOutContext(icEvent.inputContext());
        }));

    auto resetWatcher = [this](Event &event) {
        auto &icEvent = static_cast<InputContextEvent &>(event);
        resetContext(icEvent.inputContext());
    };

    eventWatchers_.emplace_back(instance_->watchEvent(
        EventType::InputContextDestroyed, EventWatcherPhase::PreInputMethod,
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

void SmartKeyAddon::focusOutContext(InputContext *ic) {
    if (ic == nullptr || engine_ == nullptr || !factory_.registered()) {
        return;
    }
    auto *state = ic->propertyFor(&factory_);
    if (state != nullptr) {
        state->resetLotusTracking();
        if (state->session() != nullptr) {
            SmartKeyAction unused{};
            smartkey_session_process_key(state->session(), engine_,
                                         SMARTKEY_KEY_FOCUS_LOST, 0,
                                         monotonicNowMs(), &unused);
        }
    }
}

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

uint8_t SmartKeyAddon::classifyProfile(InputContext *ic,
                                       std::string_view appName,
                                       const char **outReason) const {
    auto setReason = [&](const char *r) {
        if (outReason != nullptr) {
            *outReason = r;
        }
    };
    const auto flags = ic->capabilityFlags();
    if (flags.test(CapabilityFlag::PasswordOrSensitive)) {
        setReason("capability:password");
        return SMARTKEY_PROFILE_BYPASS;
    }
    if (flags.test(CapabilityFlag::Terminal)) {
        setReason("capability:terminal");
        return SMARTKEY_PROFILE_BYPASS;
    }
    if (flags.test(CapabilityFlag::Url)) {
        setReason("capability:url");
        return SMARTKEY_PROFILE_BYPASS;
    }
    if (flags.test(CapabilityFlag::Email)) {
        setReason("capability:email");
        return SMARTKEY_PROFILE_BYPASS;
    }
    if (flags.testAny(CapabilityFlags{
            CapabilityFlag::Digit, CapabilityFlag::Number,
            CapabilityFlag::Dialable, CapabilityFlag::Disable})) {
        setReason("capability:numeric_or_disabled");
        return SMARTKEY_PROFILE_BYPASS;
    }

    if (engine_ != nullptr) {
        const std::string appStr(appName);
        const uint8_t userOverride =
            smartkey_engine_classify_app_override(engine_, appStr.c_str());
        if (userOverride != SMARTKEY_PROFILE_NONE) {
            setReason(appName.empty() ? "unknown-app-policy" : "user-app-override");
            return userOverride;
        }
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
            setReason("dbus-no-surrounding-text");
            return SMARTKEY_PROFILE_BYPASS;
        }
        setReason("prose");
        return SMARTKEY_PROFILE_PROSE;
    }

    const uint8_t prof = classifyProgramName(rawProg);
    switch (prof) {
    case SMARTKEY_PROFILE_BYPASS:
        setReason("program-list");
        break;
    case SMARTKEY_PROFILE_BROWSER:
        setReason("browser");
        break;
    case SMARTKEY_PROFILE_CHAT:
        setReason("chat");
        break;
    default:
        setReason("prose");
        break;
    }
    return prof;
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
    if (rawKey.sym() == FcitxKey_KP_Space) {
        keyKind = SMARTKEY_KEY_CHAR;
        codepoint = ' ';
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

void SmartKeyAddon::applyDeleteAndCommit(InputContext *ic, SmartKeyState *state,
                                         const SmartKeyAction &act) {
    // Note: `fcitx::SurroundingText::cursor()` and `InputContext::deleteSurroundingText`
    // both operate in Unicode character counts (`surroundingtext.h:40`), matching `act.delete_chars`.
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
        ScopedSelfCommit guard(state);
        ic->commitString(std::string(act.insert_utf8, act.insert_byte_len));
    }
}

void SmartKeyAddon::applyReplaceAction(InputContext *ic, SmartKeyState *state,
                                       KeyEvent &keyEvent,
                                       const SmartKeyAction &act) {
    keyEvent.filterAndAccept();
    applyDeleteAndCommit(ic, state, act);
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
    state->setSuppressPostKey(false);

    const std::string appName = normalizeProgramId(ic->program());
    smartkey_session_set_app(state->session(), appName.c_str());

    const char *reason = "prose";
    const uint8_t profile = classifyProfile(ic, appName, &reason);
    smartkey_session_set_profile(state->session(), profile);
    if (state->shouldLogProfileTransition(profile, reason) &&
        profile == SMARTKEY_PROFILE_BYPASS) {
        smartkey_engine_log_app_event(engine_, appName.c_str(), profile, reason);
    }
    if (profile == SMARTKEY_PROFILE_BYPASS) {
        state->resetLotusTracking();
        return;
    }

    const uint64_t nowMs = monotonicNowMs();

    if (lotusActive) {
        smartkey_session_set_lang(state->session(), "vi");
        const Key rawKey = keyEvent.rawKey();

        // `fcitx5-lotus-server` terminates every uinput replacement burst with `FcitxKey_Pause`.
        // Keep `pendingBoundary` intact across `Pause` so `onCommitString` (fired inside
        // `LotusState::finishReplacement`) can evaluate the completed word immediately.
        if (rawKey.sym() == FcitxKey_Pause) {
            state->markLotusActivity();
            state->clearLotusBurst();
            state->setSuppressPostKey(true);
            return;
        }

        // Ignore synthetic `BackSpace` / `Left` / `Delete` echoes injected by `fcitx5-lotus-server`
        // during a mid-word or break-time uinput replacement burst. Note that `LotusState` forwards
        // intermediate echo BackSpaces unfiltered so they reach the client app, which means
        // `PostInputMethod` WILL fire for them — `setSuppressPostKey(true)` blocks `onPostKeyEvent`
        // from mistaking synthetic echoes for user BackSpaces!
        if (state->isInLotusBurst(nowMs) &&
            (rawKey.sym() == FcitxKey_BackSpace ||
             rawKey.sym() == FcitxKey_Left ||
             rawKey.sym() == FcitxKey_Delete)) {
            state->markLotusActivity();
            state->rearmLotusBurstEcho(nowMs);
            state->setSuppressPostKey(true);
            return;
        }

        // If a previous boundary key was filtered by Lotus without completing via `onCommitString`
        // (e.g. user typed fast into `LotusState::bufferedKeys_` mid-burst, or Lotus consumed the
        // key in Emoji popup mode), flush stale composing state before recording the new key.
        if (state->hasPendingBoundary()) {
            state->clearPendingBoundary();
            state->takeLotusActivityInWord();
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
        // uinput burst. Only arm the burst window on keys capable of triggering a Lotus burst.
        if (keyKind == SMARTKEY_KEY_CHAR && !isSmartKeyBoundaryChar(codepoint)) {
            if (isTelexBurstTriggerChar(codepoint)) {
                state->armLotusBurst(nowMs);
            }
            SmartKeyAction unused{};
            smartkey_session_process_key(state->session(), engine_,
                                         SMARTKEY_KEY_CHAR, codepoint, nowMs,
                                         &unused);
            return;
        }

        // For word-break/lookahead chars (`Space`, `,`, `.`, `!`, `?`, etc.), `BackSpace`,
        // `Enter`, `Tab`, and cursor nav: defer to `onPostKeyEvent` (if Lotus forwards) or
        // `onCommitString` (if Lotus filters the boundary to restore/commit).
        if (keyKind == SMARTKEY_KEY_CHAR && isSmartKeyBoundaryChar(codepoint)) {
            state->armLotusBurst(nowMs);
            state->setPendingBoundary(codepoint);
        } else {
            state->clearPendingBoundary();
        }
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
        applyReplaceAction(ic, state, keyEvent, act);
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

    if (state->takeSuppressPostKey()) {
        return;
    }

    const std::string appName = normalizeProgramId(ic->program());
    smartkey_session_set_app(state->session(), appName.c_str());

    const char *reason = "prose";
    const uint8_t profile = classifyProfile(ic, appName, &reason);
    smartkey_session_set_profile(state->session(), profile);
    if (state->shouldLogProfileTransition(profile, reason) &&
        profile == SMARTKEY_PROFILE_BYPASS) {
        smartkey_engine_log_app_event(engine_, appName.c_str(), profile, reason);
    }
    if (profile == SMARTKEY_PROFILE_BYPASS) {
        state->resetLotusTracking();
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

    // Reaching `PostInputMethod` on a boundary/edit/nav key proves `LotusState::keyEvent`
    // forwarded this key without filtering it.
    state->clearLotusBurst();
    state->clearPendingBoundary();
    const bool lotusMutatedWord = state->takeLotusActivityInWord();
    smartkey_session_set_lotus_composing(state->session(), lotusMutatedWord);

    const uint64_t nowMs = monotonicNowMs();
    SmartKeyAction act{};
    if (!smartkey_session_process_key(state->session(), engine_, keyKind,
                                      codepoint, nowMs, &act)) {
        return;
    }

    if (act.kind == SMARTKEY_ACTION_REPLACE) {
        applyReplaceAction(ic, state, keyEvent, act);
    }
}

void SmartKeyAddon::onCommitString(CommitStringEvent &commitEvent) {
    if (engine_ == nullptr) {
        return;
    }
    auto *ic = commitEvent.inputContext();
    if (ic == nullptr || !isLotusActive(ic)) {
        return;
    }
    auto *state = ic->propertyFor(&factory_);
    if (state == nullptr || state->session() == nullptr ||
        state->isSelfCommitting()) {
        return;
    }

    state->markLotusActivity();
    if (!state->hasPendingBoundary()) {
        return;
    }

    const std::string appName = normalizeProgramId(ic->program());
    smartkey_session_set_app(state->session(), appName.c_str());
    const uint8_t profile = classifyProfile(ic, appName, nullptr);
    smartkey_session_set_profile(state->session(), profile);
    if (profile == SMARTKEY_PROFILE_BYPASS) {
        state->resetLotusTracking();
        return;
    }

    smartkey_session_set_lang(state->session(), "vi");
    const uint32_t boundaryCp = state->takePendingBoundary();
    state->takeLotusActivityInWord();
    state->clearLotusBurst();

    const uint64_t nowMs = monotonicNowMs();
    SmartKeyAction act{};
    if (!smartkey_session_on_lotus_commit(
            state->session(), engine_, boundaryCp, commitEvent.text().c_str(),
            nowMs, &act)) {
        return;
    }

    if (act.kind == SMARTKEY_ACTION_REPLACE) {
        applyDeleteAndCommit(ic, state, act);
    }
}

void SmartKeyAddon::onUpdatePreedit(Event &event) {
    if (engine_ == nullptr) {
        return;
    }
    auto &icEvent = static_cast<InputContextEvent &>(event);
    auto *ic = icEvent.inputContext();
    if (ic == nullptr || !isLotusActive(ic)) {
        return;
    }
    if (auto *state = ic->propertyFor(&factory_)) {
        state->markLotusActivity();
    }
}

void SmartKeyAddon::onSurroundingTextUpdated(Event &event) {
    if (engine_ == nullptr) {
        return;
    }
    auto &icEvent = static_cast<InputContextEvent &>(event);
    auto *ic = icEvent.inputContext();
    if (ic == nullptr || !isEnglishKeyboardActive(ic)) {
        return;
    }
    if (!ic->capabilityFlags().test(CapabilityFlag::SurroundingText) ||
        !ic->surroundingText().isValid()) {
        return;
    }
    auto *state = ic->propertyFor(&factory_);
    if (state == nullptr || state->session() == nullptr) {
        return;
    }
    const auto &st = ic->surroundingText();
    smartkey_session_reconcile_surrounding(state->session(), engine_,
                                           st.text().c_str(), st.cursor(),
                                           monotonicNowMs());
}

} // namespace fcitx::smartkey

FCITX_ADDON_FACTORY_V2_BACKWARDS(smartkey, fcitx::smartkey::SmartKeyAddonFactory)

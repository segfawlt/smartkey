/*
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#include "smartkey-addon.h"

#include <algorithm>
#include <cctype>
#include <chrono>
#include <cstdlib>
#include <ctime>
#include <filesystem>
#include <pwd.h>
#include <string>
#include <string_view>
#include <unistd.h>
#include <utility>
#include <vector>

#include <fcitx-utils/capabilityflags.h>
#include <fcitx-utils/key.h>
#include <fcitx-utils/keysym.h>
#include <fcitx-utils/log.h>
#include <fcitx/inputcontextmanager.h>
#include <fcitx/inputmethodentry.h>

namespace fcitx::smartkey {
namespace {

uint64_t monotonicNowMs() {
    using namespace std::chrono;
    return static_cast<uint64_t>(
        duration_cast<milliseconds>(steady_clock::now().time_since_epoch())
            .count());
}

uint64_t epochNowMs() {
    using namespace std::chrono;
    return static_cast<uint64_t>(
        duration_cast<milliseconds>(system_clock::now().time_since_epoch())
            .count());
}

std::string encodeUtf8(uint32_t cp) {
    std::string out;
    if (cp <= 0x7F) {
        out.push_back(static_cast<char>(cp));
    } else if (cp <= 0x7FF) {
        out.push_back(static_cast<char>(0xC0 | ((cp >> 6) & 0x1F)));
        out.push_back(static_cast<char>(0x80 | (cp & 0x3F)));
    } else if (cp <= 0xFFFF) {
        out.push_back(static_cast<char>(0xE0 | ((cp >> 12) & 0x0F)));
        out.push_back(static_cast<char>(0x80 | ((cp >> 6) & 0x3F)));
        out.push_back(static_cast<char>(0x80 | (cp & 0x3F)));
    } else if (cp <= 0x10FFFF) {
        out.push_back(static_cast<char>(0xF0 | ((cp >> 18) & 0x07)));
        out.push_back(static_cast<char>(0x80 | ((cp >> 12) & 0x3F)));
        out.push_back(static_cast<char>(0x80 | ((cp >> 6) & 0x3F)));
        out.push_back(static_cast<char>(0x80 | (cp & 0x3F)));
    }
    return out;
}

std::string snapshotSessionCompact(SmartKeySessionHandle *session) {
    if (session == nullptr) {
        return {};
    }
    SmartKeySessionSnapshot snap{};
    if (!smartkey_session_debug_snapshot(session, &snap)) {
        return {};
    }
    return formatSessionCompact(snap);
}

struct TraceEventFields {
    std::string_view ev;
    std::string_view out;
    bool hasKey = false;
    Key rawKey{};
    std::string ch;
    bool hasDel = false;
    uint32_t del = 0;
    bool hasTxt = false;
    std::string txt;
    std::string s0;
    std::string s1;
    bool hasScr = false;
    std::string scr;
};

void emitTraceRecord(TraceLogger &trace, InputContext *ic, SmartKeyState *state,
                     std::string_view appName, std::string_view imName,
                     uint64_t monoMs, const TraceEventFields &f) {
    if (ic == nullptr || state == nullptr) {
        return;
    }
    const uint64_t tsMs = epochNowMs();
    const uint64_t dtMs = trace.nextDeltaMs(monoMs);

    std::string &line = trace.beginLine();
    line.append("{\"v\":1,\"ts_ms\":");
    line.append(std::to_string(tsMs));
    line.append(",\"mono_ms\":");
    line.append(std::to_string(monoMs));
    line.append(",\"dt_ms\":");
    line.append(std::to_string(dtMs));
    line.append(",\"ic\":");
    line.append(std::to_string(state->icId()));
    line.append(",\"app\":");
    appendJsonString(line, appName);
    line.append(",\"fe\":");
    appendJsonString(line, ic->frontendName());
    line.append(",\"im\":");
    appendJsonString(line, imName);
    line.append(",\"ev\":");
    appendJsonString(line, f.ev);
    line.append(",\"out\":");
    appendJsonString(line, f.out);

    if (f.hasKey) {
        const std::string symStr = Key::keySymToString(f.rawKey.sym());
        if (!symStr.empty()) {
            line.append(",\"sym\":");
            appendJsonString(line, symStr);
        }
        line.append(",\"code\":");
        line.append(std::to_string(static_cast<uint32_t>(f.rawKey.code())));
        const uint32_t mods = f.rawKey.states().toInteger();
        if (mods != 0) {
            line.append(",\"mods\":");
            line.append(std::to_string(mods));
        }
        if (!f.ch.empty()) {
            line.append(",\"ch\":");
            appendJsonString(line, f.ch);
        }
    }

    if (f.hasDel) {
        line.append(",\"del\":");
        line.append(std::to_string(f.del));
    }
    if (f.hasTxt) {
        line.append(",\"txt\":");
        appendJsonString(line, f.txt);
    }

    line.append(",\"s0\":");
    appendJsonString(line, f.s0);
    if (!f.s1.empty() && f.s1 != f.s0) {
        line.append(",\"s1\":");
        appendJsonString(line, f.s1);
    }

    line.append(",\"burst\":");
    line.push_back(state->isInLotusBurst(monoMs) ? '1' : '0');
    line.append(",\"act\":");
    line.push_back(state->hasLotusActivityInWord() ? '1' : '0');
    if (state->hasPendingBoundary()) {
        line.append(",\"pend\":");
        appendJsonString(line, encodeUtf8(state->pendingBoundaryCp()));
    }
    if (const std::size_t qLen = state->postBoundaryQueueLen(); qLen > 0) {
        line.append(",\"q\":");
        line.append(std::to_string(qLen));
    }
    line.append(",\"sync\":");
    line.push_back(state->isSurroundingSynced() ? '1' : '0');

    if (f.hasScr) {
        line.append(",\"scr\":");
        appendJsonString(line, f.scr);
    } else if (ic->capabilityFlags().test(CapabilityFlag::SurroundingText) &&
               ic->surroundingText().isValid()) {
        const auto &st = ic->surroundingText();
        line.append(",\"scr\":");
        appendJsonString(
            line, extractSurroundingSlice(st.text(), st.cursor(), st.anchor()));
    }

    line.push_back('}');
    trace.commitLine(tsMs);
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
    trace_.setLogDir(resolveStateLogDir());

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

    eventWatchers_.emplace_back(instance_->watchEvent(
        EventType::InputContextDestroyed, EventWatcherPhase::PreInputMethod,
        [this](Event &event) {
            auto &icEvent = static_cast<InputContextEvent &>(event);
            if (auto *ic = icEvent.inputContext();
                ic != nullptr && factory_.registered()) {
                if (auto *state = ic->propertyFor(&factory_)) {
                    state->resetSurroundingTrust();
                }
            }
            resetContext(icEvent.inputContext());
        }));
    eventWatchers_.emplace_back(instance_->watchEvent(
        EventType::InputContextReset, EventWatcherPhase::PreInputMethod,
        [this](Event &event) {
            auto &icEvent = static_cast<InputContextEvent &>(event);
            onInputContextReset(icEvent.inputContext());
        }));
    eventWatchers_.emplace_back(instance_->watchEvent(
        EventType::InputContextSwitchInputMethod,
        EventWatcherPhase::PreInputMethod,
        [this](Event &event) {
            auto &icEvent = static_cast<InputContextEvent &>(event);
            onSwitchInputMethod(icEvent.inputContext());
        }));
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
    trace_.closeIfOpen();
    if (engine_ == nullptr || instance_ == nullptr) {
        return;
    }
    smartkey_engine_reload_dicts(engine_);
    bool reloadedViaContext = false;
    instance_->inputContextManager().foreach([this, &reloadedViaContext](InputContext *ic) {
        if (ic != nullptr && factory_.registered()) {
            if (auto *state = ic->propertyFor(&factory_)) {
                state->resetSurroundingTrust();
            }
        }
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

bool SmartKeyAddon::isTraceActive() {
    const bool active =
        engine_ != nullptr && smartkey_engine_is_trace_enabled(engine_);
    if (!active) {
        trace_.closeIfOpen();
    }
    return active;
}

bool SmartKeyAddon::canTraceContext(InputContext *ic,
                                    std::string_view appName) {
    return ic != nullptr && isTraceActive() &&
           classifyProfile(ic, appName, nullptr) != SMARTKEY_PROFILE_BYPASS;
}

void SmartKeyAddon::focusOutContext(InputContext *ic) {
    if (ic == nullptr || engine_ == nullptr || !factory_.registered()) {
        return;
    }
    auto *state = ic->propertyFor(&factory_);
    if (state != nullptr) {
        const uint64_t nowMs = monotonicNowMs();
        const bool traceEnabled = isTraceActive();
        const std::string appName =
            traceEnabled ? normalizeProgramId(ic->program()) : std::string{};
        const bool traceEligible =
            traceEnabled &&
            classifyProfile(ic, appName, nullptr) != SMARTKEY_PROFILE_BYPASS;
        const std::string s0 =
            traceEligible ? snapshotSessionCompact(state->session())
                          : std::string{};
        const bool shouldTrace =
            traceEligible &&
            (!resolveDirectKeyboardLang(ic).empty() || isLotusActive(ic) ||
             s0 != "Idle" || state->hasPendingBoundary() ||
             state->isInLotusBurst(nowMs));
        flushPendingDelayedCommit(ic, state);
        state->clearSelfReplaceGuard();
        state->resetLotusTracking();
        state->resetSurroundingTrust();
        if (state->session() != nullptr) {
            SmartKeyAction unused{};
            smartkey_session_process_key(state->session(), engine_,
                                         SMARTKEY_KEY_FOCUS_LOST, 0,
                                         nowMs, &unused);
        }
        if (shouldTrace) {
            TraceEventFields f{};
            f.ev = "focus_out";
            f.out = "executed";
            f.s0 = s0;
            f.s1 = snapshotSessionCompact(state->session());
            const std::string imName =
                instance_ != nullptr ? instance_->inputMethod(ic) : "";
            emitTraceRecord(trace_, ic, state, appName, imName, nowMs, f);
        }
    }
}

void SmartKeyAddon::onInputContextReset(InputContext *ic) {
    if (ic == nullptr || engine_ == nullptr || !factory_.registered()) {
        return;
    }
    auto *state = ic->propertyFor(&factory_);
    const uint64_t nowMs = monotonicNowMs();
    const bool traceEnabled = isTraceActive();
    const std::string appName =
        traceEnabled ? normalizeProgramId(ic->program()) : std::string{};
    const bool traceEligible =
        traceEnabled &&
        classifyProfile(ic, appName, nullptr) != SMARTKEY_PROFILE_BYPASS;
    const std::string s0 =
        (traceEligible && state != nullptr)
            ? snapshotSessionCompact(state->session())
            : std::string{};
    const bool shouldTrace =
        traceEligible && state != nullptr &&
        (!resolveDirectKeyboardLang(ic).empty() || isLotusActive(ic) ||
         s0 != "Idle" || state->hasPendingBoundary() ||
         state->isInLotusBurst(nowMs));

    if (state != nullptr && state->shouldIgnoreClientReset(nowMs)) {
        if (shouldTrace) {
            TraceEventFields f{};
            f.ev = "ic_reset";
            f.out = "ignored_self_guard";
            f.s0 = s0;
            const std::string imName =
                instance_ != nullptr ? instance_->inputMethod(ic) : "";
            emitTraceRecord(trace_, ic, state, appName, imName, nowMs, f);
        }
        return;
    }
    resetContext(ic);
    if (shouldTrace) {
        TraceEventFields f{};
        f.ev = "ic_reset";
        f.out = "executed";
        f.s0 = s0;
        f.s1 = snapshotSessionCompact(state->session());
        const std::string imName =
            instance_ != nullptr ? instance_->inputMethod(ic) : "";
        emitTraceRecord(trace_, ic, state, appName, imName, nowMs, f);
    }
}

void SmartKeyAddon::onSwitchInputMethod(InputContext *ic) {
    if (ic == nullptr || engine_ == nullptr || !factory_.registered()) {
        return;
    }
    auto *state = ic->propertyFor(&factory_);
    const uint64_t nowMs = monotonicNowMs();
    const bool traceEnabled = isTraceActive();
    const std::string appName =
        traceEnabled ? normalizeProgramId(ic->program()) : std::string{};
    const bool traceEligible =
        traceEnabled &&
        classifyProfile(ic, appName, nullptr) != SMARTKEY_PROFILE_BYPASS;
    const std::string s0 =
        (traceEligible && state != nullptr)
            ? snapshotSessionCompact(state->session())
            : std::string{};
    const bool shouldTrace =
        traceEligible && state != nullptr &&
        (!resolveDirectKeyboardLang(ic).empty() || isLotusActive(ic) ||
         s0 != "Idle" || state->hasPendingBoundary() ||
         state->isInLotusBurst(nowMs));
    const std::string imName =
        shouldTrace && instance_ != nullptr ? instance_->inputMethod(ic) : "";

    if (state != nullptr) {
        state->resetSurroundingTrust();
    }
    resetContext(ic);
    if (shouldTrace) {
        TraceEventFields f{};
        f.ev = "im_switch";
        f.out = "executed";
        f.s0 = s0;
        f.s1 = snapshotSessionCompact(state->session());
        emitTraceRecord(trace_, ic, state, appName, imName, nowMs, f);
    }
}

void SmartKeyAddon::resetContext(InputContext *ic) {
    if (ic == nullptr || engine_ == nullptr || !factory_.registered()) {
        return;
    }
    auto *state = ic->propertyFor(&factory_);
    if (state != nullptr) {
        state->cancelPendingDelayedCommit();
        state->clearSelfReplaceGuard();
        state->resetLotusTracking();
        state->resetSurroundingSync();
        if (state->session() != nullptr) {
            smartkey_session_reset(state->session(), engine_);
        }
    }
}

std::string SmartKeyAddon::resolveDirectKeyboardLang(InputContext *ic) const {
    if (instance_ == nullptr || ic == nullptr) {
        return {};
    }
    const std::string im = instance_->inputMethod(ic);
    if (const auto *entry = instance_->inputMethodEntry(ic)) {
        return normalizeKeyboardLangCode(im, entry->languageCode());
    }
    if (im == "keyboard-us" || im.starts_with("keyboard-us-") ||
        im == "keyboard-gb" || im.starts_with("keyboard-gb-")) {
        return "en";
    }
    return {};
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

    // GTK4's `libim-fcitx5.so` (`frontend:dbus`, omitting `CapabilityFlag::KeyEventOrderFix`)
    // and SDL DBus frontends implement `ForwardKey` as a no-op. If `SurroundingText` is also
    // unavailable, invalid, or flagged untrusted for this context, replacement cannot delete
    // the original word and would duplicate text (`orig + replacement`).
    // Run this guard before `unknown_app_policy` so `unknown_app_policy = "prose"` cannot
    // expose unsafe DBus surfaces without a working delete path.
    const SmartKeyState *state =
        factory_.registered() ? ic->propertyFor(&factory_) : nullptr;
    const bool canSurround =
        flags.test(CapabilityFlag::SurroundingText) &&
        ic->surroundingText().isValid() &&
        (state == nullptr || !state->isSurroundingUntrusted());
    if (!canForwardBackspace(ic->frontendName(), flags) && !canSurround) {
        setReason("dbus-no-surrounding-text");
        return SMARTKEY_PROFILE_BYPASS;
    }

    const std::string_view rawProg = ic->program();

    if (engine_ != nullptr) {
        const std::string appStr(appName);
        const char *ffiReason = "prose";
        const uint8_t prof =
            smartkey_engine_classify_app(engine_, appStr.c_str(), &ffiReason);
        setReason(ffiReason != nullptr ? ffiReason : "prose");
        return prof;
    }

    if (rawProg.empty()) {
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

void replayPostBoundaryChars(SmartKeyState *state, SmartKeyEngineHandle *engine,
                             const std::vector<uint32_t> &queuedChars,
                             uint64_t nowMs) {
    SmartKeyAction unused{};
    for (uint32_t cp : queuedChars) {
        if (isTelexBurstTriggerChar(cp) || state->hasLotusActivityInWord()) {
            state->armLotusBurst(nowMs);
        }
        smartkey_session_process_key(state->session(), engine,
                                     SMARTKEY_KEY_CHAR, cp, nowMs, &unused);
    }
}

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

void SmartKeyAddon::flushPendingDelayedCommit(InputContext *ic,
                                              SmartKeyState *state) {
    if (ic == nullptr || state == nullptr ||
        !state->hasPendingDelayedCommit()) {
        return;
    }
    const std::string text = state->takePendingDelayedCommit();
    if (!text.empty()) {
        const uint64_t nowMs = monotonicNowMs();
        state->markSelfReplace(nowMs);
        ScopedSelfCommit guard(state);
        state->markClientMutated();
        ic->commitString(text);
        if (isTraceActive()) {
            const std::string appName = normalizeProgramId(ic->program());
            if (classifyProfile(ic, appName, nullptr) !=
                SMARTKEY_PROFILE_BYPASS) {
                TraceEventFields f{};
                f.ev = "delayed_flush";
                f.out = "committed";
                f.hasTxt = true;
                f.txt = text;
                f.s0 = snapshotSessionCompact(state->session());
                const std::string imName =
                    instance_ != nullptr ? instance_->inputMethod(ic) : "";
                emitTraceRecord(trace_, ic, state, appName, imName, nowMs, f);
            }
        }
    }
}

void SmartKeyAddon::disarmSessionJustAutocorrected(SmartKeyState *state,
                                                   uint64_t nowMs) {
    if (state == nullptr || state->session() == nullptr || engine_ == nullptr) {
        return;
    }
    SmartKeySessionSnapshot snap{};
    if (!smartkey_session_debug_snapshot(state->session(), &snap)) {
        return;
    }
    if (snap.mode_utf8 != nullptr &&
        std::string_view(snap.mode_utf8) == "JustAutocorrected") {
        SmartKeyAction unused{};
        smartkey_session_process_key(state->session(), engine_,
                                     SMARTKEY_KEY_NAV_OR_CLICK, 0, nowMs,
                                     &unused);
    }
}

void SmartKeyAddon::applyDeleteAndCommit(InputContext *ic, SmartKeyState *state,
                                         const SmartKeyAction &act) {
    flushPendingDelayedCommit(ic, state);

    const std::string insertStr =
        (act.insert_utf8 != nullptr && act.insert_byte_len > 0)
            ? std::string(act.insert_utf8, act.insert_byte_len)
            : std::string{};
    std::string_view replaceOut;
    auto emitReplace = [&](std::string_view out) {
        if (!isTraceActive()) {
            return;
        }
        const std::string appName = normalizeProgramId(ic->program());
        if (classifyProfile(ic, appName, nullptr) == SMARTKEY_PROFILE_BYPASS) {
            return;
        }
        TraceEventFields f{};
        f.ev = "replace";
        f.out = out;
        f.hasDel = true;
        f.del = act.delete_chars;
        if (!insertStr.empty()) {
            f.hasTxt = true;
            f.txt = insertStr;
        }
        f.s0 = snapshotSessionCompact(state->session());
        const std::string imName =
            instance_ != nullptr ? instance_->inputMethod(ic) : "";
        emitTraceRecord(trace_, ic, state, appName, imName, monotonicNowMs(),
                        f);
    };

    // Note: `fcitx::SurroundingText::cursor()` and `InputContext::deleteSurroundingText`
    // both operate in Unicode character counts (`surroundingtext.h:40`), matching `act.delete_chars`.
    // On Wayland (`wayland` / `wayland_v2`), only call `deleteSurroundingText` when
    // `surroundingSynced` is true; otherwise fall back to `forwardKey(BackSpace)` and delay
    // `commitString` by 15ms so `zwp_virtual_keyboard_v1` BackSpaces arrive in Gecko/Zen
    // before `zwp_input_method_v2.commit_string`.
    if (act.delete_chars > 0) {
        const bool fallbackAvailable =
            canForwardBackspace(ic->frontendName(), ic->capabilityFlags());
        state->checkUnansweredBeforeDelete(fallbackAvailable);
        state->markSelfReplace(monotonicNowMs());
        if (state->canUseDeleteSurrounding(
                ic->capabilityFlags().test(CapabilityFlag::SurroundingText),
                ic->surroundingText().isValid(),
                ic->surroundingText().cursor(), act.delete_chars,
                ic->frontendName())) {
            replaceOut = "delete_surround";
            const std::string preTextCopy = ic->surroundingText().text();
            const uint32_t preCursor = ic->surroundingText().cursor();
            ic->deleteSurroundingText(-static_cast<int>(act.delete_chars),
                                      act.delete_chars);
            state->markClientMutated();
            state->armSurroundingDeleteCheck(preTextCopy, preCursor,
                                             act.delete_chars, insertStr);
            if (!insertStr.empty()) {
                ScopedSelfCommit guard(state);
                ic->commitString(insertStr);
            }
        } else if (fallbackAvailable) {
            const Key bsKey(FcitxKey_BackSpace);
            for (uint32_t i = 0; i < act.delete_chars; ++i) {
                ic->forwardKey(bsKey, false);
                ic->forwardKey(bsKey, true);
            }
            state->markClientMutated();
            if (!insertStr.empty()) {
                if (isWaylandFrontend(ic->frontendName()) &&
                    instance_ != nullptr) {
                    constexpr uint64_t kWaylandPostBackspaceDelayUsec = 15000;
                    auto timer = instance_->eventLoop().addTimeEvent(
                        CLOCK_MONOTONIC,
                        fcitx::now(CLOCK_MONOTONIC) +
                            kWaylandPostBackspaceDelayUsec,
                        0,
                        [this, icRef = ic->watch()](EventSourceTime *,
                                                    uint64_t) {
                            if (auto *liveIc = icRef.get()) {
                                if (factory_.registered()) {
                                    if (auto *st =
                                            liveIc->propertyFor(&factory_)) {
                                        flushPendingDelayedCommit(liveIc, st);
                                    }
                                }
                            }
                            return false;
                        });
                    if (timer) {
                        replaceOut = "forward_bs_delay15ms";
                        state->setPendingDelayedCommit(insertStr,
                                                       std::move(timer));
                    } else {
                        replaceOut = "forward_bs_sync";
                        ScopedSelfCommit guard(state);
                        state->markClientMutated();
                        ic->commitString(insertStr);
                    }
                } else {
                    replaceOut = "forward_bs_sync";
                    ScopedSelfCommit guard(state);
                    state->markClientMutated();
                    ic->commitString(insertStr);
                }
            } else {
                replaceOut = "forward_bs_sync";
            }
        } else {
            state->markClientMutated();
            disarmSessionJustAutocorrected(state, monotonicNowMs());
            replaceOut = "aborted_no_delete";
        }
    } else if (!insertStr.empty()) {
        replaceOut = "insert_only";
        state->markSelfReplace(monotonicNowMs());
        ScopedSelfCommit guard(state);
        state->markClientMutated();
        ic->commitString(insertStr);
    } else {
        return;
    }

    emitReplace(replaceOut);
}

void SmartKeyAddon::applyReplaceAction(InputContext *ic, SmartKeyState *state,
                                       KeyEvent &keyEvent,
                                       const SmartKeyAction &act) {
    if (act.delete_chars > 0) {
        const bool fallbackAvailable =
            canForwardBackspace(ic->frontendName(), ic->capabilityFlags());
        state->checkUnansweredBeforeDelete(fallbackAvailable);
        const bool surroundUsable = state->canUseDeleteSurrounding(
            ic->capabilityFlags().test(CapabilityFlag::SurroundingText),
            ic->surroundingText().isValid(), ic->surroundingText().cursor(),
            act.delete_chars, ic->frontendName());
        if (!surroundUsable && !fallbackAvailable) {
            flushPendingDelayedCommit(ic, state);
            state->markClientMutated();
            disarmSessionJustAutocorrected(state, monotonicNowMs());
            if (canTraceContext(ic, normalizeProgramId(ic->program()))) {
                TraceEventFields f{};
                f.ev = "replace";
                f.out = "aborted_no_delete";
                f.hasDel = true;
                f.del = act.delete_chars;
                if (act.insert_utf8 != nullptr && act.insert_byte_len > 0) {
                    f.hasTxt = true;
                    f.txt.assign(act.insert_utf8, act.insert_byte_len);
                }
                f.s0 = snapshotSessionCompact(state->session());
                const std::string appName = normalizeProgramId(ic->program());
                const std::string imName =
                    instance_ != nullptr ? instance_->inputMethod(ic) : "";
                emitTraceRecord(trace_, ic, state, appName, imName,
                                monotonicNowMs(), f);
            }
            return;
        }
    }
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

    const std::string keyboardLang = resolveDirectKeyboardLang(ic);
    const bool directKeyboardActive = !keyboardLang.empty();
    const bool lotusActive = !directKeyboardActive && isLotusActive(ic);
    if (!directKeyboardActive && !lotusActive) {
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
    if (state->shouldLogProfileTransition(profile, reason)) {
        smartkey_engine_log_app_event(engine_, appName.c_str(), profile, reason);
    }
    if (profile == SMARTKEY_PROFILE_BYPASS) {
        state->cancelPendingDelayedCommit();
        state->resetLotusTracking();
        state->resetSurroundingSync();
        return;
    }

    const uint64_t nowMs = monotonicNowMs();
    const bool trace = isTraceActive();
    const std::string imName =
        (trace && instance_ != nullptr) ? instance_->inputMethod(ic) : "";
    const Key rawKey = keyEvent.rawKey();
    const bool isModKey = rawKey.isModifier() || keyEvent.key().isModifier();

    auto emitPreKey = [&](std::string_view out, const std::string &s0,
                          const std::string &s1, const std::string &ch) {
        if (!trace) {
            return;
        }
        TraceEventFields f{};
        f.ev = "pre_key";
        f.out = out;
        f.hasKey = true;
        f.rawKey = rawKey;
        f.ch = ch;
        f.s0 = s0;
        f.s1 = s1;
        emitTraceRecord(trace_, ic, state, appName, imName, nowMs, f);
    };

    if (lotusActive) {
        smartkey_session_set_lang(state->session(), "vi");
        const std::string s0 =
            trace ? snapshotSessionCompact(state->session()) : std::string{};

        // `fcitx5-lotus-server` terminates every uinput replacement burst with `FcitxKey_Pause`.
        // Re-arm the echo grace window across `Pause` (rather than clearing to 0) so chained
        // bursts launched immediately from `LotusState::replayBufferedKeys()` stay suppressed.
        if (rawKey.sym() == FcitxKey_Pause) {
            state->markLotusActivity();
            state->rearmLotusBurstEcho(nowMs);
            state->setSuppressPostKey(true);
            if (trace) {
                emitPreKey("lotus_pause", s0,
                           snapshotSessionCompact(state->session()), {});
            }
            return;
        }

        // Ignore synthetic `BackSpace` / `Left` / `Delete` echoes injected by `fcitx5-lotus`
        // during a mid-word or break-time replacement burst. Note that `LotusState` forwards
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
            if (trace) {
                emitPreKey("lotus_echo_bs", s0,
                           snapshotSessionCompact(state->session()), {});
            }
            return;
        }

        bool isShortcutReset = false;
        uint8_t keyKind = SMARTKEY_KEY_CHAR;
        uint32_t codepoint = 0;
        if (!decodeKeyEvent(keyEvent, isShortcutReset, keyKind, codepoint)) {
            if (trace && !isModKey) {
                emitPreKey("decode_skip", s0, s0, {});
            }
            return;
        }
        const std::string chUtf8 =
            (trace && keyKind == SMARTKEY_KEY_CHAR && codepoint >= 0x20 &&
             codepoint != 0x7F)
                ? encodeUtf8(codepoint)
                : std::string{};

        // On Wayland compositors without echoFrontend, `LotusWlrVirtualKeyboard` emits
        // precomposed non-ASCII runes or ASCII chars mapped onto `kTextKeys[0..11]` (XKB 10..21).
        // Always suppress those synthetic echoes so late post-Space ASCII echoes cannot leak
        // into `SmartKeySession` as phantom leading letters on the next word; only re-arm burst
        // tracking when `cp >= 0x80`, inside an active burst, or awaiting a boundary commit so
        // fast `<250ms` user `BackSpace` undo on a completed word is preserved.
        if (!isShortcutReset && keyKind == SMARTKEY_KEY_CHAR &&
            isSyntheticVkKey(static_cast<uint32_t>(rawKey.code()), codepoint)) {
            state->markClientMutated();
            if (codepoint >= 0x80 || state->isInLotusBurst(nowMs) ||
                state->hasPendingBoundary()) {
                state->markLotusActivity();
                state->rearmLotusBurstEcho(nowMs);
            }
            if (state->hasPendingBoundary()) {
                state->appendVkBoundaryCommit(encodeUtf8(codepoint));
                if (codepoint == state->pendingBoundaryCp()) {
                    if (trace) {
                        emitPreKey("vk_boundary_pass", s0,
                                   snapshotSessionCompact(state->session()),
                                   chUtf8);
                    }
                    return;
                }
            }
            state->setSuppressPostKey(true);
            if (trace) {
                emitPreKey("vk_synth", s0,
                           snapshotSessionCompact(state->session()), chUtf8);
            }
            return;
        }

        flushPendingDelayedCommit(ic, state);

        if (isShortcutReset) {
            state->markClientMutated();
            state->resetLotusTracking();
            SmartKeyAction unused{};
            smartkey_session_process_key(state->session(), engine_,
                                         SMARTKEY_KEY_NAV_OR_CLICK, 0, nowMs,
                                         &unused);
            if (trace) {
                emitPreKey("shortcut_reset", s0,
                           snapshotSessionCompact(state->session()), chUtf8);
            }
            return;
        }

        // If a previous boundary key is waiting for a Lotus burst commit and the user already
        // types non-boundary letter(s) of the next word into `LotusState::bufferedKeys_`,
        // queue them for replay after `onCommitString` finishes evaluating the prior word.
        bool boundaryAbortedNav = false;
        if (state->hasPendingBoundary()) {
            if (keyKind == SMARTKEY_KEY_CHAR &&
                !isSmartKeyBoundaryChar(codepoint) &&
                state->isInLotusBurst(nowMs)) {
                state->pushPostBoundaryChar(codepoint);
                if (isTelexBurstTriggerChar(codepoint) ||
                    state->hasLotusActivityInWord()) {
                    state->armLotusBurst(nowMs);
                }
                state->markClientMutated();
                state->setSuppressPostKey(true);
                if (trace) {
                    emitPreKey("queued_post_boundary", s0,
                               snapshotSessionCompact(state->session()),
                               chUtf8);
                }
                return;
            }
            boundaryAbortedNav = true;
            state->clearPendingBoundary();
            state->takeLotusActivityInWord();
            SmartKeyAction unused{};
            smartkey_session_process_key(state->session(), engine_,
                                         SMARTKEY_KEY_NAV_OR_CLICK, 0, nowMs,
                                         &unused);
        }

        // Record non-boundary letters (`a..z`, etc.) passively in `PreInputMethod` so SmartKey
        // sees every raw keystroke even when `LotusState::keyEvent` filters the key to run a
        // uinput burst. Arm the burst window on Telex triggers OR any subsequent letter after
        // Lotus has already modified the current word (covers mid-word English fallbacks and
        // tone-target shifts).
        if (keyKind == SMARTKEY_KEY_CHAR && !isSmartKeyBoundaryChar(codepoint)) {
            if (isTelexBurstTriggerChar(codepoint) ||
                state->hasLotusActivityInWord()) {
                state->armLotusBurst(nowMs);
            }
            state->markClientMutated();
            SmartKeyAction unused{};
            smartkey_session_process_key(state->session(), engine_,
                                         SMARTKEY_KEY_CHAR, codepoint, nowMs,
                                         &unused);
            if (trace) {
                emitPreKey(boundaryAbortedNav ? "boundary_aborted_nav+lotus_char"
                                              : "lotus_char",
                           s0, snapshotSessionCompact(state->session()), chUtf8);
            }
            return;
        }

        // For word-break/lookahead chars (`Space`, `,`, `.`, `!`, `?`, etc.), `BackSpace`,
        // `Enter`, `Tab`, and cursor nav: defer to `onPostKeyEvent` (if Lotus forwards) or
        // `onCommitString` (if Lotus filters the boundary to restore/commit).
        state->markClientMutated();
        std::string_view outLabel;
        if (keyKind == SMARTKEY_KEY_CHAR && isSmartKeyBoundaryChar(codepoint)) {
            state->armLotusBurst(nowMs);
            state->setPendingBoundary(codepoint);
            outLabel = boundaryAbortedNav
                           ? "boundary_aborted_nav+lotus_defer_boundary"
                           : "lotus_defer_boundary";
        } else {
            state->clearPendingBoundary();
            outLabel = boundaryAbortedNav
                           ? "boundary_aborted_nav+lotus_defer_other"
                           : "lotus_defer_other";
        }
        if (trace) {
            emitPreKey(outLabel, s0, snapshotSessionCompact(state->session()),
                       chUtf8);
        }
        return;
    }

    // `directKeyboardActive` (`keyboard-*` with a resolved direct-keyboard language):
    const std::string s0 =
        trace ? snapshotSessionCompact(state->session()) : std::string{};
    if (instance_->isComposing(ic)) {
        state->markClientMutated();
        if (trace && !isModKey) {
            const uint32_t rawCp = Key::keySymToUnicode(rawKey.sym());
            const std::string rawCh =
                (rawCp >= 0x20 && rawCp != 0x7F) ? encodeUtf8(rawCp) : "";
            emitPreKey("direct_composing_skip", s0, s0, rawCh);
        }
        return;
    }
    state->resetLotusTracking();
    smartkey_session_set_lang(state->session(), keyboardLang.c_str());

    bool isShortcutReset = false;
    uint8_t keyKind = SMARTKEY_KEY_CHAR;
    uint32_t codepoint = 0;
    if (!decodeKeyEvent(keyEvent, isShortcutReset, keyKind, codepoint)) {
        if (trace && !isModKey) {
            emitPreKey("decode_skip", s0, s0, {});
        }
        return;
    }
    const std::string chUtf8 =
        (trace && keyKind == SMARTKEY_KEY_CHAR && codepoint >= 0x20 &&
         codepoint != 0x7F)
            ? encodeUtf8(codepoint)
            : std::string{};

    flushPendingDelayedCommit(ic, state);

    if (isShortcutReset) {
        state->markClientMutated();
        SmartKeyAction unused{};
        smartkey_session_process_key(state->session(), engine_,
                                     SMARTKEY_KEY_NAV_OR_CLICK, 0, nowMs,
                                     &unused);
        if (trace) {
            emitPreKey("shortcut_reset", s0,
                       snapshotSessionCompact(state->session()), chUtf8);
        }
        return;
    }

    SmartKeyAction act{};
    if (!smartkey_session_process_key(state->session(), engine_, keyKind,
                                      codepoint, nowMs, &act)) {
        state->markClientMutated();
        if (trace) {
            emitPreKey("direct_pass", s0,
                       snapshotSessionCompact(state->session()), chUtf8);
        }
        return;
    }

    if (act.kind == SMARTKEY_ACTION_REPLACE) {
        if (trace) {
            emitPreKey("direct_replace", s0,
                       snapshotSessionCompact(state->session()), chUtf8);
        }
        applyReplaceAction(ic, state, keyEvent, act);
    } else {
        state->markClientMutated();
        if (trace) {
            emitPreKey("direct_pass", s0,
                       snapshotSessionCompact(state->session()), chUtf8);
        }
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

    const uint64_t nowMs = monotonicNowMs();
    if (state->takeSuppressPostKey()) {
        if (isTraceActive() && !keyEvent.rawKey().isModifier() &&
            !keyEvent.key().isModifier()) {
            const std::string appName = normalizeProgramId(ic->program());
            if (classifyProfile(ic, appName, nullptr) !=
                SMARTKEY_PROFILE_BYPASS) {
                TraceEventFields f{};
                f.ev = "post_key";
                f.out = "suppressed";
                f.hasKey = true;
                f.rawKey = keyEvent.rawKey();
                const uint32_t rawCp =
                    Key::keySymToUnicode(keyEvent.rawKey().sym());
                if (rawCp >= 0x20 && rawCp != 0x7F) {
                    f.ch = encodeUtf8(rawCp);
                }
                f.s0 = snapshotSessionCompact(state->session());
                const std::string imName =
                    instance_ != nullptr ? instance_->inputMethod(ic) : "";
                emitTraceRecord(trace_, ic, state, appName, imName, nowMs, f);
            }
        }
        return;
    }

    const std::string appName = normalizeProgramId(ic->program());
    smartkey_session_set_app(state->session(), appName.c_str());

    const char *reason = "prose";
    const uint8_t profile = classifyProfile(ic, appName, &reason);
    smartkey_session_set_profile(state->session(), profile);
    if (state->shouldLogProfileTransition(profile, reason)) {
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

    const bool trace = isTraceActive();
    const std::string imName =
        (trace && instance_ != nullptr) ? instance_->inputMethod(ic) : "";
    const std::string chUtf8 =
        (trace && keyKind == SMARTKEY_KEY_CHAR && codepoint >= 0x20 &&
         codepoint != 0x7F)
            ? encodeUtf8(codepoint)
            : std::string{};

    // On Wayland compositors without echoFrontend, `LotusWlrVirtualKeyboard` emits the
    // boundary-restore sequence via virtual key events ending with the boundary char itself.
    if (state->hasPendingBoundary() && !state->vkBoundaryCommit().empty() &&
        keyKind == SMARTKEY_KEY_CHAR &&
        codepoint == state->pendingBoundaryCp()) {
        const std::string s0 =
            trace ? snapshotSessionCompact(state->session()) : std::string{};
        const uint32_t boundaryCp = state->takePendingBoundary();
        const std::string vkCommit = state->takeVkBoundaryCommit();
        const auto queuedChars = state->takePostBoundaryChars();
        state->takeLotusActivityInWord();
        state->clearLotusBurst();

        SmartKeyAction act{};
        const bool isReplace =
            smartkey_session_on_lotus_commit(state->session(), engine_,
                                             boundaryCp, vkCommit.c_str(),
                                             nowMs, &act) &&
            act.kind == SMARTKEY_ACTION_REPLACE;
        if (isReplace) {
            if (act.delete_chars > 0) {
                act.delete_chars -= 1;
            }
            applyReplaceAction(ic, state, keyEvent, act);
        } else {
            state->markClientMutated();
        }
        replayPostBoundaryChars(state, engine_, queuedChars, nowMs);
        if (trace) {
            TraceEventFields f{};
            f.ev = "post_key";
            f.out = isReplace ? "vk_boundary_replace" : "vk_boundary_pass";
            f.hasKey = true;
            f.rawKey = keyEvent.rawKey();
            f.ch = chUtf8;
            f.s0 = s0;
            f.s1 = snapshotSessionCompact(state->session());
            emitTraceRecord(trace_, ic, state, appName, imName, nowMs, f);
        }
        return;
    }

    // Reaching `PostInputMethod` on a boundary/edit/nav key proves `LotusState::keyEvent`
    // forwarded this key without filtering it.
    const std::string s0 =
        trace ? snapshotSessionCompact(state->session()) : std::string{};
    state->clearLotusBurst();
    state->clearPendingBoundary();
    const bool lotusMutatedWord = state->takeLotusActivityInWord();
    smartkey_session_set_lotus_composing(state->session(), lotusMutatedWord);

    SmartKeyAction act{};
    const bool ok = smartkey_session_process_key(state->session(), engine_,
                                                 keyKind, codepoint, nowMs,
                                                 &act);
    const bool isReplace = ok && act.kind == SMARTKEY_ACTION_REPLACE;
    if (isReplace) {
        if (trace) {
            TraceEventFields f{};
            f.ev = "post_key";
            f.out = "forwarded_replace";
            f.hasKey = true;
            f.rawKey = keyEvent.rawKey();
            f.ch = chUtf8;
            f.s0 = s0;
            f.s1 = snapshotSessionCompact(state->session());
            emitTraceRecord(trace_, ic, state, appName, imName, nowMs, f);
        }
        applyReplaceAction(ic, state, keyEvent, act);
    } else {
        state->markClientMutated();
        if (trace) {
            TraceEventFields f{};
            f.ev = "post_key";
            f.out = "forwarded_pass";
            f.hasKey = true;
            f.rawKey = keyEvent.rawKey();
            f.ch = chUtf8;
            f.s0 = s0;
            f.s1 = snapshotSessionCompact(state->session());
            emitTraceRecord(trace_, ic, state, appName, imName, nowMs, f);
        }
    }
}

void SmartKeyAddon::onCommitString(CommitStringEvent &commitEvent) {
    if (engine_ == nullptr) {
        return;
    }
    auto *ic = commitEvent.inputContext();
    if (ic == nullptr) {
        return;
    }
    auto *state = ic->propertyFor(&factory_);
    if (state == nullptr || state->session() == nullptr) {
        return;
    }
    if (state->isSelfCommitting()) {
        if (isTraceActive()) {
            const std::string appName = normalizeProgramId(ic->program());
            if (classifyProfile(ic, appName, nullptr) !=
                SMARTKEY_PROFILE_BYPASS) {
                TraceEventFields f{};
                f.ev = "commit";
                f.out = "self_commit";
                f.hasTxt = true;
                f.txt = commitEvent.text();
                f.s0 = snapshotSessionCompact(state->session());
                const std::string imName =
                    instance_ != nullptr ? instance_->inputMethod(ic) : "";
                emitTraceRecord(trace_, ic, state, appName, imName,
                                monotonicNowMs(), f);
            }
        }
        return;
    }
    state->markClientMutated();
    if (!isLotusActive(ic)) {
        return;
    }

    state->markLotusActivity();
    if (!state->hasPendingBoundary()) {
        if (isTraceActive()) {
            const std::string appName = normalizeProgramId(ic->program());
            if (classifyProfile(ic, appName, nullptr) !=
                SMARTKEY_PROFILE_BYPASS) {
                TraceEventFields f{};
                f.ev = "commit";
                f.out = "lotus_no_pending_boundary";
                f.hasTxt = true;
                f.txt = commitEvent.text();
                f.s0 = snapshotSessionCompact(state->session());
                const std::string imName =
                    instance_ != nullptr ? instance_->inputMethod(ic) : "";
                emitTraceRecord(trace_, ic, state, appName, imName,
                                monotonicNowMs(), f);
            }
        }
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

    const uint64_t nowMs = monotonicNowMs();
    const bool trace = isTraceActive();
    const std::string imName =
        (trace && instance_ != nullptr) ? instance_->inputMethod(ic) : "";
    const std::string &committed = commitEvent.text();
    const std::string boundaryUtf8 = encodeUtf8(state->pendingBoundaryCp());
    const std::size_t pos =
        boundaryUtf8.empty() ? std::string::npos : committed.find(boundaryUtf8);
    if (pos == std::string::npos) {
        // Intermediate mid-word commit (e.g. `"àg"` flushed before a chained burst inside
        // `LotusState::replayBufferedKeys()` while the boundary key is still buffered).
        // Keep `pendingBoundary` intact and extend the burst window so the chained burst's
        // synthetic echoes stay suppressed until the boundary commit arrives.
        const std::string s0 =
            trace ? snapshotSessionCompact(state->session()) : std::string{};
        state->rearmLotusBurstEcho(nowMs);
        if (trace) {
            TraceEventFields f{};
            f.ev = "commit";
            f.out = "lotus_intermediate_burst";
            f.hasTxt = true;
            f.txt = committed;
            f.s0 = s0;
            emitTraceRecord(trace_, ic, state, appName, imName, nowMs, f);
        }
        return;
    }

    const std::string s0 =
        trace ? snapshotSessionCompact(state->session()) : std::string{};
    smartkey_session_set_lang(state->session(), "vi");
    const std::string wordCommitPart =
        committed.substr(0, pos + boundaryUtf8.size());
    const std::string afterBoundaryPart =
        committed.substr(pos + boundaryUtf8.size());
    const uint32_t boundaryCp = state->takePendingBoundary();
    const auto queuedChars = state->takePostBoundaryChars();
    state->takeLotusActivityInWord();
    state->clearLotusBurst();

    SmartKeyAction act{};
    const bool hasAction = smartkey_session_on_lotus_commit(
        state->session(), engine_, boundaryCp, wordCommitPart.c_str(), nowMs,
        &act);
    const bool isReplace = hasAction && act.kind == SMARTKEY_ACTION_REPLACE;

    if (isReplace) {
        if (afterBoundaryPart.empty()) {
            applyDeleteAndCommit(ic, state, act);
        } else {
            const uint32_t extraAfterChars = countUtf8Chars(afterBoundaryPart);
            std::string combinedInsert;
            if (act.insert_utf8 != nullptr && act.insert_byte_len > 0) {
                combinedInsert.assign(act.insert_utf8, act.insert_byte_len);
            }
            combinedInsert += afterBoundaryPart;
            SmartKeyAction adjustedAct = act;
            adjustedAct.delete_chars = act.delete_chars + extraAfterChars;
            adjustedAct.insert_utf8 = combinedInsert.c_str();
            adjustedAct.insert_byte_len =
                static_cast<uint32_t>(combinedInsert.size());
            applyDeleteAndCommit(ic, state, adjustedAct);
        }
    }

    replayPostBoundaryChars(state, engine_, queuedChars, nowMs);
    if (trace) {
        TraceEventFields f{};
        f.ev = "commit";
        f.out = isReplace ? "lotus_boundary_replace" : "lotus_boundary_pass";
        f.hasTxt = true;
        f.txt = committed;
        f.s0 = s0;
        f.s1 = snapshotSessionCompact(state->session());
        emitTraceRecord(trace_, ic, state, appName, imName, nowMs, f);
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
    if (ic == nullptr) {
        return;
    }
    const bool directKeyboardActive = !resolveDirectKeyboardLang(ic).empty();
    const bool lotusActive = !directKeyboardActive && isLotusActive(ic);
    if (!directKeyboardActive && !lotusActive) {
        return;
    }
    auto *state = ic->propertyFor(&factory_);
    if (state == nullptr || state->session() == nullptr) {
        return;
    }
    if (!ic->capabilityFlags().test(CapabilityFlag::SurroundingText) ||
        !ic->surroundingText().isValid()) {
        state->markClientMutated();
        return;
    }
    const uint64_t nowMs = monotonicNowMs();
    const auto &st = ic->surroundingText();
    const bool trace = isTraceActive();
    const std::string appName =
        trace ? normalizeProgramId(ic->program()) : std::string{};
    const bool traceAllowed =
        trace &&
        classifyProfile(ic, appName, nullptr) != SMARTKEY_PROFILE_BYPASS;
    const std::string imName =
        (traceAllowed && instance_ != nullptr) ? instance_->inputMethod(ic) : "";

    const DeleteCheckVerdict delVerdict =
        state->verifyPendingSurroundingDelete(st.text(), st.cursor(),
                                              st.anchor());
    if (delVerdict == DeleteCheckVerdict::DroppedDeletion) {
        const std::string s0 =
            traceAllowed ? snapshotSessionCompact(state->session())
                         : std::string{};
        disarmSessionJustAutocorrected(state, nowMs);
        const std::string appLabel =
            appName.empty() ? normalizeProgramId(ic->program()) : appName;
        FCITX_WARN() << "SmartKey detected dropped SurroundingText delete in "
                        "app="
                     << appLabel << " fe=" << ic->frontendName()
                     << "; disabling SurroundingText delete for context";
        const char *reason = "prose";
        const uint8_t newProfile = classifyProfile(ic, appLabel, &reason);
        smartkey_session_set_profile(state->session(), newProfile);
        if (state->shouldLogProfileTransition(newProfile, reason)) {
            smartkey_engine_log_app_event(engine_, appLabel.c_str(), newProfile,
                                          reason);
        }
        if (traceAllowed) {
            TraceEventFields f{};
            f.ev = "surround";
            f.out = "dropped_delete_untrusted";
            f.s0 = s0;
            f.s1 = snapshotSessionCompact(state->session());
            f.hasScr = true;
            f.scr = extractSurroundingSlice(st.text(), st.cursor(), st.anchor());
            emitTraceRecord(trace_, ic, state, appName, imName, nowMs, f);
        }
        return;
    }
    if (delVerdict == DeleteCheckVerdict::StalePreCommit) {
        if (traceAllowed) {
            std::string slice =
                extractSurroundingSlice(st.text(), st.cursor(), st.anchor());
            if (state->shouldEmitSurroundTrace(
                    st.cursor(), st.anchor(), state->isSurroundingSynced(),
                    slice)) {
                TraceEventFields f{};
                f.ev = "surround";
                f.out = "stale_pre_commit";
                f.s0 = snapshotSessionCompact(state->session());
                f.hasScr = true;
                f.scr = std::move(slice);
                emitTraceRecord(trace_, ic, state, appName, imName, nowMs, f);
            }
        }
        return;
    }

    if (state->isSelfCommitting() ||
        (lotusActive &&
         (state->isInLotusBurst(nowMs) || state->hasPendingBoundary()))) {
        if (traceAllowed) {
            std::string slice =
                extractSurroundingSlice(st.text(), st.cursor(), st.anchor());
            if (state->shouldEmitSurroundTrace(
                    st.cursor(), st.anchor(), state->isSurroundingSynced(),
                    slice)) {
                TraceEventFields f{};
                f.ev = "surround";
                f.out = "skipped_guard";
                f.s0 = snapshotSessionCompact(state->session());
                f.hasScr = true;
                f.scr = std::move(slice);
                emitTraceRecord(trace_, ic, state, appName, imName, nowMs, f);
            }
        }
        return;
    }
    state->markSurroundingUpdated();
    const std::string s0 =
        traceAllowed ? snapshotSessionCompact(state->session()) : std::string{};
    smartkey_session_reconcile_surrounding(state->session(), engine_,
                                           st.text().c_str(), st.cursor(),
                                           nowMs);
    if (traceAllowed) {
        const std::string s1 = snapshotSessionCompact(state->session());
        std::string slice =
            extractSurroundingSlice(st.text(), st.cursor(), st.anchor());
        const bool changedSession = (s0 != s1);
        const bool dedupDiff = state->shouldEmitSurroundTrace(
            st.cursor(), st.anchor(), state->isSurroundingSynced(), slice);
        if (changedSession || dedupDiff) {
            TraceEventFields f{};
            f.ev = "surround";
            f.out = changedSession ? "reconcile_reset" : "synced";
            f.s0 = s0;
            f.s1 = s1;
            f.hasScr = true;
            f.scr = std::move(slice);
            emitTraceRecord(trace_, ic, state, appName, imName, nowMs, f);
        }
    }
}

} // namespace fcitx::smartkey

FCITX_ADDON_FACTORY_V2_BACKWARDS(smartkey, fcitx::smartkey::SmartKeyAddonFactory)

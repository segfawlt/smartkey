/*
 * SPDX-License-Identifier: GPL-3.0-or-later
 *
 * Verifies that Fcitx5's real SharedLibrary AddonManager discovers, dlopens,
 * initializes, reloads, and cleanly unloads `libsmartkey.so` via user-space
 * `XDG_DATA_HOME/fcitx5/addon/smartkey.conf` WITHOUT `FCITX_ADDON_DIRS` or root.
 */

#include <cassert>
#include <cstdlib>
#include <ctime>
#include <filesystem>
#include <fstream>
#include <iostream>
#include <memory>
#include <unistd.h>

#include <fcitx/addonmanager.h>

#include "../src/smartkey-addon.h"

namespace {

void verifyProfileClassification() {
    using fcitx::smartkey::classifyProgramName;

    // Wayland reverse-DNS & Flatpak terminals -> BYPASS
    assert(classifyProgramName("com.mitchellh.ghostty") == SMARTKEY_PROFILE_BYPASS);
    assert(classifyProgramName("org.wezfurlong.wezterm") == SMARTKEY_PROFILE_BYPASS);
    assert(classifyProgramName("io.github.alacritty") == SMARTKEY_PROFILE_BYPASS);
    assert(classifyProgramName("org.alacritty.Alacritty") == SMARTKEY_PROFILE_BYPASS);
    assert(classifyProgramName("org.gnome.Terminal") == SMARTKEY_PROFILE_BYPASS);
    assert(classifyProgramName("org.gnome.Terminal.Server") == SMARTKEY_PROFILE_BYPASS);
    assert(classifyProgramName("org.gnome.Console") == SMARTKEY_PROFILE_BYPASS);
    assert(classifyProgramName("org.gnome.Ptyxis") == SMARTKEY_PROFILE_BYPASS);
    assert(classifyProgramName("org.kde.konsole") == SMARTKEY_PROFILE_BYPASS);
    assert(classifyProgramName("org.kde.yakuake") == SMARTKEY_PROFILE_BYPASS);
    assert(classifyProgramName("com.raggesilver.BlackBox") == SMARTKEY_PROFILE_BYPASS);

    // Classic binary / path terminals -> BYPASS
    assert(classifyProgramName("ghostty") == SMARTKEY_PROFILE_BYPASS);
    assert(classifyProgramName("/usr/bin/ghostty") == SMARTKEY_PROFILE_BYPASS);
    assert(classifyProgramName("kitty") == SMARTKEY_PROFILE_BYPASS);
    assert(classifyProgramName("foot") == SMARTKEY_PROFILE_BYPASS);
    assert(classifyProgramName("st") == SMARTKEY_PROFILE_BYPASS);

    // Wayland reverse-DNS & Flatpak code editors / IDEs -> BYPASS
    assert(classifyProgramName("dev.zed.Zed") == SMARTKEY_PROFILE_BYPASS);
    assert(classifyProgramName("com.visualstudio.code") == SMARTKEY_PROFILE_BYPASS);
    assert(classifyProgramName("zcode") == SMARTKEY_PROFILE_BYPASS);
    assert(classifyProgramName("/usr/bin/zcode") == SMARTKEY_PROFILE_BYPASS);
    assert(classifyProgramName("com.vscodium.codium") == SMARTKEY_PROFILE_BYPASS);
    assert(classifyProgramName("com.sublimetext.three") == SMARTKEY_PROFILE_BYPASS);
    assert(classifyProgramName("com.jetbrains.rustrover") == SMARTKEY_PROFILE_BYPASS);
    assert(classifyProgramName("com.jetbrains.DataGrip") == SMARTKEY_PROFILE_BYPASS);
    assert(classifyProgramName("org.jetbrains.PhpStorm") == SMARTKEY_PROFILE_BYPASS);
    assert(classifyProgramName("com.google.AndroidStudio") == SMARTKEY_PROFILE_BYPASS);
    assert(classifyProgramName("jetbrains-idea") == SMARTKEY_PROFILE_BYPASS);

    // Browsers (classic & reverse-DNS) -> BROWSER
    assert(classifyProgramName("zen") == SMARTKEY_PROFILE_BROWSER);
    assert(classifyProgramName("app.zen_browser.zen") == SMARTKEY_PROFILE_BROWSER);
    assert(classifyProgramName("io.github.zen_browser.zen") == SMARTKEY_PROFILE_BROWSER);
    assert(classifyProgramName("org.mozilla.firefox") == SMARTKEY_PROFILE_BROWSER);
    assert(classifyProgramName("com.brave.Browser") == SMARTKEY_PROFILE_BROWSER);
    assert(classifyProgramName("com.microsoft.Edge") == SMARTKEY_PROFILE_BROWSER);
    assert(classifyProgramName("com.opera.Opera") == SMARTKEY_PROFILE_BROWSER);

    // Chat apps (classic & reverse-DNS) -> CHAT
    assert(classifyProgramName("org.telegram.desktop") == SMARTKEY_PROFILE_CHAT);
    assert(classifyProgramName("com.discordapp.Discord") == SMARTKEY_PROFILE_CHAT);
    assert(classifyProgramName("dev.vencord.Vesktop") == SMARTKEY_PROFILE_CHAT);
    assert(classifyProgramName("org.signal.Signal") == SMARTKEY_PROFILE_CHAT);
    assert(classifyProgramName("im.riot.Riot") == SMARTKEY_PROFILE_CHAT);
    assert(classifyProgramName("org.gnome.Fractal") == SMARTKEY_PROFILE_CHAT);

    // General prose apps -> PROSE
    assert(classifyProgramName("org.gnome.TextEditor") == SMARTKEY_PROFILE_PROSE);
    assert(classifyProgramName("md.obsidian.Obsidian") == SMARTKEY_PROFILE_PROSE);
    assert(classifyProgramName("libreoffice") == SMARTKEY_PROFILE_PROSE);

    // Boundary vs non-boundary key classification for Lotus 2-phase bridge
    using fcitx::smartkey::isSmartKeyBoundaryChar;
    assert(isSmartKeyBoundaryChar(' '));
    assert(isSmartKeyBoundaryChar('.'));
    assert(isSmartKeyBoundaryChar(','));
    assert(isSmartKeyBoundaryChar('!'));
    assert(isSmartKeyBoundaryChar('?'));
    assert(!isSmartKeyBoundaryChar('a'));
    assert(!isSmartKeyBoundaryChar('t'));
    assert(!isSmartKeyBoundaryChar('s'));

    // Telex burst trigger classification & burst state machine
    using fcitx::smartkey::isTelexBurstTriggerChar;
    using fcitx::smartkey::LotusBridgeTracker;
    assert(isTelexBurstTriggerChar('s'));
    assert(isTelexBurstTriggerChar('w'));
    assert(isTelexBurstTriggerChar('e'));
    assert(isTelexBurstTriggerChar('['));
    assert(!isTelexBurstTriggerChar('t'));
    assert(!isTelexBurstTriggerChar('n'));
    assert(!isTelexBurstTriggerChar('g'));

    LotusBridgeTracker st{};
    assert(!st.isInBurst(1000));
    st.armBurst(1000);
    // Must stay armed past Lotus's 200ms `kReplacementAckSlackMs`
    assert(st.isInBurst(1250));
    assert(!st.isInBurst(1400));
    st.rearmBurstEcho(1250);
    assert(st.isInBurst(1500));

    st.setSuppressPost(true);
    assert(st.takeSuppressPost());
    assert(!st.takeSuppressPost());

    st.setBoundary(' ');
    assert(st.hasPendingBoundary);
    assert(st.boundaryCp() == ' ');
    assert(st.takeBoundary() == ' ');
    assert(!st.hasPendingBoundary);

    // Virtual-keyboard synthetic key classification & fast-typing queues
    using fcitx::smartkey::isSyntheticVkKey;
    assert(isSyntheticVkKey(24, 0x00E0));   // 'à' (>= 0x80, non-boundary)
    assert(isSyntheticVkKey(38, 0x1EA1));   // 'ạ' (>= 0x80, non-boundary)
    assert(!isSyntheticVkKey(24, 0x201C));  // curly opening quote is a boundary char
    assert(isSyntheticVkKey(10, 'o'));      // XKB 10 (KEY_1) emitting ASCII letter
    assert(isSyntheticVkKey(14, ' '));      // XKB 14 emitting Space
    assert(!isSyntheticVkKey(10, '1'));     // Physical digit '1' on XKB 10
    assert(!isSyntheticVkKey(38, 'a'));     // Physical 'a' on XKB 38

    assert(!st.hasActivityInWord());
    st.markActivity();
    assert(st.hasActivityInWord());
    assert(st.takeActivityInWord());
    assert(!st.hasActivityInWord());

    st.setBoundary(' ');
    st.pushPostBoundaryChar('t');
    st.pushPostBoundaryChar('i');
    const auto queued = st.takePostBoundaryChars();
    assert(queued.size() == 2 && queued[0] == 't' && queued[1] == 'i');
    assert(st.takePostBoundaryChars().empty());

    st.appendVkBoundaryCommit("ongx");
    st.appendVkBoundaryCommit(" ");
    assert(st.vkBoundaryCommit() == "ongx ");
    assert(st.takeVkBoundaryCommit() == "ongx ");
    assert(st.vkBoundaryCommit().empty());
}

void verifyKeyboardLangNormalization() {
    using fcitx::smartkey::normalizeKeyboardLangCode;

    assert(normalizeKeyboardLangCode("keyboard-us", "en") == "en");
    assert(normalizeKeyboardLangCode("keyboard-latam", "es") == "es");
    assert(normalizeKeyboardLangCode("keyboard-fr", "fr_FR") == "fr");
    assert(normalizeKeyboardLangCode("keyboard-pt", "pt-BR") == "pt");
    assert(normalizeKeyboardLangCode("keyboard-brai", "") == "");
    assert(normalizeKeyboardLangCode("keyboard-vn", "vi") == "");
    assert(normalizeKeyboardLangCode("lotus", "vi") == "");

    // Additional edge cases: case-folding, regional Vietnamese, non-keyboard IMs, empty subtags
    assert(normalizeKeyboardLangCode("keyboard-us", "EN_US") == "en");
    assert(normalizeKeyboardLangCode("keyboard-es", "es_ES") == "es");
    assert(normalizeKeyboardLangCode("keyboard-vn", "vi_VN") == "");
    assert(normalizeKeyboardLangCode("keyboard-trans", "") == "");
    assert(normalizeKeyboardLangCode("keyboard-de", "de") == "de");
    assert(normalizeKeyboardLangCode("keyboard-us", "_US") == "");
    assert(normalizeKeyboardLangCode("", "en") == "");
}

void verifyWaylandSurroundingSyncAndDelayedTimer() {
    using fcitx::smartkey::isWaylandFrontend;
    using fcitx::smartkey::SurroundingSyncTracker;

    // Frontend classification
    assert(isWaylandFrontend("wayland"));
    assert(isWaylandFrontend("wayland_v2"));
    assert(!isWaylandFrontend("xcb"));
    assert(!isWaylandFrontend("x11"));
    assert(!isWaylandFrontend("dbus"));
    assert(!isWaylandFrontend("ibus"));
    assert(!isWaylandFrontend("wayland_v3"));
    assert(!isWaylandFrontend(""));

    SurroundingSyncTracker sync{};
    assert(!sync.surroundingSynced);

    // Wayland unsynced: never trust potentially stale SurroundingText
    assert(!sync.canUseDeleteSurrounding(true, true, 10, 5, "wayland_v2"));
    assert(!sync.canUseDeleteSurrounding(true, true, 10, 5, "wayland"));

    // X11 / DBus: allowed when SurroundingText is valid and cursor >= deleteChars
    assert(sync.canUseDeleteSurrounding(true, true, 10, 5, "xcb"));
    assert(sync.canUseDeleteSurrounding(true, true, 8, 3, "x11"));
    assert(sync.canUseDeleteSurrounding(true, true, 5, 5, "dbus"));

    // Wayland synced: allowed once fresh SurroundingTextUpdated arrives
    sync.markSurroundingUpdated();
    assert(sync.surroundingSynced);
    assert(sync.canUseDeleteSurrounding(true, true, 10, 5, "wayland_v2"));
    assert(sync.canUseDeleteSurrounding(true, true, 5, 5, "wayland"));

    // Cursor bounds & capability/validity guards across all frontends
    assert(!sync.canUseDeleteSurrounding(true, true, 4, 5, "wayland_v2"));
    assert(!sync.canUseDeleteSurrounding(true, true, 0, 1, "wayland"));
    assert(!sync.canUseDeleteSurrounding(true, true, 2, 3, "xcb"));
    assert(!sync.canUseDeleteSurrounding(true, true, 0, 1, "dbus"));
    assert(!sync.canUseDeleteSurrounding(false, true, 10, 5, "wayland_v2"));
    assert(!sync.canUseDeleteSurrounding(true, false, 10, 5, "wayland_v2"));
    assert(!sync.canUseDeleteSurrounding(false, true, 10, 5, "dbus"));
    assert(!sync.canUseDeleteSurrounding(true, false, 10, 5, "xcb"));

    // Client mutation invalidates Wayland surrounding sync until next update
    sync.markClientMutated();
    assert(!sync.surroundingSynced);
    assert(!sync.canUseDeleteSurrounding(true, true, 10, 5, "wayland_v2"));
    assert(!sync.canUseDeleteSurrounding(true, true, 10, 5, "wayland"));
    assert(sync.canUseDeleteSurrounding(true, true, 10, 5, "dbus"));

    // Context reset also clears Wayland surrounding sync
    sync.markSurroundingUpdated();
    assert(sync.canUseDeleteSurrounding(true, true, 6, 6, "wayland_v2"));
    sync.reset();
    assert(!sync.surroundingSynced);
    assert(!sync.canUseDeleteSurrounding(true, true, 6, 6, "wayland_v2"));

    // Verify 15ms EventSourceTime one-shot callback safely resets its owning unique_ptr
    fcitx::EventLoop loop;
    bool timerFired = false;
    std::unique_ptr<fcitx::EventSourceTime> delayedTimer;
    delayedTimer = loop.addTimeEvent(
        CLOCK_MONOTONIC, fcitx::now(CLOCK_MONOTONIC) + 15000, 0,
        [&](fcitx::EventSourceTime *, uint64_t) {
            auto localTimer = std::move(delayedTimer);
            if (localTimer) {
                localTimer->setEnabled(false);
            }
            timerFired = true;
            loop.exit();
            return false;
        });
    assert(delayedTimer != nullptr);
    assert(loop.exec());
    assert(timerFired);
    assert(delayedTimer == nullptr);

    // Self-replace reset-echo guard prevents GTK/Gecko Wayland `InputContextReset` bursts
    // within 150ms of SmartKey's BackSpace+commit replacement from wiping `EditingRestored`.
    using fcitx::smartkey::SelfReplaceResetGuard;
    SelfReplaceResetGuard resetGuard{};
    assert(!resetGuard.shouldIgnoreClientReset(false, false, 1000));
    assert(resetGuard.shouldIgnoreClientReset(true, false, 1000));
    assert(resetGuard.shouldIgnoreClientReset(false, true, 1000));

    resetGuard.markSelfReplace(1000);
    assert(resetGuard.shouldIgnoreClientReset(false, false, 1050));
    assert(resetGuard.shouldIgnoreClientReset(false, false, 1150));
    assert(!resetGuard.shouldIgnoreClientReset(false, false, 1151));

    resetGuard.clear();
    assert(!resetGuard.shouldIgnoreClientReset(false, false, 1050));
}

void verifyTraceRecorderHelpers() {
    using namespace fcitx::smartkey;

    // JSON escaping: quotes, backslashes, newlines, control chars, Vietnamese UTF-8, invalid UTF-8
    {
        std::string escaped;
        appendJsonEscaped(escaped, "say \"hi\"\\slash\n\x01 cần sinh ");
        const unsigned char badByte = 0xFF;
        std::string badStr(1, static_cast<char>(badByte));
        appendJsonEscaped(escaped, badStr);
        assert(escaped.find("\\\"hi\\\"") != std::string::npos);
        assert(escaped.find("\\\\slash") != std::string::npos);
        assert(escaped.find("\\u0001") != std::string::npos);
        assert(escaped.find("cần sinh") != std::string::npos);
        assert(escaped.find("\\n") != std::string::npos);
        assert(escaped.find("\\ufffd") != std::string::npos);
    }

    // Unicode scalar SurroundingText slicing (multi-byte UTF-8 safe)
    {
        const std::string text = "cần sinh";
        assert(extractSurroundingSlice(text, 4, 4) == "cần |sinh");
        assert(extractSurroundingSlice("", 0, 0) == "|");
        assert(extractSurroundingSlice(text, 4, 4, 2, 2) ==
               "\xE2\x80\xA6n |si\xE2\x80\xA6");
        assert(extractSurroundingSlice(text, 4, 1) == "cần |sinh [sel=1]");
        assert(extractSurroundingSlice(text, 99, 99) == "<invalid-cursor>");
        assert(extractSurroundingSlice(text, 20000, 20000) ==
               "<cursor=20000 bytes=" + std::to_string(text.size()) + ">");
    }

    // Session snapshot compact formatter across all modes + prev + lotus_composing
    {
        SmartKeySessionSnapshot idle{};
        assert(formatSessionCompact(idle) == "Idle");

        SmartKeySessionSnapshot comp{};
        comp.mode_utf8 = "Composing";
        comp.buf_utf8 = "caafn";
        comp.buf_byte_len = 5;
        comp.lotus_composing = true;
        assert(formatSessionCompact(comp) == "Composing(caafn)");
        comp.aux_utf8 = "caafnx";
        comp.aux_byte_len = 6;
        assert(formatSessionCompact(comp) == "Composing(caafn,peak=caafnx)");

        SmartKeySessionSnapshot after{};
        after.mode_utf8 = "AfterWord";
        after.buf_utf8 = "cần";
        after.buf_byte_len = static_cast<uint32_t>(std::string_view("cần").size());
        after.trailing_count = 1;
        after.lotus_composing = true;
        assert(formatSessionCompact(after) == "AfterWord(cần,sp=1)");

        SmartKeySessionSnapshot just{};
        just.mode_utf8 = "JustAutocorrected";
        just.buf_utf8 = "nính";
        just.buf_byte_len = static_cast<uint32_t>(std::string_view("nính").size());
        just.aux_utf8 = "nsinh";
        just.aux_byte_len = 5;
        just.prev_utf8 = "cần";
        just.prev_byte_len = static_cast<uint32_t>(std::string_view("cần").size());
        just.trailing_count = 1;
        just.lotus_composing = false;
        assert(formatSessionCompact(just) ==
               "JustAutocorrected(nính<-nsinh,tb=1)[prev=cần][lc=0]");

        SmartKeySessionSnapshot restored{};
        restored.mode_utf8 = "EditingRestored";
        restored.buf_utf8 = "nsin";
        restored.buf_byte_len = 4;
        restored.aux_utf8 = "nsinh";
        restored.aux_byte_len = 5;
        restored.lotus_composing = true;
        assert(formatSessionCompact(restored) == "EditingRestored(nsin<-nsinh)");
    }

    // SurroundingTraceDedup suppresses consecutive identical snapshots
    {
        SurroundingTraceDedup dedup{};
        assert(dedup.shouldEmit(4, 4, true, "cần |sinh"));
        assert(!dedup.shouldEmit(4, 4, true, "cần |sinh"));
        assert(dedup.shouldEmit(5, 5, true, "cần s|inh"));
        assert(!dedup.shouldEmit(5, 5, true, "cần s|inh"));
        assert(dedup.shouldEmit(5, 5, false, "cần s|inh"));
        dedup.reset();
        assert(dedup.shouldEmit(5, 5, false, "cần s|inh"));
    }

    // TraceLogger: 0600 file permissions, .1 rotation when maxFileBytes is exceeded, and closeIfOpen
    {
        namespace fs = std::filesystem;
        const fs::path logDir =
            fs::temp_directory_path() /
            ("fcitx5_smartkey_trace_" + std::to_string(::getpid()));
        fs::remove_all(logDir);

        TraceLogger logger;
        logger.setLogDir(logDir);
        logger.setMaxFileBytes(180);

        // 2026-11-18T00:00:00Z in epoch ms (1794528000000? Let's compute exact UTC day 20775)
        const uint64_t fixedEpochMs = 1794528000000ULL; // 2026-11-13
        const std::string expectedDay =
            TraceLogger::formatUtcDate(static_cast<int64_t>(fixedEpochMs / 86400000ULL));

        auto writeRec = [&](std::string_view word) {
            auto &line = logger.beginLine();
            line.append("{\"v\":1,\"ts_ms\":");
            line.append(std::to_string(fixedEpochMs));
            line.append(",\"ev\":\"pre_key\",\"app\":\"zen\",\"txt\":");
            appendJsonString(line, word);
            line.append(",\"scr\":\"cần |sinh\",\"pad\":\"012345678901234567890123456789\"}");
            logger.commitLine(fixedEpochMs);
        };

        writeRec("first");
        assert(logger.isOpen());
        const fs::path activePath = logDir / ("trace-" + expectedDay + ".jsonl");
        const fs::path rotPath = logDir / ("trace-" + expectedDay + ".jsonl.1");
        assert(fs::exists(activePath));

        struct stat st{};
        assert(::stat(activePath.c_str(), &st) == 0);
        assert((st.st_mode & 0777) == 0600);

        // Exceed 180 bytes so next write triggers `.1` rotation.
        writeRec("second-to-exceed-threshold");
        assert(!fs::exists(rotPath));
        writeRec("third-after-rotate");
        assert(fs::exists(rotPath));
        assert(fs::exists(activePath));
        assert(fs::file_size(activePath) > 0 && fs::file_size(activePath) < 180);

        logger.closeIfOpen();
        assert(!logger.isOpen());
        fs::remove_all(logDir);
    }
}

} // namespace

int main(int argc, char *argv[]) {
    verifyProfileClassification();
    verifyKeyboardLangNormalization();
    verifyWaylandSurroundingSyncAndDelayedTimer();
    verifyTraceRecorderHelpers();
    if (argc < 2) {
        std::cerr << "usage: addon_load_smoke <libsmartkey.so>\n";
        return 2;
    }

    namespace fs = std::filesystem;
    const fs::path soSrc = fs::absolute(argv[1]);

    const fs::path tmpRoot =
        fs::temp_directory_path() /
        ("fcitx5_smartkey_smoke_" + std::to_string(::getpid()));
    fs::remove_all(tmpRoot);

    const fs::path dataHome = tmpRoot / "data";
    const fs::path cfgHome = tmpRoot / "config";
    const fs::path stateHome = tmpRoot / "state";
    const fs::path addonLibDir = tmpRoot / "local_lib" / "fcitx5";
    const fs::path addonConfDir = dataHome / "fcitx5" / "addon";
    fs::create_directories(addonLibDir);
    fs::create_directories(addonConfDir);
    fs::create_directories(cfgHome);
    fs::create_directories(stateHome);

    const fs::path installedSo = addonLibDir / "libsmartkey.so";
    fs::copy_file(soSrc, installedSo);

    {
        std::ofstream conf(addonConfDir / "smartkey.conf");
        conf << "[Addon]\n"
             << "Name=SmartKey Autocorrect\n"
             << "Type=SharedLibrary\n"
             << "Library=" << (addonLibDir / "libsmartkey").string() << "\n"
             << "Category=Module\n"
             << "Version=0.1.1\n"
             << "OnDemand=False\n"
             << "Configurable=False\n";
    }

    ::setenv("XDG_CONFIG_HOME", cfgHome.c_str(), 1);
    ::setenv("XDG_DATA_HOME", dataHome.c_str(), 1);
    ::setenv("XDG_STATE_HOME", stateHome.c_str(), 1);
    ::unsetenv("FCITX_ADDON_DIRS");

    {
        fcitx::AddonManager mgr;
        mgr.registerDefaultLoader(nullptr);
        mgr.load({"smartkey"}, {"all"});

        auto *addon = mgr.addon("smartkey", true);
        assert(addon != nullptr && "Fcitx5 AddonManager failed to load smartkey");
        addon->reloadConfig();
        addon->save();
        mgr.unload();
    }

    assert(fs::exists(cfgHome / "smartkey" / "rules.toml") &&
           "SmartKeyAddon did not initialize rules.toml in XDG_CONFIG_HOME");

    fs::remove_all(tmpRoot);
    std::cout << "Fcitx5 AddonManager zero-env user-local load smoke test PASSED.\n";
    return 0;
}

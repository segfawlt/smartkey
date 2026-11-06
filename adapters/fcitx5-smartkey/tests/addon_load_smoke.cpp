/*
 * SPDX-License-Identifier: GPL-3.0-or-later
 *
 * Verifies that Fcitx5's real SharedLibrary AddonManager discovers, dlopens,
 * initializes, reloads, and cleanly unloads `libsmartkey.so` via user-space
 * `XDG_DATA_HOME/fcitx5/addon/smartkey.conf` WITHOUT `FCITX_ADDON_DIRS` or root.
 */

#include <cassert>
#include <cstdlib>
#include <filesystem>
#include <fstream>
#include <iostream>
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
    assert(classifyProgramName("com.vscodium.codium") == SMARTKEY_PROFILE_BYPASS);
    assert(classifyProgramName("com.sublimetext.three") == SMARTKEY_PROFILE_BYPASS);
    assert(classifyProgramName("com.jetbrains.rustrover") == SMARTKEY_PROFILE_BYPASS);
    assert(classifyProgramName("jetbrains-idea") == SMARTKEY_PROFILE_BYPASS);

    // Browsers (classic & reverse-DNS) -> BROWSER
    assert(classifyProgramName("zen") == SMARTKEY_PROFILE_BROWSER);
    assert(classifyProgramName("app.zen_browser.zen") == SMARTKEY_PROFILE_BROWSER);
    assert(classifyProgramName("io.github.zen_browser.zen") == SMARTKEY_PROFILE_BROWSER);
    assert(classifyProgramName("org.mozilla.firefox") == SMARTKEY_PROFILE_BROWSER);
    assert(classifyProgramName("com.brave.Browser") == SMARTKEY_PROFILE_BROWSER);

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
}

} // namespace

int main(int argc, char *argv[]) {
    verifyProfileClassification();
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
    const fs::path addonLibDir = tmpRoot / "local_lib" / "fcitx5";
    const fs::path addonConfDir = dataHome / "fcitx5" / "addon";
    fs::create_directories(addonLibDir);
    fs::create_directories(addonConfDir);
    fs::create_directories(cfgHome);

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

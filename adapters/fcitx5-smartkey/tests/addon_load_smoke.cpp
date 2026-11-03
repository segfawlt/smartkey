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

int main(int argc, char *argv[]) {
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
             << "Version=0.1.0\n"
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

/*
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#ifndef FCITX5_SMARTKEY_ADDON_H
#define FCITX5_SMARTKEY_ADDON_H

#include <cstdint>
#include <memory>
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

private:
    SmartKeySessionHandle *session_ = nullptr;
};

class SmartKeyAddon final : public AddonInstance {
public:
    explicit SmartKeyAddon(Instance *instance);
    ~SmartKeyAddon() override;

    void reloadConfig() override;
    void save() override;

private:
    void onKeyEvent(KeyEvent &keyEvent);
    void resetContext(InputContext *ic);
    bool isEnglishKeyboardActive(InputContext *ic) const;
    static uint8_t classifyProfile(InputContext *ic);

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

# SmartKey for Windows

SmartKey for Windows ships the English autocorrect engine, embedded `en` and `vi` dictionaries, default `rules.toml`, and deterministic backspace learning as a Windows Text Services Framework (TSF) text service (`smartkey_tsf.dll`) paired with `smartkey.exe`.

## Install

1. Extract this archive and run PowerShell in the extracted folder:
   - Machine-wide (run as Administrator so 64-bit desktop and Store/WinUI apps load the DLL cleanly):
     `.\install.ps1`
   - Current user only (no Administrator prompt):
     `.\install.ps1 -User`
2. Press `Win+Space` and pick **English (United States) — SmartKey Autocorrect**.
3. Check status any time with `smartkey status`.

To remove the TSF registration later, run `.\uninstall.ps1` (or `.\uninstall.ps1 -User`).

## Files and Learning

- Config and learned rules: `%APPDATA%\smartkey\rules.toml` (created with the default `balanced` preset on install).
- Staged learning counters: `%APPDATA%\smartkey\learner_state.json`.
- Event and trace logs: `%LOCALAPPDATA%\smartkey\logs\`.

Multiple apps share `rules.toml` and `learner_state.json` safely via `.smartkey.lock`. Sandboxed Store/AppContainer apps read your rules without writing to `%APPDATA%`.

## Coexisting with UniKey, EVKey, and OpenKey

SmartKey targets English autocorrect on Windows and steps out of Vietnamese input:

- Injected characters (`VK_PACKET`, non-ASCII runes, and synthetic `BackSpace` bursts) pass straight through without editing.
- When Vietnamese input is detected, valid unaccented Vietnamese syllables (`anh`, `thanh`, `trong`) are protected until `Ctrl+Shift`, `Alt+Z`, or two consecutive English-only words signal English mode again.
- `smartkey.exe` runs `status`, `enable`, `disable`, `toggle`, `reload`, `rules`, `config`, `explain`, `app`, `log`, `trace`, and `dict` from PowerShell or Windows Terminal.

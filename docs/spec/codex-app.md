# Codex app integration

`dalgona codex install` MUST make the Codex desktop app use the current native dalgona executable. The executable path MUST be absolute. An install MUST fail before changing a target when the path is empty or a required file cannot be read.

The installer MUST record its prior state in `<dalgona-home>/codex-install.json` before it changes any target. The record MUST contain every file's prior bytes and whether the file existed. The record MUST use mode `0600`. The installer MUST publish the record with a temporary file and rename.

On Linux, the installer MUST write `~/.config/environment.d/60-dalgona-codex.conf` with one `CODEX_CLI_PATH=<executable>` line. When `/usr/share/applications/chatgpt.desktop` exists, it MUST write a user copy at `~/.local/share/applications/chatgpt.desktop` and prefix its `Exec=` command with `env CODEX_CLI_PATH=<executable> `. It MUST update the user VS Code settings file at `~/.config/Code/User/settings.json`.

On macOS, the installer MUST set `CODEX_CLI_PATH` with `launchctl setenv`. It MUST write `~/Library/LaunchAgents/dev.dalgona.codex-cli-path.plist` with `RunAtLoad` and the same executable. It MUST update the user VS Code settings file at `~/Library/Application Support/Code/User/settings.json`.

On Windows, the installer MUST set `HKCU\\Environment\\CODEX_CLI_PATH` and broadcast `WM_SETTINGCHANGE`. It MUST update the user VS Code settings file under `%APPDATA%\\Code\\User\\settings.json`.

The VS Code edit MUST preserve unrelated bytes, including comments and whitespace. An existing `chatgpt.cliExecutable` value MUST be replaced in place. If the key is absent, the installer MUST insert it after the first opening brace. A missing settings file MUST be created without rewriting a later install's unrelated bytes.

`dalgona codex uninstall` MUST read the record and restore every prior file byte for byte. It MUST remove files that did not exist before install. It MUST restore or remove the prior environment value and then remove the record. A missing record MUST be an error. Uninstall MUST NOT remove an unrelated file.

`dalgona codex status` MUST report each platform mechanism, its path or current value, and whether it matches the requested executable. Install, uninstall, and status MUST use an injectable command runner so tests can exercise launchctl and schtasks behavior without those programs.

The Codex app reads the setting only after restart. The command surface MUST tell the user to restart the Codex app after installation.

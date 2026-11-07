# Uninstall CodexMonitor

[中文权威版本](uninstall.zh-CN.md)；本文件是英文翻译。

Uninstalling removes the application but preserves the current user's database,
settings, and structured logs by default. Data deletion is a separate, explicit
step.

## Before uninstalling

1. If login autostart is enabled, turn it off in the tray or Dashboard settings
   so CodexMonitor removes its current-user startup registration.
2. Choose Quit in the tray. If no tray is available, run the current executable
   with `--quit`.
3. Wait for ordered shutdown to finish before removing application files or
   deleting data. Closing the browser is not sufficient because monitoring
   continues without a Dashboard tab.

If the application cannot start, remove its entry through the operating
system's current-user startup settings after uninstalling.

## Remove the application

### Windows

Windows has no installer to remove. After quitting, run this in the directory
that contains the portable EXE you are about to remove:

```powershell
.\codex-monitor.exe --cleanup-platform-registration
```

The command removes the Windows platform registration for the current EXE. Only
after it succeeds, delete `codex-monitor.exe` and the directory created when the
ZIP was extracted. Do not delete the EXE first, because it can no longer perform
the cleanup.

### macOS

After CodexMonitor has exited, remove `CodexMonitor.app` from Applications or
the directory where it was installed. Eject and delete any downloaded DMG
separately.

### Linux

- For deb or rpm, remove CodexMonitor with the same distribution package
  manager that installed it.
- For AppImage, remove the AppImage file after the resident process has exited.

Removing a Linux package also removes its packaged files. It does not delete the
current user's application data directory.

## Data retained after uninstall

The following directory is intentionally preserved:

| Platform | Data directory                                                                                  |
| -------- | ----------------------------------------------------------------------------------------------- |
| Windows  | `%LOCALAPPDATA%\CodexMonitor`                                                                   |
| macOS    | `~/Library/Application Support/CodexMonitor`                                                    |
| Linux    | `$XDG_DATA_HOME/codex-monitor`, or `~/.local/share/codex-monitor` when `XDG_DATA_HOME` is unset |

This allows a later reinstall or upgrade to reuse the SQLite history and stored
settings. The directory can contain pseudonymous conversation/thread
identifiers and should be treated as sensitive local data.

## Delete all local data

These commands are irreversible. Confirm that CodexMonitor has exited, verify
the path, and keep any desired local backup before running the command. Do not
upload the backup.

### Windows PowerShell

```powershell
Remove-Item -LiteralPath "$env:LOCALAPPDATA\CodexMonitor" -Recurse -Force
```

### macOS terminal

```sh
rm -rf -- "$HOME/Library/Application Support/CodexMonitor"
```

### Linux terminal

```sh
rm -rf -- "${XDG_DATA_HOME:-$HOME/.local/share}/codex-monitor"
```

The commands remove the local SQLite database, settings, and sanitized
structured logs under the application data directory. They cannot remove copies,
backups, filesystem snapshots, or attachments stored elsewhere.

## Verify removal

1. Confirm no CodexMonitor process remains. If one is still running, issue
   `--quit` before retrying deletion.
2. Confirm the Windows portable EXE, application entry, or AppImage is gone. On
   Windows, `--cleanup-platform-registration` must have run before deletion.
3. If full deletion was requested, confirm the platform data directory no longer
   exists.
4. Remove or disable the Codex OTel exporter configuration if no other local
   receiver uses `http://127.0.0.1:4318/v1/logs`. Otherwise Codex may continue
   attempting to send logs to an absent receiver.

See the [Privacy policy](../../PRIVACY.md) for data that CodexMonitor never
persists and the limits of local deletion.

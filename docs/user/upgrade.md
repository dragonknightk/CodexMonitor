# Upgrade CodexMonitor

[中文权威版本](upgrade.zh-CN.md)；本文件是英文翻译。

CodexMonitor upgrades are overwrite installs. The application data directory is
preserved, and the new version applies its forward-only SQLite migrations before
accepting telemetry.

## Before you upgrade

1. Confirm that the new version is newer than the installed version. Downgrades
   are unsupported and forbidden.
2. Quit the resident process with the tray Quit action. If the tray is missing,
   run the installed executable with `--quit` and wait for it to exit.
3. Keep the same operating-system architecture and, where practical, the same
   package family.
4. If you make a backup, copy the application data directory only after the
   process has stopped. Treat that copy as sensitive local data and do not upload
   it.
5. Check the new artifact's signing/development label and associated native
   workflow run. `unsigned-development` is a development build, not a formal
   signed release.

## Upgrade on Windows

Quit the resident process, extract the newer x64 portable ZIP, then overwrite
the EXE at the same path with its single `codex-monitor.exe`. Launch it normally
once afterwards. An overwrite at the same path preserves data and saved settings
in `%LOCALAPPDATA%\CodexMonitor`; notifications and login autostart need no
reconfiguration.

If the upgrade also moves or renames the EXE, manually launch it once from the
new path to restore its notification identity. If login autostart had been
enabled, turn it off and on again so its registration uses the new path. A formal
tagged release requires Windows signing credentials; a credential-free
development package is labeled `unsigned-development` and can show Windows trust
warnings.

## Upgrade on macOS

Quit CodexMonitor, then replace the existing `CodexMonitor.app` with the newer
x64 or arm64 application of the same architecture. If using a DMG, copy the new
application over the old one and eject the DMG. Launch the replaced application
normally.

Formal tagged releases require Developer ID signing and notarization. A macOS
`unsigned-development` build is ad-hoc signed only for local validation and is
neither Developer ID signed nor notarized.

## Upgrade on Linux

- For deb or rpm, use the distribution package manager to install the newer
  package over the existing package. This keeps dependency resolution for
  WebKitGTK 4.1 and AppIndicator active.
- For AppImage, quit the process, replace the AppImage at its stable path, retain
  its executable permission, and launch it normally.

The AppImage remains tied to the Ubuntu 22.04 glibc baseline and the host's
WebKitGTK runtime. If the dynamic loader cannot load WebKitGTK, the new process
does not start and no monitoring is active. Missing tray/AppIndicator or
notification services after a successful start only disables those platform
capabilities; the engine, OTLP receiver, SQLite, and Dashboard continue.

## Import legacy history

The packaged application includes a native, one-time-compatible importer for an
explicitly selected legacy Python SQLite database. It does not start, wrap, or
require the old Python runtime.

Run the installed executable with an absolute source path:

```text
codex-monitor --import-legacy <absolute-sqlite-path>
```

You may quit CodexMonitor first and run the command, or invoke it while the
resident instance is active. In the latter case, the single-instance route
forwards the validated import request to the existing process. Relative paths
are rejected. Use a drive-qualified absolute path on Windows and a `/`-rooted
absolute path on macOS or Linux.

The importer opens the source read-only, validates its schema, and copies only
allowlisted structured fields in one atomic transaction. The source file remains
unchanged. Imported records are historical and cannot create realtime alerts or
notifications. A successful import, or a request for a source already imported,
opens the Dashboard.

## Verify the upgrade

After the first start:

1. Confirm that the Dashboard opens in the default browser and that the existing
   local history remains available.
2. Confirm the source status reports the OTLP receiver on
   `127.0.0.1:4318/v1/logs` as available.
3. Make a normal Codex request and verify that a new model call appears.
4. Review the autostart and notification settings; both should retain the user's
   stored choices.
5. Close the browser and confirm monitoring remains resident. Launch the app
   again to reopen the existing instance's Dashboard.

The Dashboard may use a different loopback port after restart because port
`4319` is only preferred. Do not hard-code or bookmark a bootstrap URL.

## Downgrade policy

Downgrades are forbidden. Database migrations are append-only and have no
down-migration path. Do not overwrite a newer Windows portable EXE with an older
one, force an older deb/rpm/AppImage, or replace a macOS application with an
older build against a database opened by a newer version.

If an upgrade must be reversed, stop and collect the version and safe status
code. Restore only through a project-approved recovery procedure; do not edit
the SQLite schema or delete migration records.

## If upgrade fails

- If files are reported as in use, issue `--quit`, wait for ordered shutdown,
  and retry the overwrite install.
- If the Dashboard works but OTLP is unavailable, resolve a local port `4318`
  conflict; reinstalling does not free that port.
- On Linux, distinguish a missing tray/notification capability from a process
  that cannot start because WebKitGTK is missing.
- Do not delete the application data directory as a first troubleshooting step.

Continue with [Troubleshooting](troubleshooting.md) and the complete
[Privacy policy](../../PRIVACY.md).

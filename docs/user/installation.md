# Install CodexMonitor

[中文权威版本](installation.zh-CN.md)；本文件是英文翻译。

CodexMonitor is a normal-user resident application. It does not install a
system service, bind a LAN interface, or create a visible desktop WebView in v1.

## Supported packages

| Platform | Architecture | Packages           | Additional requirement                           |
| -------- | ------------ | ------------------ | ------------------------------------------------ |
| Windows  | x64          | Portable ZIP       | Supported Windows desktop session                |
| macOS    | x64, arm64   | `.app`, DMG        | macOS 12.0 or newer                              |
| Linux    | x64          | AppImage, deb, rpm | WebKitGTK 4.1; AppIndicator for tray integration |

Linux AppImages use Ubuntu 22.04 as the glibc compatibility baseline. Linux
arm64 is not part of the v1 package matrix.

## Before installation

1. Download the package for your operating system and CPU architecture from the
   project's release page.
2. Check the artifact label. `unsigned-development` identifies a development
   build with different trust guarantees from a formal tagged release.
3. Do not install or test with a copied personal Codex database or log. The
   application creates its own current-user data directory.
4. If replacing an existing installation, follow [Upgrade](upgrade.md) and quit
   the resident process before overwriting files.

## Install on Windows

The Windows x64 release is a portable ZIP. When extracted, its root contains
only `codex-monitor.exe`; place that EXE in the local directory where it will be
kept, then launch it normally. There is no NSIS, MSI, or system-wide install
step, and it must not be configured as a Windows Service.

Each manual launch self-calibrates the Windows notification identity from the
current EXE path. The first launch needs no extra configuration. The EXE in a
formal tagged release is expected to be signed. An `unsigned-development` build
may trigger Windows trust warnings and is intended only for controlled
development validation.

## Install on macOS

CodexMonitor requires macOS 12.0 or newer. Choose the x64 artifact for an Intel
Mac or the arm64 artifact for Apple silicon.

- For a DMG, open it and copy `CodexMonitor.app` to Applications.
- For a standalone `.app`, place it in Applications or another stable local
  application directory.

Launch the copied application normally. Formal tagged releases require a
Developer ID signature and notarization. The macOS `unsigned-development`
artifact is ad-hoc signed only for local validation; it is not Developer ID
signed or notarized. Follow your organization's local application trust policy
rather than treating that build as a formal release.

## Install on Linux

Use the x64 package matching the distribution:

- Install deb and rpm files through the distribution package manager so it can
  resolve WebKitGTK 4.1 and AppIndicator dependencies.
- Mark an AppImage executable, keep it at a stable path, and launch it as the
  desktop user. The AppImage is built against the Ubuntu 22.04 glibc baseline.

The application still needs the host dynamic loader to find the required
WebKitGTK runtime. Correct deb/rpm dependency metadata should prevent an
incomplete installation. An AppImage on a host without the required WebKitGTK
runtime fails during startup; the engine has not started and monitoring does not
continue.

After a successful process start, a desktop without tray/AppIndicator support or
a user notification service can still run the engine, OTLP receiver, SQLite,
and Dashboard. Use another manual launch to open the Dashboard when no tray is
available.

## First launch

Launch CodexMonitor normally. The single resident process starts and the system
default browser opens the authenticated Dashboard. The final Dashboard address
is loopback-only: it prefers `127.0.0.1:4319` and uses another available
loopback port when necessary.

The first install does not enable login autostart. Turn it on explicitly in the
tray or Dashboard settings if wanted. The registration uses `--background`, so
later login starts do not open a browser. System notifications can also be
disabled in settings; alerts remain visible in the Dashboard. After moving or
renaming a Windows portable EXE, manually launch it once from the new path to
restore its notification identity. If login autostart was enabled, turn it off
and on again so it points to the new path.

Closing the browser does not stop CodexMonitor. Use the tray Quit action or run
the installed executable with `--quit` for an ordered exit.

## Configure Codex

Configure Codex to send OTel logs to the fixed local receiver while keeping
prompt logging, metrics, and traces disabled:

```toml
[analytics]
enabled = false

[otel]
environment = "local"
log_user_prompt = false
metrics_exporter = "none"
trace_exporter = "none"
exporter = { otlp-http = { endpoint = "http://127.0.0.1:4318/v1/logs", protocol = "binary" } }
```

Restart Codex after saving its configuration. Make one normal Codex request,
then check the Dashboard source status and recent calls. CodexMonitor accepts
OTLP binary Protobuf, OTLP Protobuf JSON, and gzip on `/v1/logs`; the binary
configuration above is the normal path.

## Data location

CodexMonitor creates current-user local files in:

| Platform | Data directory                                                                                  |
| -------- | ----------------------------------------------------------------------------------------------- |
| Windows  | `%LOCALAPPDATA%\CodexMonitor`                                                                   |
| macOS    | `~/Library/Application Support/CodexMonitor`                                                    |
| Linux    | `$XDG_DATA_HOME/codex-monitor`, or `~/.local/share/codex-monitor` when `XDG_DATA_HOME` is unset |

The directory contains the SQLite database, settings, and sanitized structured
logs. Uninstalling preserves it by default. See [Privacy](../../PRIVACY.md) and
[Uninstall and delete data](uninstall.md).

## Build trust and release status

The release workflow targets four native runner jobs: Windows x64, macOS x64,
macOS arm64, and Linux x64. Documentation or a source checkout does not prove
those jobs passed; inspect the workflow run associated with the artifact.

A formal tagged release fails if the required Windows signing credentials or
macOS Developer ID signing/notarization credentials are unavailable. Credential-
free development packages are labeled `unsigned-development`; the macOS package
uses only an ad-hoc local signature. Do not describe such artifacts as signed or
notarized releases.

## Next steps

- [Upgrade](upgrade.md)
- [Uninstall and delete data](uninstall.md)
- [Troubleshooting](troubleshooting.md)
- [Privacy](../../PRIVACY.md)

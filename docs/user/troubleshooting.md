# Troubleshoot CodexMonitor

[中文权威版本](troubleshooting.zh-CN.md)；本文件是英文翻译。

CodexMonitor deliberately avoids raw-payload diagnostics. Start with observable
status and synthetic reproduction steps; do not attach personal Codex data.

## Start with safe diagnostics

Record only:

- CodexMonitor version, operating system version, and CPU architecture;
- package type: Windows portable ZIP, `.app`, DMG, AppImage, deb, or rpm;
- artifact label, including `unsigned-development` when present;
- whether the process starts, whether the Dashboard opens, and whether the OTLP
  source is available;
- the closed safe status code shown by the Dashboard or tray;
- the approximate event time and steps using synthetic content.

Do not paste a bootstrap URL, database row, raw OTLP body, local path, account or
conversation identifier, Codex log, session index, or native error message into
a report.

## App does not start

1. Confirm the package matches the operating system and architecture. macOS
   requires 12.0 or newer; Linux v1 packages are x64.
2. Confirm the resident process is not already running. A second normal launch
   forwards an Open Dashboard request to the existing instance and then exits.
3. If startup reports a migration or database-integrity code, do not downgrade,
   edit the SQLite schema, or delete migration records. Preserve the local data
   directory and report the safe code.
4. On Windows, confirm the ZIP was extracted and its root contains only
   `codex-monitor.exe`, then check whether an `unsigned-development` artifact
   was blocked by local application-trust policy. A formal tagged release should
   be signed.
5. On macOS, check the version, architecture, and artifact label. The
   `unsigned-development` app is ad-hoc signed for local validation only and is
   not Developer ID signed or notarized.
6. On Linux, verify the AppImage has executable permission and that the dynamic
   loader can find WebKitGTK 4.1. deb/rpm installs should resolve this dependency
   through the package manager.

If WebKitGTK cannot load, the Linux process fails before the engine starts. In
that case there is no monitoring, OTLP receiver, SQLite writer, or Dashboard to
fall back to.

## Dashboard does not open

v1 has no visible desktop WebView. A normal manual launch asks the system default
browser to open the Dashboard. Try another manual launch or choose Open Dashboard
from the tray; both request a fresh one-time bootstrap URL from the existing
instance.

The Dashboard prefers `127.0.0.1:4319` but selects another available loopback
port when necessary. Do not assume `4319`, hard-code the URL, or bookmark a
bootstrap address. A successful bootstrap redirects to a clean URL and stores an
`HttpOnly` local session cookie.

If the browser reaches that clean loopback URL but shows `HTTP ERROR 404`, the
resident API is running but the installed executable is not serving its packaged
Dashboard document. Record only the version and package type, quit from the tray,
and install a current package. Reopening the same installation or changing
browsers does not repair a missing packaged presentation route.

Closing the browser does not stop monitoring. If no browser opens, check the
operating system's default-browser association and local policy. Do not expose
the Dashboard through a LAN proxy or change the listener away from loopback.

## No telemetry arrives

Verify the Codex configuration uses the exact local logs endpoint and keeps
prompt, metrics, and trace export disabled:

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

Restart Codex after changing the configuration, make a normal request, and
check the Dashboard source status. The Dashboard port is unrelated to the fixed
OTLP receiver port; do not send OTel logs to `4319` or to a random Dashboard
port.

If the source is healthy but no call appears, reproduce with synthetic content
and report the safe source code. Do not capture or upload a real OTLP body.

## Port 4318 is in use

Another process binding `127.0.0.1:4318` prevents the CodexMonitor OTLP receiver
from starting. CodexMonitor keeps the resident host and Dashboard available and
reports the receiver conflict; it does not move the OTLP receiver to another
port because the Codex endpoint is fixed.

Identify and stop or reconfigure the other local receiver, then use the receiver
retry control when offered or restart CodexMonitor. Reinstalling CodexMonitor
does not release a port owned by another process.

## Linux tray or notifications are unavailable

After the process has started successfully, missing tray/AppIndicator support or
a missing user-session notification service is a supported degraded state. The
engine, `127.0.0.1:4318` OTLP receiver, SQLite storage, and loopback Dashboard
continue. Manually launch CodexMonitor again to open the Dashboard when no tray
is visible. Alerts remain in Dashboard history when notification delivery is
unavailable.

This degradation does not apply to a missing WebKitGTK dynamic-loader runtime.
If that dependency prevents startup, the process and monitoring are absent.

## System notifications do not appear

1. Check that reasoning notifications are enabled in CodexMonitor settings.
2. Check the operating system's per-application notification permission and
   focus/do-not-disturb settings.
3. Check the alert history. An alert can exist while its action is suppressed,
   unavailable, failed, or coalesced with another notification.
4. Remember that the default reasoning-pattern threshold notifies only eligible
   low-`n` matches. Higher matches can remain in history without a notification.

CodexMonitor does not retry a notification whose delivery became ambiguous
after it started, preventing duplicate user-visible notifications after a
crash. The underlying alert remains available.

After a Windows portable EXE is moved or renamed, manually launch it normally
once from the new path. That launch self-calibrates the notification identity
for the current path and restores notifications; no manual notification
registration change is needed.

## Login autostart does not run

Login autostart is off by default. Enable it explicitly in the tray or Dashboard
settings and confirm the displayed setting remains enabled. Registration is
per-user and launches the same application with `--background`, so no browser
opens at login.

Keep the executable targeted by login autostart at a stable path. If a Windows
portable EXE, an AppImage, or another registered file was moved or renamed,
disable and re-enable login autostart from the running application to refresh
the registration.

## Legacy import is rejected

Use the installed native command with an absolute SQLite path:

```text
codex-monitor --import-legacy <absolute-sqlite-path>
```

Relative paths are rejected. The command can run after an explicit quit or while
the resident instance is active; an active instance receives the request through
the single-instance route. The importer does not require the old Python runtime.

Do not modify the source schema or copy raw tables into the current database. A
successful or already-imported source opens the Dashboard. If it does not, keep
the source unchanged and report only the safe import status code, package type,
and synthetic reproduction steps.

## Upgrade or uninstall is blocked

Closing Dashboard tabs leaves the resident process running and can keep
application files in use. Choose tray Quit or run the current executable with
`--quit`, wait for ordered shutdown, and retry. Before deleting a Windows
portable EXE, run `--cleanup-platform-registration`.

Do not force a downgrade. Upgrades overwrite the application and preserve data;
uninstall also preserves data by default. Follow [Upgrade](upgrade.md) or
[Uninstall and delete data](uninstall.md) rather than deleting the SQLite files
while the process is active.

## Report a problem safely

Include the safe facts listed at the top of this document and a minimal
synthetic reproduction. If the problem concerns an artifact, link the public
native workflow run instead of claiming the four platform jobs passed.

Never upload the application data directory, database, WAL/SHM files, settings,
Codex logs, `session_index.jsonl`, raw telemetry, bootstrap URL, signing material,
or a screenshot containing identifiers. Review the [Privacy policy](../../PRIVACY.md)
before sending a report.

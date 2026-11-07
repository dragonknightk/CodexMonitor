# 原生打包与发布

CodexMonitor 使用两套明确分离的 native packaging workflow。正式 workflow 从 version tag 发布经过身份验证的 release；开发 workflow 生成名称和 manifest 均含 `unsigned-development` 的产物，仅有 repository read permission，不能访问 signing secrets、OIDC、attestation 或 GitHub Releases。

## 支持矩阵

| Runner           | Rust target                | Tauri bundles      | 发布产物                            |
| ---------------- | -------------------------- | ------------------ | ----------------------------------- |
| `windows-2025`   | `x86_64-pc-windows-msvc`   | 无                 | 只含 `codex-monitor.exe` 的便携 ZIP |
| `macos-15-intel` | `x86_64-apple-darwin`      | `app,dmg`          | `.app.zip`、`.dmg`                  |
| `macos-15`       | `aarch64-apple-darwin`     | `app,dmg`          | `.app.zip`、`.dmg`                  |
| `ubuntu-22.04`   | `x86_64-unknown-linux-gnu` | `appimage,deb,rpm` | `.AppImage`、`.deb`、`.rpm`         |

所有 job 使用 Node.js `24.14.0`、pnpm `11.13.0`、`rust-toolchain.toml` 中的 Rust `1.97.0`、锁定的 Tauri CLI `2.11.4` 与不可变 GitHub Action revision。Workflow 先构建应用 EXE；macOS 与 Linux 再调用 Tauri bundler。Windows 把已签名或 unsigned 的 EXE 归档为一个只含该文件的确定性便携 ZIP。

Bundle 与 Unix autostart identifier 是 `dev.codexmonitor.desktop`。Release contract 拒绝以 macOS 保留 `.app` suffix 结尾的 identifier。`apps/desktop/src-tauri/tauri.conf.json`、根 `package.json` 与 Cargo workspace 的 version 必须相同，正式 tag 必须精确为 `v<version>`。

## Release 前置完整门禁

`.github/workflows/release.yml` 的 `verify` job 在任何平台 package job 前运行，执行 `pnpm verify` 与 `pnpm audit:security`。它覆盖 format、crate boundary、Clippy、一次 workspace tests、tooling、frontend unit、产品集成、desktop build、完整 Playwright matrix、文档、依赖 audit 与 secret scan。`package` job 明确 `needs: verify`，前置失败时不会接触 signing credentials。

`main` push、每周 schedule 和显式完整 CI 同样执行完整门禁。PR 由风险分类选择 jobs；release tag 始终全量。

## Unsigned development packages

`.github/workflows/package-unsigned.yml` 可手动运行，也会在 packaging 相关 PR 上运行。Windows 的便携 EXE 与 Linux 包不签名；macOS 使用 ad-hoc identity `-`，只满足平台 executable signature 要求，不认证 publisher 且不 notarize。

Target manifest 明确记录：

- Windows/Linux：`codeSigning: none`；
- macOS：`codeSigning: ad-hoc` 与 `notarized: false`。

这些产物不能作为正式 release 分发，也不能从名称中删除 `unsigned-development`。每个 target upload 包含发布产物、manifest、CycloneDX JSON SBOM，以及覆盖其他文件且稳定排序的 SHA-256 checksum file。

## 正式发布凭据

Tagged workflow 使用 GitHub `release` environment。推送 version tag 前配置：

| Secret                         | 内容                                   |
| ------------------------------ | -------------------------------------- |
| `WINDOWS_CERTIFICATE_BASE64`   | Base64 Authenticode PFX                |
| `WINDOWS_CERTIFICATE_PASSWORD` | PFX password                           |
| `WINDOWS_TIMESTAMP_URL`        | RFC 3161 timestamp service URL         |
| `APPLE_CERTIFICATE_BASE64`     | Base64 Developer ID Application P12    |
| `APPLE_CERTIFICATE_PASSWORD`   | P12 password                           |
| `APPLE_SIGNING_IDENTITY`       | 精确 Developer ID Application identity |
| `APPLE_API_ISSUER`             | App Store Connect API issuer ID        |
| `APPLE_API_KEY`                | App Store Connect API key ID           |
| `APPLE_API_PRIVATE_KEY_BASE64` | Base64 App Store Connect P8 key        |

Windows 或 macOS credential 缺失会使 tagged matrix 失败，正式 workflow 不会 fallback 到 unsigned package。

Windows custom sign command 直接从 `RUNNER_TEMP` 读取 PFX，在打入 ZIP 前验证唯一便携 EXE 的 Authenticode 与 timestamp，不把 private key 导入 user certificate store。macOS 在 `RUNNER_TEMP` 创建临时 keychain 与 P8，验证 app/DMG 的 Developer ID signature、Gatekeeper assessment 与 stapled ticket。两者都在 `always()` cleanup 删除临时 key material。

## Release assets 与 attestations

四个 native jobs 必须合计生成精确 8 个发布产物：Windows 1 个便携 ZIP、每个 macOS architecture 2 个、Linux 3 个。每个 target 还生成一个 manifest 与一个由 packaged executable 产生的 CycloneDX JSON SBOM。

每个发布产物获得同一 build 的 SBOM attestation 和 GitHub SLSA build-provenance attestation。Assembly job 只接受 8 个发布产物、4 个 manifests 与 4 个 SBOMs，扁平化为唯一文件名，再生成覆盖其他 16 个 asset 的 `CodexMonitor-v<version>-SHA256SUMS`。Publish job 再次执行 allowlist/digest 检查，先创建 draft release，核对 upload names 后才移除 draft。既有 tag release 不覆盖。

下载后可验证：

```text
gh attestation verify <artifact> --repo dragonknightk/CodexMonitor
```

## Installed smoke contract

Smoke job 从 primary packaged artifact 派生稳定路径，不测试 `target` 中的 raw executable。

- Windows：把便携 ZIP 分别解压到两个 `RUNNER_TEMP` 隔离目录，确认每个目录只含根目录的 `codex-monitor.exe`，并以 `--require-notifications` 运行。第二个路径的 smoke 验证通知身份会随当前 EXE 路径自校准；可见 toast 仍属于人工 installed check。
- macOS：把 signed `.app` 复制到 `RUNNER_TEMP` 下隔离 Applications-like directory，再运行其中 executable。
- Linux：把 AppImage 复制到稳定隔离路径，运行正常 smoke 并等待退出，再以全新 profile 运行 forced degraded smoke；两次都使用 isolated D-Bus session 与 Xvfb。

每个平台都在 `always()` cleanup 删除临时安装。Windows 删除便携 EXE 前以该 EXE 运行 `--cleanup-platform-registration`。Smoke 覆盖 synthetic legacy import、binary OTLP、SQLite projection、Dashboard assets/API、516 alert、platform lifecycle、source-file immutability 与 isolated profile。

Linux 在 smoke 前检查 DEB `Depends` 与 RPM `Requires` 中的 WebKitGTK 4.1，确认 `ldd` 无 unresolved dependency 且含 `libwebkit2gtk-4.1.so`，并确认 AppImage 是 ELF。`--linux-degraded` 只强制 tray/notification degradation，不模拟缺失 WebKitGTK；缺少该 runtime 时 desktop 进程无法启动。

## 供应链限制

Workflow 固定 source toolchains、GitHub Actions 与 Syft `v1.46.0`。macOS/Linux Tauri bundler 打包时仍可能下载平台 helper，构建尚未完全 hermetic。未来 hardening 应预填 checksum-verified Tauri tools cache，或使用仓库维护的 per-platform pinned image。不要以动态下载的仓库自写 installer script 填补该空缺。

## 本机静态验证

以下命令不创建 native package，也不使用 signing credential：

```text
node --check scripts/release/*.mjs
pnpm test:release
pnpm exec prettier --check .github/workflows/package-unsigned.yml .github/workflows/release.yml scripts/release docs/development/releasing.md
pnpm docs:check
```

Native packaging、signature verification、notarization 与 installed smoke 仍由 GitHub-hosted runner 提供执行证据。

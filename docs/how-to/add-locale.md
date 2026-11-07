# 增加界面语言

在新 locale 下增加每个 feature namespace，key 与 named interpolation parameter 必须和 `en-US` 完全一致。Model name、event name、API field、configuration key、filesystem path 和 protocol identifier 保持原文。

在 frontend 登记 locale matching 与 fallback，再增加 tray 和 notification 使用的 native host catalog。日期与数字通过 active locale 的 `Intl` 格式化，API 值继续使用原始 UTC timestamp 与 number。

运行 catalog consistency 与 visible-text 检查。Playwright 需要覆盖 desktop/mobile、切换语言无需重启、持久选择、overflow 与 accessible name。开发期可以运行 `test:e2e:locales`，最终交付运行完整四项目 production matrix。

```text
pnpm check:localization
pnpm test:web:unit
pnpm --dir web test:e2e:locales
```

`pnpm check:localization` 检查 namespace parity、translation keys、named interpolation parameters、Dashboard JSX text 与 native host menu/notification literals。有意不翻译的 JSX identifier 使用 `translate="no"`；native literal 需要同一行 `i18n-visible-ok:` 原因。

import assert from "node:assert/strict";
import { mkdtemp, mkdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import {
  findCatalogViolations,
  findDashboardVisibleTextViolations,
  findLocalizationViolations,
  findNativeVisibleTextViolations,
  requiredNamespaces,
  supportedLocales,
} from "../check-localization.mjs";

async function temporaryCatalogs(t) {
  const root = await mkdtemp(join(tmpdir(), "codex-monitor-locales-"));
  t.after(() => rm(root, { force: true, recursive: true }));
  for (const locale of supportedLocales) {
    await mkdir(join(root, locale), { recursive: true });
    for (const namespace of requiredNamespaces) {
      await writeFile(
        join(root, locale, `${namespace}.json`),
        JSON.stringify({ message: "Value {{count}}" }),
        "utf8",
      );
    }
  }
  return root;
}

test("catalog validation accepts complete keys and matching parameters", async (t) => {
  const root = await temporaryCatalogs(t);
  assert.deepEqual(await findCatalogViolations(root), []);
});

test("catalog validation reports key and interpolation drift", async (t) => {
  const root = await temporaryCatalogs(t);
  await writeFile(
    join(root, "zh-CN", "dashboard.json"),
    JSON.stringify({ message: "值 {{total}}", extra: "额外" }),
    "utf8",
  );

  const violations = await findCatalogViolations(root);
  assert.ok(violations.some((message) => message.includes("extra key extra")));
  assert.ok(
    violations.some((message) => message.includes("interpolation mismatch")),
  );
});

test("Dashboard visible-text validation requires catalogs or an explicit no-translate boundary", () => {
  const violations = findDashboardVisibleTextViolations(
    `
      export function Example() {
        return <><button title="Save now">Save</button><code translate="no">gpt-5.5</code><span>{t("label")}</span></>;
      }
    `,
    "Example.tsx",
  );

  assert.deepEqual(
    violations.map(({ kind, text }) => ({ kind, text })),
    [
      { kind: "visible JSX attribute title", text: "Save now" },
      { kind: "visible JSX text", text: "Save" },
    ],
  );
});

test("native visible-text validation guards menu and notification literals", () => {
  const violations = findNativeVisibleTextViolations(
    `
      let item = MenuItem::new(app, "Open Dashboard", true, None)?;
      notification.title("Reasoning alert").body(localized_body);
      notification.tooltip("CodexMonitor"); // i18n-visible-ok: product identifier
    `,
    "host.rs",
  );
  assert.deepEqual(
    violations.map(({ text }) => text),
    ["Open Dashboard", "Reasoning alert"],
  );
});

test("committed localization and production presentation boundaries pass", async () => {
  assert.deepEqual(await findLocalizationViolations(), []);
});

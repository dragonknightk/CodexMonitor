import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { parse } from "yaml";

import {
  executeSelection,
  selectChecks,
  validateVerificationMap,
} from "../verify-changed.mjs";

const root = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  "../..",
);
const config = validateVerificationMap(
  parse(
    readFileSync(path.join(root, "docs/_meta/verification-map.yaml"), "utf8"),
  ),
);

function selectedIds(changedPaths) {
  return selectChecks(config, changedPaths).map((check) => check.id);
}

test("selects isolated Rust, docs and web verification scopes", async (t) => {
  await t.test("Rust-only", () => {
    const ids = selectedIds(["crates/codex-monitor-telemetry/src/decoder.rs"]);
    assert.ok(ids.includes("telemetry-check"));
    assert.ok(ids.includes("telemetry-test"));
    assert.ok(ids.includes("crate-boundaries"));
    assert.ok(!ids.includes("web-unit"));
    assert.ok(!ids.includes("browser-full"));
  });

  await t.test("docs-only", () => {
    assert.deepEqual(selectedIds(["docs/architecture/system-overview.md"]), [
      "docs-check",
    ]);
  });

  await t.test("web-only", () => {
    const ids = selectedIds(["web/src/features/calls/CallsView.tsx"]);
    assert.ok(ids.includes("web-lint"));
    assert.ok(ids.includes("web-unit"));
    assert.ok(ids.includes("dashboard-build"));
    assert.ok(ids.includes("browser-smoke"));
    assert.ok(!ids.includes("test-rust"));
  });
});

test("cross-cutting contracts and migrations upgrade their checks", () => {
  const apiIds = selectedIds(["web/src/api/events.ts"]);
  assert.ok(apiIds.includes("api-check"));
  assert.ok(apiIds.includes("dashboard-test"));
  assert.ok(apiIds.includes("browser-full"));

  const migrationIds = selectedIds([
    "crates/codex-monitor-storage/migrations/0010_example.sql",
  ]);
  for (const id of [
    "storage-test",
    "database-schema-check",
    "privacy-check",
    "docs-check",
    "product-integration",
  ]) {
    assert.ok(migrationIds.includes(id), id);
  }
  assert.equal(migrationIds.filter((id) => id === "storage-test").length, 1);
});

test("fails closed for an unknown managed path", () => {
  assert.throws(
    () => selectedIds(["new-subsystem/implementation.rs"]),
    /verification map has no owner/u,
  );
});

test("full verification follows the declared stage order", () => {
  const selection = selectChecks(config, [], { full: true });
  const stageOrder = new Map(
    config.stages.map((stage, index) => [stage.id, index]),
  );
  for (let index = 1; index < selection.length; index += 1) {
    assert.ok(
      stageOrder.get(selection[index - 1].stage) <=
        stageOrder.get(selection[index].stage),
    );
  }
});

test("proven coverage removes only selected subset checks and reports them", () => {
  const selection = selectChecks(config, [
    "crates/codex-monitor-core/src/lib.rs",
  ]);
  const ids = selection.map((check) => check.id);
  assert.ok(ids.includes("workspace-check"));
  assert.ok(ids.includes("test-rust"));
  assert.ok(!ids.includes("core-check"));
  assert.ok(!ids.includes("core-test"));
  assert.ok(!ids.includes("product-integration"));

  const output = [];
  executeSelection(config, selection, {
    dryRun: true,
    write: (line) => output.push(line),
  });
  assert.ok(output.includes("[覆盖] workspace-check — 已覆盖 core-check"));
  assert.ok(output.includes("[覆盖] test-rust — 已覆盖 core-test"));
});

test("workspace library coverage only declares library test targets", () => {
  const testRust = config.checks.find((check) => check.id === "test-rust");
  for (const checkId of testRust.covers) {
    const covered = config.checks.find((check) => check.id === checkId);
    assert.match(covered.command, /cargo test .+ --lib$/u, checkId);
  }
});

test("unknown and cyclic coverage never omit checks", () => {
  const unknownCoverage = {
    version: 1,
    stages: [{ id: "test", label: "测试" }],
    checks: [{ id: "a", stage: "test", command: "a", covers: ["missing"] }],
    full_checks: ["a"],
    mappings: [],
  };
  assert.throws(
    () => validateVerificationMap(unknownCoverage),
    /covers unknown check/u,
  );

  const cyclicCoverage = {
    version: 1,
    stages: [{ id: "test", label: "测试" }],
    checks: [
      { id: "a", stage: "test", command: "a", covers: ["b"] },
      { id: "b", stage: "test", command: "b", covers: ["a"] },
    ],
    full_checks: ["a", "b"],
    mappings: [],
  };
  assert.deepEqual(
    selectChecks(cyclicCoverage, [], { full: true }).map((check) => check.id),
    ["a", "b"],
  );
});

test("runner reports per-check, per-stage and total elapsed time", () => {
  const selection = selectChecks(config, [
    "crates/codex-monitor-telemetry/src/decoder.rs",
  ]).slice(0, 2);
  const output = [];
  let clock = 0;
  const result = executeSelection(config, selection, {
    execute: () => ({ status: 0 }),
    now: () => {
      clock += 100;
      return clock;
    },
    write: (line) => output.push(line),
  });
  assert.equal(result.checks.length, 2);
  assert.ok(output.some((line) => line.startsWith("[通过]")));
  assert.ok(output.some((line) => line.startsWith("[阶段完成]")));
  assert.ok(output.some((line) => line.startsWith("[验证完成]")));
  assert.ok(output.some((line) => /耗时 \d+\.\d{2}s/u.test(line)));
});

test("package scripts keep categories explicit and Rust tests non-redundant", () => {
  const rootPackage = JSON.parse(
    readFileSync(path.join(root, "package.json"), "utf8"),
  );
  const webPackage = JSON.parse(
    readFileSync(path.join(root, "web/package.json"), "utf8"),
  );
  assert.equal(
    rootPackage.scripts["test:rust"],
    "cargo test --workspace --lib",
  );
  assert.equal(rootPackage.scripts["test:integration"], undefined);
  assert.equal(rootPackage.scripts["test:unit"], undefined);
  assert.equal(
    rootPackage.scripts["test:product-integration"],
    "cargo test -p codex-monitor-engine --test product_integration",
  );
  assert.equal(
    rootPackage.scripts["test:tooling"],
    "node --test scripts/tests/*.test.mjs",
  );
  assert.equal(
    rootPackage.scripts["test:release"],
    "node --test scripts/release/*.test.mjs",
  );
  assert.match(rootPackage.scripts["verify:rust"], /test:product-integration/u);
  assert.match(rootPackage.scripts["build:app"], /pnpm --dir web build/u);
  assert.match(
    rootPackage.scripts["build:app"],
    /cargo build -p codex-monitor-desktop/u,
  );
  for (const script of [
    "test:e2e:smoke",
    "test:e2e:responsive",
    "test:e2e:locales",
    "test:e2e:production",
  ]) {
    assert.equal(typeof webPackage.scripts[script], "string", script);
  }
  assert.equal(
    webPackage.scripts["test:e2e:smoke"],
    'playwright test --project=desktop-en-US --grep "exposes an accessible"',
  );
  assert.equal(
    webPackage.scripts["test:e2e:responsive"],
    "playwright test --project=desktop-en-US --project=mobile-en-US",
  );
  assert.equal(
    webPackage.scripts["test:e2e:locales"],
    "playwright test --project=desktop-en-US --project=desktop-zh-CN",
  );
  assert.equal(webPackage.scripts["test:e2e:production"], "playwright test");
  const playwrightConfig = readFileSync(
    path.join(root, "web/playwright.config.ts"),
    "utf8",
  );
  assert.match(playwrightConfig, /command: "pnpm preview"/u);
  assert.doesNotMatch(playwrightConfig, /pnpm build && pnpm preview/u);
  assert.doesNotMatch(playwrightConfig, /CODEX_MONITOR_USE_PREBUILT_ASSETS/u);
});

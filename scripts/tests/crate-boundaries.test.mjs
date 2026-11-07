import assert from "node:assert/strict";
import test from "node:test";

import {
  validateCrateBoundaries,
  validateDesktopOnlyConfiguration,
} from "../check-crate-boundaries.mjs";

const names = [
  "codex-monitor-core",
  "codex-monitor-storage",
  "codex-monitor-telemetry",
  "codex-monitor-dashboard",
  "codex-monitor-engine",
  "codex-monitor-host",
  "codex-monitor-desktop",
];

function dependency(name, kind = null) {
  return { name, kind };
}

function metadata(overrides = {}) {
  const dependencies = {
    "codex-monitor-core": [],
    "codex-monitor-storage": [dependency("codex-monitor-core")],
    "codex-monitor-telemetry": [dependency("codex-monitor-core")],
    "codex-monitor-dashboard": [dependency("codex-monitor-core")],
    "codex-monitor-engine": [
      dependency("codex-monitor-core"),
      dependency("codex-monitor-storage"),
      dependency("codex-monitor-telemetry"),
      dependency("codex-monitor-dashboard"),
    ],
    "codex-monitor-host": [],
    "codex-monitor-desktop": [
      dependency("codex-monitor-engine"),
      dependency("codex-monitor-host"),
      dependency("tauri"),
    ],
    ...overrides,
  };
  return {
    workspace_members: names.map((name) => `${name} 0.1.0`),
    packages: names.map((name) => ({
      id: `${name} 0.1.0`,
      name,
      dependencies: dependencies[name],
    })),
  };
}

test("accepts the target acyclic production graph", () => {
  assert.deepEqual(validateCrateBoundaries(metadata()), []);
});

test("rejects adapter sibling dependencies and heavy core dependencies", () => {
  const errors = validateCrateBoundaries(
    metadata({
      "codex-monitor-core": [dependency("rusqlite")],
      "codex-monitor-dashboard": [
        dependency("codex-monitor-core"),
        dependency("codex-monitor-storage"),
      ],
      "codex-monitor-telemetry": [
        dependency("codex-monitor-core"),
        dependency("rusqlite"),
      ],
    }),
  );
  assert.ok(
    errors.some((error) => error.includes("core 不得直接依赖 rusqlite")),
  );
  assert.ok(
    errors.some((error) =>
      error.includes("dashboard 不得依赖 workspace package"),
    ),
  );
  assert.ok(
    errors.some((error) => error.includes("telemetry 不得直接依赖 rusqlite")),
  );
});

test("ignores dev-only product integration edges", () => {
  const value = metadata();
  value.packages
    .find((pkg) => pkg.name === "codex-monitor-engine")
    .dependencies.push(dependency("codex-monitor-host", "dev"));
  assert.deepEqual(validateCrateBoundaries(value), []);
});

test("accepts the desktop-only default Rust library contract", () => {
  assert.deepEqual(
    validateDesktopOnlyConfiguration({
      manifest: '[lib]\nname = "codex_monitor_desktop"\n',
      repositoryPaths: [
        "src/lib.rs",
        "src/main.rs",
        "src-tauri/icons/android/mipmap-hdpi/ic_launcher.png",
      ],
      sourceContents: [["src/lib.rs", "pub fn run() {}\n"]],
      workflowContents: [["release.yml", "cargo build --release\n"]],
    }),
    [],
  );
});

test("rejects mobile crate types, placeholders, and workflow targets", () => {
  const errors = validateDesktopOnlyConfiguration({
    manifest:
      '[lib]\nname = "codex_monitor_desktop"\ncrate-type = ["lib", "cdylib", "staticlib"]\n',
    repositoryPaths: ["gen/android/app/build.gradle"],
    sourceContents: [
      ["src/lib.rs", "#[cfg_attr(mobile, tauri::mobile_entry_point)]\n"],
    ],
    workflowContents: [
      ["mobile.yml", "pnpm exec tauri ios build --target aarch64-apple-ios\n"],
    ],
  });
  assert.ok(errors.some((error) => error.includes("移动端 crate-type")));
  assert.ok(errors.some((error) => error.includes("移动端路径")));
  assert.ok(errors.some((error) => error.includes("移动端入口或 target")));
  assert.ok(errors.some((error) => error.includes("桌面源码")));
});

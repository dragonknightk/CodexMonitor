import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { parse } from "yaml";

import { classifyPaths, ciCategories } from "../classify-ci-changes.mjs";

const root = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  "../..",
);

test("classifies docs-only, native, dependency and release changes", () => {
  const docs = classifyPaths(["docs/architecture/system-overview.md"]);
  assert.equal(docs.docs, true);
  assert.equal(docs.rust, false);
  assert.equal(docs.native_host, false);
  assert.equal(docs.web, false);

  const native = classifyPaths(["apps/desktop/src-tauri/src/notification.rs"]);
  assert.equal(native.rust, true);
  assert.equal(native.native_host, true);

  const dependency = classifyPaths(["Cargo.lock"]);
  assert.equal(dependency.rust, true);
  assert.equal(dependency.dependency_security, true);

  for (const releasePath of [
    "scripts/release/stage-artifacts.mjs",
    "scripts/installed-artifact-smoke.mjs",
    ".github/workflows/release.yml",
    ".github/workflows/package-unsigned.yml",
  ]) {
    const release = classifyPaths([releasePath]);
    assert.equal(release.release, true, releasePath);
    assert.equal(release.tooling, false, releasePath);
  }
});

test("classification fails closed and explicit full mode is independent", () => {
  assert.throws(
    () => classifyPaths(["unowned/component.txt"]),
    /CI classification has no owner/u,
  );
  const full = classifyPaths([], { forceFull: true });
  assert.equal(full.full, true);
  for (const category of ciCategories) assert.equal(full[category], false);
});

test("CI workflow keeps risk conditions and one asset build", () => {
  const source = readFileSync(
    path.join(root, ".github/workflows/ci.yml"),
    "utf8",
  );
  const workflow = parse(source);
  const jobs = workflow.jobs;

  for (const output of [
    "rust",
    "native_host",
    "web",
    "docs",
    "dependency_security",
    "release",
  ]) {
    assert.equal(typeof jobs.classify.outputs[output], "string", output);
  }
  assert.match(jobs.rust.if, /outputs\.rust/u);
  assert.doesNotMatch(jobs.rust.if, /outputs\.docs/u);
  assert.match(jobs["native-host"].if, /outputs\.native_host/u);
  assert.deepEqual(jobs["native-host"].strategy.matrix.os, [
    "windows-latest",
    "macos-latest",
    "ubuntu-latest",
  ]);
  assert.match(jobs.frontend.if, /outputs\.web/u);
  assert.match(jobs.documentation.if, /outputs\.docs/u);
  assert.match(jobs.security.if, /outputs\.dependency_security/u);
  assert.equal(jobs.secrets.if, undefined);

  assert.equal((source.match(/pnpm --dir web build/gu) ?? []).length, 1);
  assert.doesNotMatch(source, /CODEX_MONITOR_USE_PREBUILT_ASSETS/u);
  for (const jobName of ["rust", "native-host", "frontend"]) {
    assert.ok(
      jobs[jobName].steps.some(
        (step) => step.with?.name === "dashboard-production-assets",
      ),
      jobName,
    );
  }
  const rustCommands = jobs.rust.steps
    .map((step) => step.run)
    .filter((value) => typeof value === "string")
    .join("\n");
  assert.match(rustCommands, /cargo test --workspace --lib/u);
  assert.match(
    rustCommands,
    /cargo test -p codex-monitor-engine --test product_integration/u,
  );
  assert.match(
    jobs["native-host"].steps.find(
      (step) => step.name === "Test native desktop host",
    ).run,
    /--lib/u,
  );
  assert.equal(jobs["release-contract"].steps.at(-1).run, "pnpm test:release");
});

test("PR documentation runs once with its base while main and schedules are full", () => {
  const workflow = parse(
    readFileSync(path.join(root, ".github/workflows/ci.yml"), "utf8"),
  );
  assert.deepEqual(workflow.on.push.branches, ["main"]);
  assert.ok(Array.isArray(workflow.on.schedule));
  assert.ok(workflow.on.workflow_dispatch);

  const documentationSteps = workflow.jobs.documentation.steps;
  const pullRequestChecks = documentationSteps.filter(
    (step) =>
      step.if === "github.event_name == 'pull_request'" &&
      typeof step.run === "string" &&
      step.run.includes("check-docs.mjs"),
  );
  assert.equal(pullRequestChecks.length, 1);
  assert.match(pullRequestChecks[0].run, /--base/u);
  assert.ok(
    documentationSteps.some(
      (step) =>
        step.if === "github.event_name != 'pull_request'" &&
        step.run === "pnpm docs:check",
    ),
  );
  const classifyStep = workflow.jobs.classify.steps.find(
    (step) => step.id === "classify",
  );
  assert.equal(
    classifyStep.env.FORCE_FULL,
    "${{ github.event_name != 'pull_request' }}",
  );
});

test("release packaging depends on complete verification and security audit", () => {
  const workflow = parse(
    readFileSync(path.join(root, ".github/workflows/release.yml"), "utf8"),
  );
  assert.equal(workflow.jobs.package.needs, "verify");
  const commands = workflow.jobs.verify.steps
    .map((step) => step.run)
    .filter((value) => typeof value === "string")
    .join("\n");
  assert.match(commands, /pnpm verify/u);
  assert.match(commands, /pnpm audit:security/u);
});

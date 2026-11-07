import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const root = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  "../..",
);

function readRepositoryFile(relativePath) {
  return readFileSync(path.join(root, relativePath), "utf8");
}

function profileSections(manifest) {
  return [...manifest.matchAll(/^\[profile\.([^\]]+)\]$/gmu)].map(
    (match) => match[1],
  );
}

function profileBody(manifest, profile) {
  const match = manifest.match(
    new RegExp(
      `\\[profile\\.${profile.replace(/[.*+?^${}()|[\]\\]/gu, "\\$&")}\\]\\r?\\n([\\s\\S]*?)(?=\\r?\\n\\[|$)`,
    ),
  );
  return match?.[1] ?? "";
}

test("Cargo manifest keeps daily and full-debug profiles isolated", () => {
  const manifest = readRepositoryFile("Cargo.toml");

  assert.deepEqual(profileSections(manifest), ["dev", "full-debug"]);
  assert.match(profileBody(manifest, "dev"), /^debug\s*=\s*1\s*$/mu);
  assert.match(
    profileBody(manifest, "full-debug"),
    /^inherits\s*=\s*"dev"\s*$/mu,
  );
  assert.match(
    profileBody(manifest, "full-debug"),
    /^debug\s*=\s*"full"\s*$/mu,
  );
  assert.doesNotMatch(manifest, /^\[profile\.test\]$/mu);
  assert.doesNotMatch(manifest, /^\[profile\.release\]$/mu);
});

test("daily and diagnostic commands document the intended Cargo contracts", () => {
  const commands = readRepositoryFile("docs/development/commands.md");
  const testing = readRepositoryFile("docs/development/testing.md");

  for (const command of ["cargo build", "cargo check", "cargo test"]) {
    assert.match(commands, new RegExp(`^${command}.*$`, "mu"));
  }
  assert.match(
    commands,
    /^cargo build -p codex-monitor-desktop --profile full-debug$/mu,
  );
  assert.match(
    commands,
    /^cargo test -p <受影响 crate> --profile full-debug$/mu,
  );
  for (const document of [commands, testing]) {
    assert.match(document, /(?:内置.*`dev`.*`test`|`dev`.*内置.*`test`)/su);
    assert.match(document, /常规.*无法定位问题/u);
    assert.match(document, /target\/full-debug\//u);
    assert.match(document, /cargo clean --profile full-debug/u);
  }
  assert.match(commands, /不采用 `line-tables-only`/u);
  assert.match(commands, /可复现收益/u);
  assert.match(commands, /调试器.*类型、参数和局部变量/u);
});

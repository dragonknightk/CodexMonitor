import assert from "node:assert/strict";
import {
  mkdirSync,
  mkdtempSync,
  rmSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";
import { fileURLToPath } from "node:url";

const root = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  "..",
  "..",
);
const checker = path.join(root, "scripts", "check-no-legacy-runtime.mjs");

function runGit(directory, arguments_) {
  const result = spawnSync("git", arguments_, {
    cwd: directory,
    encoding: "utf8",
    windowsHide: true,
  });
  assert.equal(result.status, 0, result.stderr);
  return result;
}

function writeRepositoryFile(directory, repoPath, contents) {
  const target = path.join(directory, ...repoPath.split("/"));
  mkdirSync(path.dirname(target), { recursive: true });
  writeFileSync(target, contents, "utf8");
  return target;
}

function createRepository(t) {
  const directory = mkdtempSync(
    path.join(os.tmpdir(), "codex-monitor-no-legacy-runtime-"),
  );
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  runGit(directory, ["init", "-q"]);
  writeRepositoryFile(
    directory,
    ".gitignore",
    ".venv/\nbuild/\ntarget/\nnode_modules/\n",
  );
  runGit(directory, ["add", ".gitignore"]);
  return directory;
}

function runChecker(directory) {
  return spawnSync(process.execPath, [checker, "--root", directory], {
    cwd: root,
    encoding: "utf8",
    windowsHide: true,
  });
}

test("accepts the native runtime and documented Python history", (t) => {
  const directory = createRepository(t);
  writeRepositoryFile(
    directory,
    "crates/native/src/main.rs",
    'fn main() { println!("native"); }\n',
  );
  writeRepositoryFile(
    directory,
    "docs/cutover.md",
    "The removed Python prototype is preserved only as documented history.\n",
  );
  writeRepositoryFile(
    directory,
    "scripts/licenses.mjs",
    'const accepted = "Python-2.0";\nvoid accepted;\n',
  );
  writeRepositoryFile(
    directory,
    ".venv/Scripts/python.exe",
    "ignored local interpreter",
  );

  const result = runChecker(directory);

  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /legacy runtime absent/u);
});

test("rejects every removed runtime tree and Python artifact", (t) => {
  const directory = createRepository(t);
  for (const [repoPath, contents] of Object.entries({
    "pyproject.toml": "[project]\nname = 'legacy'\n",
    "run.ps1": "Write-Output legacy\n",
    "scripts/check-codex-otel.ps1": "Write-Output legacy\n",
    "src/codex_monitor/server.py": "pass\n",
    "tests/legacy_contract/test_contract.py": "pass\n",
    "tests/test_server.py": "pass\n",
  })) {
    writeRepositoryFile(directory, repoPath, contents);
  }

  const result = runChecker(directory);

  assert.equal(result.status, 1);
  for (const rule of [
    "legacy-runtime-path-present",
    "legacy-runtime-tree-present",
    "python-artifact-present",
    "python-toolchain-present",
  ]) {
    assert.match(result.stderr, new RegExp(rule, "u"));
  }
  assert.equal(result.stderr.includes(directory), false);
  assert.doesNotMatch(result.stderr, /Write-Output|pass/u);
});

test("rejects Python commands and references from execution surfaces", (t) => {
  const directory = createRepository(t);
  writeRepositoryFile(
    directory,
    ".github/workflows/legacy.yml",
    "jobs:\n  legacy:\n    runs-on: ubuntu-latest\n    steps:\n      - run: python -m codex_monitor\n",
  );
  writeRepositoryFile(
    directory,
    "crates/launcher/src/main.rs",
    'fn launch() { std::process::Command::new("python3"); }\n',
  );
  writeRepositoryFile(
    directory,
    "scripts/forward.mjs",
    'spawn("py", ["-m", "codex_monitor.server"]);\n',
  );

  const result = runChecker(directory);

  assert.equal(result.status, 1);
  assert.match(result.stderr, /python-command-invocation/u);
  assert.match(result.stderr, /removed-runtime-reference/u);
  assert.equal(result.stderr.includes(directory), false);
  assert.doesNotMatch(result.stderr, /Command::new|spawn\(|codex_monitor/u);
});

test("checks untracked candidate shims before they enter the index", (t) => {
  const directory = createRepository(t);
  writeRepositoryFile(
    directory,
    "tools/bootstrap.ps1",
    "python -m codex_monitor\n",
  );

  const untracked = runGit(directory, [
    "ls-files",
    "--others",
    "--exclude-standard",
  ]).stdout;
  assert.match(untracked, /tools\/bootstrap\.ps1/u);

  const result = runChecker(directory);

  assert.equal(result.status, 1);
  assert.match(result.stderr, /tools\/bootstrap\.ps1/u);
});

test("treats a deleted tracked legacy file as absent from the candidate tree", (t) => {
  const directory = createRepository(t);
  const legacy = writeRepositoryFile(
    directory,
    "run.ps1",
    "python -m codex_monitor\n",
  );
  runGit(directory, ["add", "run.ps1"]);
  unlinkSync(legacy);
  assert.match(runGit(directory, ["status", "--short"]).stdout, /D run\.ps1/u);

  const result = runChecker(directory);

  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /legacy runtime absent/u);
});

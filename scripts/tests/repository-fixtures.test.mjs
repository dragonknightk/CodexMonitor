import assert from "node:assert/strict";
import {
  cpSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
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
const checker = path.join(root, "scripts", "check-repository-fixtures.mjs");

function runGit(directory, arguments_) {
  const result = spawnSync("git", arguments_, {
    cwd: directory,
    encoding: "utf8",
    windowsHide: true,
  });
  assert.equal(result.status, 0, result.stderr);
  return result;
}

function createRepository(t) {
  const directory = mkdtempSync(
    path.join(os.tmpdir(), "codex-monitor-repository-fixtures-"),
  );
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  runGit(directory, ["init", "-q"]);

  mkdirSync(path.join(directory, "contracts"), { recursive: true });
  cpSync(
    path.join(root, "contracts", "legacy-otlp-v1.json"),
    path.join(directory, "contracts", "legacy-otlp-v1.json"),
  );
  mkdirSync(path.join(directory, "fixtures"), { recursive: true });
  cpSync(
    path.join(root, "fixtures", "otlp"),
    path.join(directory, "fixtures", "otlp"),
    {
      recursive: true,
    },
  );
  writeFileSync(
    path.join(directory, ".gitignore"),
    ".venv/\nbuild/\n*.sqlite3\n*.jsonl\n",
    "utf8",
  );
  runGit(directory, ["add", "."]);
  return directory;
}

function runChecker(directory, ...arguments_) {
  return spawnSync(
    process.execPath,
    [checker, "--root", directory, ...arguments_],
    {
      cwd: root,
      encoding: "utf8",
      windowsHide: true,
    },
  );
}

function writeContractViolation(directory) {
  const target = path.join(directory, "contracts", "legacy-otlp-v1.json");
  const contract = JSON.parse(readFileSync(target, "utf8"));
  contract.cases[0].request.body_builder = "captured_binary";
  writeFileSync(target, `${JSON.stringify(contract, null, 2)}\n`, "utf8");
  return target;
}

function writeFixtureViolation(directory) {
  const target = path.join(
    directory,
    "fixtures",
    "otlp",
    "unknown_fields.json",
  );
  const fixture = JSON.parse(readFileSync(target, "utf8"));
  fixture.syntheticIndexRegression = { email: "person@corp.example" };
  writeFileSync(target, `${JSON.stringify(fixture, null, 2)}\n`, "utf8");
  return target;
}

test("accepts the frozen synthetic OTLP set and ignores untracked local caches", (t) => {
  const directory = createRepository(t);
  mkdirSync(path.join(directory, ".venv"), { recursive: true });
  mkdirSync(path.join(directory, "build"), { recursive: true });
  writeFileSync(
    path.join(directory, ".venv", "private.sqlite3"),
    "not-a-database",
    "utf8",
  );
  writeFileSync(
    path.join(directory, "build", "session.jsonl"),
    "synthetic-cache\n",
    "utf8",
  );

  const result = runChecker(directory);

  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /repository fixtures verified/u);
  const tracked = runGit(directory, ["ls-files"]).stdout;
  assert.doesNotMatch(tracked, /\.venv|build/u);
});

test("uses staged contract and fixture violations when worktree copies are safe", (t) => {
  const directory = createRepository(t);
  const frozenContractPath = writeContractViolation(directory);
  const fixturePath = writeFixtureViolation(directory);
  runGit(directory, [
    "add",
    "contracts/legacy-otlp-v1.json",
    "fixtures/otlp/unknown_fields.json",
  ]);

  cpSync(
    path.join(root, "contracts", "legacy-otlp-v1.json"),
    frozenContractPath,
  );
  cpSync(
    path.join(root, "fixtures", "otlp", "unknown_fields.json"),
    fixturePath,
  );

  const result = runChecker(directory);

  assert.equal(result.status, 1);
  assert.match(result.stderr, /otlp-contract-fixture-type/u);
  assert.match(result.stderr, /otlp-fixture-real-email/u);
  assert.doesNotMatch(result.stderr, /person@corp|captured_binary/u);
});

test("ignores unstaged contract and fixture violations", (t) => {
  const directory = createRepository(t);
  writeContractViolation(directory);
  writeFixtureViolation(directory);

  const changed = runGit(directory, ["diff", "--name-only"]).stdout;
  assert.match(changed, /contracts\/legacy-otlp-v1\.json/u);
  assert.match(changed, /fixtures\/otlp\/unknown_fields\.json/u);

  const result = runChecker(directory);

  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /repository fixtures verified/u);
});

test("worktree mode honors deletions and validates unstaged candidate files", (t) => {
  const directory = createRepository(t);
  const deletedReadme = path.join(directory, "fixtures", "otlp", "README.md");
  writeFileSync(deletedReadme, "synthetic fixture notes\n", "utf8");
  runGit(directory, ["add", "fixtures/otlp/README.md"]);
  rmSync(deletedReadme);

  const indexResult = runChecker(directory);
  assert.equal(indexResult.status, 1);
  assert.match(
    indexResult.stderr,
    /otlp-fixture-unregistered: fixtures\/otlp\/README\.md/u,
  );

  const deletedCandidateResult = runChecker(directory, "--worktree");
  assert.equal(deletedCandidateResult.status, 0, deletedCandidateResult.stderr);
  assert.match(deletedCandidateResult.stdout, /repository fixtures verified/u);

  writeFixtureViolation(directory);
  const unsafeCandidateResult = runChecker(directory, "--worktree");
  assert.equal(unsafeCandidateResult.status, 1);
  assert.match(unsafeCandidateResult.stderr, /otlp-fixture-real-email/u);
  assert.doesNotMatch(unsafeCandidateResult.stderr, /README\.md|person@corp/u);
});

test("rejects tracked local databases, logs, environment files, and key material", (t) => {
  const directory = createRepository(t);
  const files = {
    ".env.local": "SYNTHETIC_TOKEN=synthetic-only",
    ".envrc": "SYNTHETIC_ONLY=1",
    "local/.netrc": "machine example.invalid login synthetic",
    "local/engine.log": "synthetic log",
    "local/events.sqlite3-wal": "synthetic wal",
    "local/history.sqlite3": "synthetic sqlite",
    "local/history.sqlite3.backup": "synthetic sqlite backup",
    "local/session.jsonl": '{"synthetic":true}\n',
    "local/session.jsonl.gz": "synthetic compressed log",
    "local/signing.pfx": "synthetic key material",
  };
  for (const [repoPath, contents] of Object.entries(files)) {
    const target = path.join(directory, repoPath);
    mkdirSync(path.dirname(target), { recursive: true });
    writeFileSync(target, contents, "utf8");
  }
  runGit(directory, ["add", "-f", ...Object.keys(files)]);

  const result = runChecker(directory);

  assert.equal(result.status, 1);
  for (const rule of [
    "tracked-environment-file",
    "tracked-key-material",
    "tracked-local-database",
    "tracked-local-log",
  ]) {
    assert.match(result.stderr, new RegExp(rule, "u"));
  }
  assert.equal(result.stderr.includes(directory), false);
  assert.doesNotMatch(result.stderr, /SYNTHETIC_TOKEN|synthetic key material/u);
});

test("rejects real locations, external identities, and unregistered fixture markers", (t) => {
  const directory = createRepository(t);
  const fixturePath = path.join(
    directory,
    "fixtures",
    "otlp",
    "unknown_fields.json",
  );
  const fixture = JSON.parse(readFileSync(fixturePath, "utf8"));
  fixture.syntheticPrivacyRegression = {
    id: "external-account-42",
    email: "person@corp.example",
    url: "https://service.example.com/private",
    windowsPath: "C:\\Users\\Example\\private.log",
    posixPath: "/home/example/private.log",
    fileUrl: "file:///Users/example/private.log",
    prompt: "synthetic-unregistered-prompt.example.invalid",
    uuid: "018f0c9a-7b2d-7a14-b123-123456789abc",
  };
  writeFileSync(fixturePath, `${JSON.stringify(fixture, null, 2)}\n`, "utf8");
  runGit(directory, ["add", "fixtures/otlp/unknown_fields.json"]);

  const result = runChecker(directory);

  assert.equal(result.status, 1);
  for (const rule of [
    "otlp-fixture-absolute-path",
    "otlp-fixture-non-synthetic-id",
    "otlp-fixture-real-email",
    "otlp-fixture-real-url",
    "otlp-fixture-unregistered-marker",
    "otlp-fixture-uuid",
  ]) {
    assert.match(result.stderr, new RegExp(rule, "u"));
  }
  assert.equal(result.stderr.includes(directory), false);
  assert.doesNotMatch(
    result.stderr,
    /person@corp|service\.example\.com|C:\\Users|external-account/u,
  );
});

test("rejects a contract case with an unknown fixture builder", (t) => {
  const directory = createRepository(t);
  const frozenContractPath = path.join(
    directory,
    "contracts",
    "legacy-otlp-v1.json",
  );
  const contract = JSON.parse(readFileSync(frozenContractPath, "utf8"));
  contract.cases[0].request.body_builder = "captured_binary";
  writeFileSync(
    frozenContractPath,
    `${JSON.stringify(contract, null, 2)}\n`,
    "utf8",
  );
  runGit(directory, ["add", "contracts/legacy-otlp-v1.json"]);

  const result = runChecker(directory);

  assert.equal(result.status, 1);
  assert.match(
    result.stderr,
    /otlp-contract-fixture-type: contracts\/legacy-otlp-v1\.json/u,
  );
});

test("rejects backslash traversal in a staged fixture source path", (t) => {
  const directory = createRepository(t);
  const frozenContractPath = path.join(
    directory,
    "contracts",
    "legacy-otlp-v1.json",
  );
  const contract = JSON.parse(readFileSync(frozenContractPath, "utf8"));
  contract.cases[0].request.source_fixture =
    "fixtures/otlp/synthetic\\..\\captured.json";
  writeFileSync(
    frozenContractPath,
    `${JSON.stringify(contract, null, 2)}\n`,
    "utf8",
  );
  runGit(directory, ["add", "contracts/legacy-otlp-v1.json"]);

  const result = runChecker(directory);

  assert.equal(result.status, 1);
  assert.match(
    result.stderr,
    /otlp-contract-source-path: contracts\/legacy-otlp-v1\.json/u,
  );
  assert.doesNotMatch(result.stderr, /synthetic\\|captured\.json/u);
});

test("requires OTLP JSON sources to contain an object", (t) => {
  const directory = createRepository(t);
  const fixturePath = path.join(
    directory,
    "fixtures",
    "otlp",
    "response_completed.json",
  );
  writeFileSync(fixturePath, "[]\n", "utf8");
  runGit(directory, ["add", "fixtures/otlp/response_completed.json"]);

  const result = runChecker(directory);

  assert.equal(result.status, 1);
  assert.match(
    result.stderr,
    /otlp-fixture-json-invalid: fixtures\/otlp\/response_completed\.json/u,
  );
});

test("rejects an OTLP fixture that is not registered by the frozen contract", (t) => {
  const directory = createRepository(t);
  const extra = path.join(directory, "fixtures", "otlp", "captured.bin");
  writeFileSync(extra, "synthetic-only", "utf8");
  runGit(directory, ["add", "fixtures/otlp/captured.bin"]);

  const result = runChecker(directory);

  assert.equal(result.status, 1);
  assert.match(
    result.stderr,
    /otlp-fixture-unregistered: fixtures\/otlp\/captured\.bin/u,
  );
  assert.equal(result.stderr.includes(directory), false);
});

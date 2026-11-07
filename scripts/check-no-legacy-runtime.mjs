import { lstatSync, readFileSync } from "node:fs";
import path from "node:path";
import process from "node:process";
import { spawnSync } from "node:child_process";
import { fileURLToPath, pathToFileURL } from "node:url";
import { TextDecoder } from "node:util";

const scriptPath = fileURLToPath(import.meta.url);
const defaultRoot = path.resolve(path.dirname(scriptPath), "..");
const maximumExecutionSurfaceBytes = 2 * 1024 * 1024;

const excludedScannerPaths = new Set([
  "scripts/check-no-legacy-runtime.mjs",
  "scripts/tests/no-legacy-runtime.test.mjs",
]);

const forbiddenExactPaths = new Set([
  "pyproject.toml",
  "run.ps1",
  "scripts/check-codex-otel.ps1",
  "scripts/install-codex-otel-snippet.ps1",
  "scripts/smoke-test-json.ps1",
  "tests/test_server.py",
]);

const forbiddenPrefixes = ["src/codex_monitor/", "tests/legacy_contract/"];
const pythonToolchainFiles = new Set([
  ".python-version",
  "pipfile",
  "pipfile.lock",
  "poetry.lock",
  "pyproject.toml",
  "setup.cfg",
  "setup.py",
  "tox.ini",
  "uv.lock",
]);

const executableExtensions = new Set([
  ".bash",
  ".bat",
  ".cjs",
  ".cmd",
  ".fish",
  ".js",
  ".mjs",
  ".ps1",
  ".psm1",
  ".rs",
  ".sh",
  ".ts",
  ".tsx",
  ".zsh",
]);

const configurationExtensions = new Set([".json", ".toml", ".yaml", ".yml"]);
const rootExecutionConfigs = new Set([
  ".gitattributes",
  ".gitmodules",
  ".npmrc",
  "Cargo.toml",
  "package.json",
  "pnpm-workspace.yaml",
]);

const removedRuntimeReferencePattern =
  /(?:src[\\/]codex_monitor|codex_monitor\.server|tests[\\/]legacy_contract|pyproject\.toml|(?:^|[\\/])run\.ps1|check-codex-otel\.ps1|install-codex-otel-snippet\.ps1|smoke-test-json\.ps1|-m\s+codex_monitor\b)/iu;
const quotedPythonToolPattern =
  /["'`](?:python(?:3(?:\.\d+)*)?|py|pip(?:3)?|pytest)(?:\.exe)?(?:["'`]|\s)/iu;
const barePythonCommandPattern =
  /(?:^|[\r\n])\s*(?:(?:-\s*)?(?:command|run|script)\s*:\s*|&\s*)?(?:python(?:3(?:\.\d+)*)?|py|pip(?:3)?|pytest)(?:\.exe)?\b/imu;
const chainedPythonCommandPattern =
  /(?:&&|\|\||;|\|)\s*(?:python(?:3(?:\.\d+)*)?|py|pip(?:3)?|pytest)(?:\.exe)?\b/iu;
const pythonExecutablePathPattern =
  /[\\/](?:python(?:3(?:\.\d+)*)?|py|pip(?:3)?|pytest)\.exe\b/iu;
const pythonShebangPattern =
  /^#![^\r\n]*\b(?:python(?:3(?:\.\d+)*)?|py)(?:\.exe)?\b/imu;
const pythonEnvironmentRunnerPattern =
  /\b(?:hatch|nox|pdm|pipenv|poetry|tox|uv)\s+run\b/iu;

function normalizeRepoPath(value) {
  return value.replaceAll("\\", "/").replace(/^\.\//u, "");
}

function parseArguments(argv) {
  let root = defaultRoot;
  for (let index = 0; index < argv.length; index += 1) {
    if (argv[index] !== "--root") throw new Error("invalid_arguments");
    const value = argv[index + 1];
    if (value === undefined || value.startsWith("--")) {
      throw new Error("invalid_arguments");
    }
    root = path.resolve(value);
    index += 1;
  }
  return { root };
}

function candidatePath(root, repoPath) {
  const resolved = path.resolve(root, ...repoPath.split("/"));
  const relative = normalizeRepoPath(path.relative(root, resolved));
  if (
    relative === "" ||
    relative === ".." ||
    relative.startsWith("../") ||
    path.isAbsolute(relative)
  ) {
    throw new Error("candidate_path_invalid");
  }
  return resolved;
}

export function listCandidateInputs(root) {
  const result = spawnSync(
    "git",
    [
      "-c",
      "core.quotepath=false",
      "ls-files",
      "--cached",
      "--others",
      "--exclude-standard",
      "-z",
    ],
    { cwd: root, encoding: "buffer", windowsHide: true },
  );
  if (result.error || result.status !== 0 || !Buffer.isBuffer(result.stdout)) {
    throw new Error("candidate_input_enumeration_failed");
  }

  let output;
  try {
    output = new TextDecoder("utf-8", { fatal: true }).decode(result.stdout);
  } catch {
    throw new Error("candidate_input_enumeration_failed");
  }

  const inputs = new Set();
  for (const value of output.split("\0").filter(Boolean)) {
    const repoPath = normalizeRepoPath(value);
    const absolutePath = candidatePath(root, repoPath);
    let entry;
    try {
      entry = lstatSync(absolutePath);
    } catch (error) {
      if (
        error instanceof Error &&
        "code" in error &&
        error.code === "ENOENT"
      ) {
        continue;
      }
      throw new Error("candidate_input_enumeration_failed");
    }
    if (entry.isFile() || entry.isSymbolicLink()) inputs.add(repoPath);
  }
  return [...inputs].sort((left, right) => left.localeCompare(right, "en"));
}

function isExecutionSurface(repoPath) {
  if (excludedScannerPaths.has(repoPath)) return false;
  const extension = path.posix.extname(repoPath).toLowerCase();
  if (executableExtensions.has(extension)) {
    return !["contracts/", "docs/", "fixtures/"].some((prefix) =>
      repoPath.startsWith(prefix),
    );
  }
  if (!configurationExtensions.has(extension)) {
    return rootExecutionConfigs.has(repoPath);
  }
  return (
    rootExecutionConfigs.has(repoPath) ||
    repoPath.startsWith(".github/") ||
    repoPath.startsWith("apps/") ||
    repoPath.startsWith("scripts/")
  );
}

function readExecutionSurface(root, repoPath) {
  const absolutePath = candidatePath(root, repoPath);
  const metadata = lstatSync(absolutePath);
  if (metadata.isSymbolicLink()) throw new Error("execution_surface_symlink");
  if (!metadata.isFile() || metadata.size > maximumExecutionSurfaceBytes) {
    throw new Error("execution_surface_unreadable");
  }
  return new TextDecoder("utf-8", { fatal: true }).decode(
    readFileSync(absolutePath),
  );
}

function pythonArtifactRule(repoPath) {
  const basename = path.posix.basename(repoPath);
  const lower = basename.toLowerCase();
  if (/\.(?:py|pyc|pyd|pyi|pyo|pyw)$/u.test(lower)) {
    return "python-artifact-present";
  }
  if (
    pythonToolchainFiles.has(lower) ||
    /^requirements(?:-[A-Za-z0-9._-]+)?\.txt$/iu.test(basename)
  ) {
    return "python-toolchain-present";
  }
  return null;
}

export function validateNoLegacyRuntime(root, candidateInputs) {
  const issues = new Map();
  const addIssue = (rule, repoPath) => {
    issues.set(`${repoPath}\0${rule}`, { path: repoPath, rule });
  };

  for (const repoPath of candidateInputs) {
    if (forbiddenExactPaths.has(repoPath)) {
      addIssue("legacy-runtime-path-present", repoPath);
    }
    if (forbiddenPrefixes.some((prefix) => repoPath.startsWith(prefix))) {
      addIssue("legacy-runtime-tree-present", repoPath);
    }
    const artifactRule = pythonArtifactRule(repoPath);
    if (artifactRule !== null) addIssue(artifactRule, repoPath);

    if (!isExecutionSurface(repoPath)) continue;
    let source;
    try {
      source = readExecutionSurface(root, repoPath);
    } catch (error) {
      addIssue(
        error instanceof Error && error.message === "execution_surface_symlink"
          ? "execution-surface-symlink"
          : "execution-surface-unreadable",
        repoPath,
      );
      continue;
    }
    if (removedRuntimeReferencePattern.test(source)) {
      addIssue("removed-runtime-reference", repoPath);
    }
    if (
      quotedPythonToolPattern.test(source) ||
      barePythonCommandPattern.test(source) ||
      chainedPythonCommandPattern.test(source) ||
      pythonExecutablePathPattern.test(source) ||
      pythonShebangPattern.test(source) ||
      pythonEnvironmentRunnerPattern.test(source)
    ) {
      addIssue("python-command-invocation", repoPath);
    }
  }

  return [...issues.values()].sort(
    (left, right) =>
      left.path.localeCompare(right.path, "en") ||
      left.rule.localeCompare(right.rule, "en"),
  );
}

function main() {
  let root;
  let candidateInputs;
  try {
    ({ root } = parseArguments(process.argv.slice(2)));
    candidateInputs = listCandidateInputs(root);
  } catch (error) {
    const rule =
      error instanceof Error && error.message === "invalid_arguments"
        ? "invalid-arguments"
        : "candidate-input-enumeration";
    process.stderr.write(`legacy runtime validation failed\n${rule}: .\n`);
    process.exitCode = 1;
    return;
  }

  const issues = validateNoLegacyRuntime(root, candidateInputs);
  if (issues.length > 0) {
    process.stderr.write(
      `legacy runtime validation failed\n${issues.map(({ path: repoPath, rule }) => `${rule}: ${repoPath}`).join("\n")}\n`,
    );
    process.exitCode = 1;
    return;
  }
  process.stdout.write(
    `legacy runtime absent: ${candidateInputs.length} candidate files inspected\n`,
  );
}

if (
  process.argv[1] !== undefined &&
  import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href
) {
  main();
}

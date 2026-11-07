import path from "node:path";
import process from "node:process";
import { spawnSync } from "node:child_process";
import { lstatSync, readFileSync } from "node:fs";
import { fileURLToPath, pathToFileURL } from "node:url";
import { TextDecoder } from "node:util";

const scriptPath = fileURLToPath(import.meta.url);
const defaultRoot = path.resolve(path.dirname(scriptPath), "..");
const contractPath = "contracts/legacy-otlp-v1.json";
const fixtureRoot = "fixtures/otlp/";
const maximumTrackedBlobBytes = 1024 * 1024;

const knownBodyBuilderSuffixes = new Map([
  ["hex", [".protobuf.hex"]],
  ["otlp_json", [".json"]],
  ["otlp_protobuf", [".json"]],
  ["otlp_protobuf_unknown_wire", [".json"]],
  ["raw_text", [".json", ".gzip.txt"]],
]);
const knownFixtureSuffixes = new Set(
  [...knownBodyBuilderSuffixes.values()].flat(),
);
const safeFixtureBasenamePattern = /^[A-Za-z0-9][A-Za-z0-9._-]*$/u;

const allowedEnvironmentTemplates = new Set([
  ".env.example",
  ".env.sample",
  ".env.template",
]);

const keyMaterialExtensions = [
  ".cer",
  ".crt",
  ".jks",
  ".kdbx",
  ".key",
  ".keystore",
  ".mobileprovision",
  ".p12",
  ".pem",
  ".pfx",
  ".provisionprofile",
];

const emailPattern = /[A-Za-z0-9._%+-]+@([A-Za-z0-9.-]+)/gu;
const urlPattern = /\bhttps?:\/\/[^\s"'<>`\\]+/giu;
const uuidPattern =
  /\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b/giu;
const registeredMarkerPattern =
  /[A-Za-z0-9][A-Za-z0-9._%+@-]*(?:@|\.)example\.invalid/giu;
const fileUrlPattern = /\bfile:\/\//giu;
const windowsAbsolutePathPattern =
  /(?:^|[\s"'(=])(?:[A-Za-z]:[\\/]|\\\\[^\\/\s]+[\\/])/gmu;
const posixAbsolutePathPattern =
  /(?:^|[\s"'(=])\/(?!\/)[A-Za-z0-9._-]+(?:\/[A-Za-z0-9._-]+)+/gmu;

function normalizeRepoPath(value) {
  return value.replaceAll("\\", "/").replace(/^\.\//u, "");
}

function parseArguments(argv) {
  let root = defaultRoot;
  let rootSeen = false;
  let worktree = false;
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === "--worktree") {
      if (worktree) throw new Error("invalid_arguments");
      worktree = true;
      continue;
    }
    if (argument !== "--root" || rootSeen) {
      throw new Error("invalid_arguments");
    }
    const value = argv[index + 1];
    if (value === undefined || value.startsWith("--")) {
      throw new Error("invalid_arguments");
    }
    root = path.resolve(value);
    rootSeen = true;
    index += 1;
  }
  return { root, worktree };
}

export function listTrackedInputs(root) {
  const result = spawnSync(
    "git",
    ["-c", "core.quotepath=false", "ls-files", "--stage", "-z"],
    {
      cwd: root,
      encoding: "buffer",
      windowsHide: true,
    },
  );
  if (result.error || result.status !== 0 || !Buffer.isBuffer(result.stdout)) {
    throw new Error("tracked_input_enumeration_failed");
  }

  let output;
  try {
    output = new TextDecoder("utf-8", { fatal: true }).decode(result.stdout);
  } catch {
    throw new Error("tracked_input_enumeration_failed");
  }

  return output
    .split("\0")
    .filter(Boolean)
    .map((record) => {
      const tab = record.indexOf("\t");
      if (tab === -1) throw new Error("tracked_input_enumeration_failed");
      const metadata = record.slice(0, tab).split(" ");
      if (metadata.length !== 3) {
        throw new Error("tracked_input_enumeration_failed");
      }
      const [mode, objectId, stage] = metadata;
      if (
        !/^[0-7]{6}$/u.test(mode) ||
        !/^[0-9a-f]{40}(?:[0-9a-f]{24})?$/u.test(objectId) ||
        !/^[0-3]$/u.test(stage)
      ) {
        throw new Error("tracked_input_enumeration_failed");
      }
      return {
        mode,
        objectId,
        path: normalizeRepoPath(record.slice(tab + 1)),
        stage,
      };
    });
}

export function listWorktreeInputs(root) {
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
    {
      cwd: root,
      encoding: "buffer",
      windowsHide: true,
    },
  );
  if (result.error || result.status !== 0 || !Buffer.isBuffer(result.stdout)) {
    throw new Error("tracked_input_enumeration_failed");
  }

  let output;
  try {
    output = new TextDecoder("utf-8", { fatal: true }).decode(result.stdout);
  } catch {
    throw new Error("tracked_input_enumeration_failed");
  }

  const inputs = [];
  for (const record of output.split("\0").filter(Boolean)) {
    const repoPath = normalizeRepoPath(record);
    const absolutePath = path.resolve(root, repoPath);
    const relativePath = path.relative(root, absolutePath);
    if (
      repoPath.length === 0 ||
      path.isAbsolute(relativePath) ||
      relativePath === ".." ||
      relativePath.startsWith(`..${path.sep}`)
    ) {
      throw new Error("tracked_input_enumeration_failed");
    }

    let status;
    try {
      status = lstatSync(absolutePath);
    } catch (error) {
      if (error?.code === "ENOENT") continue;
      throw new Error("tracked_input_enumeration_failed");
    }
    const mode = status.isFile()
      ? "100644"
      : status.isSymbolicLink()
        ? "120000"
        : "000000";
    inputs.push({ mode, path: repoPath, source: "worktree", stage: "0" });
  }
  return inputs;
}

function readTrackedBlob(root, trackedInput) {
  if (trackedInput.source === "worktree") {
    const absolutePath = path.resolve(root, trackedInput.path);
    const relativePath = path.relative(root, absolutePath);
    if (
      path.isAbsolute(relativePath) ||
      relativePath === ".." ||
      relativePath.startsWith(`..${path.sep}`)
    ) {
      throw new Error("tracked_blob_read_failed");
    }
    let status;
    try {
      status = lstatSync(absolutePath);
    } catch {
      throw new Error("tracked_blob_read_failed");
    }
    if (!status.isFile() || status.size > maximumTrackedBlobBytes) {
      throw new Error("tracked_blob_read_failed");
    }
    const contents = readFileSync(absolutePath);
    if (contents.length > maximumTrackedBlobBytes) {
      throw new Error("tracked_blob_read_failed");
    }
    return contents;
  }

  const result = spawnSync("git", ["cat-file", "blob", trackedInput.objectId], {
    cwd: root,
    encoding: "buffer",
    env: { ...process.env, GIT_NO_LAZY_FETCH: "1" },
    maxBuffer: maximumTrackedBlobBytes,
    windowsHide: true,
  });
  if (result.error || result.status !== 0 || !Buffer.isBuffer(result.stdout)) {
    throw new Error("tracked_blob_read_failed");
  }
  return result.stdout;
}

function readTrackedText(root, trackedInput) {
  return new TextDecoder("utf-8", { fatal: true }).decode(
    readTrackedBlob(root, trackedInput),
  );
}

function forbiddenTrackedFileRule(repoPath) {
  const lower = repoPath.toLowerCase();
  const basename = path.posix.basename(lower);

  if (
    /\.(?:sqlite3?|db)(?:(?:-(?:journal|shm|wal))|\.(?:bak|backup|copy))?$/u.test(
      lower,
    ) ||
    /(?:^|\/)[^/]+-(?:shm|wal)$/u.test(lower) ||
    /\.(?:shm|wal)$/u.test(lower)
  ) {
    return "tracked-local-database";
  }
  if (
    /\.(?:jsonl|ndjson)(?:\.(?:bz2|gz|xz|zst))?$/u.test(lower) ||
    /\.log(?:\.\d+)?(?:\.(?:bz2|gz|xz|zst))?$/u.test(lower)
  ) {
    return "tracked-local-log";
  }
  if (
    (basename === ".env" ||
      basename === ".envrc" ||
      basename.startsWith(".env.")) &&
    !allowedEnvironmentTemplates.has(basename)
  ) {
    return "tracked-environment-file";
  }
  if (
    keyMaterialExtensions.some((extension) => lower.endsWith(extension)) ||
    /(?:^|\/)id_(?:dsa|ecdsa|ed25519|rsa)$/u.test(lower) ||
    basename === ".authinfo" ||
    basename === ".netrc" ||
    lower.endsWith(".keychain-db")
  ) {
    return "tracked-key-material";
  }
  return null;
}

function stringArray(value) {
  return Array.isArray(value) &&
    value.every((entry) => typeof entry === "string")
    ? value
    : null;
}

function isKnownSourceFixturePath(value) {
  if (typeof value !== "string" || !value.startsWith(fixtureRoot)) return false;
  const basename = value.slice(fixtureRoot.length);
  return (
    safeFixtureBasenamePattern.test(basename) &&
    [...knownFixtureSuffixes].some((suffix) => basename.endsWith(suffix))
  );
}

function loadContract(root, trackedInputs, trackedPaths, addIssue) {
  const contractEntry = trackedInputs.find(
    (entry) => entry.path === contractPath && entry.stage === "0",
  );
  if (!trackedPaths.has(contractPath) || contractEntry === undefined) {
    addIssue("otlp-contract-missing", contractPath);
    return null;
  }
  if (contractEntry.mode !== "100644" && contractEntry.mode !== "100755") {
    addIssue("otlp-contract-non-file", contractPath);
    return null;
  }

  let contract;
  try {
    contract = JSON.parse(readTrackedText(root, contractEntry));
  } catch {
    addIssue("otlp-contract-invalid", contractPath);
    return null;
  }

  const identifiers = stringArray(contract?.privacy?.synthetic_identifiers);
  const forbiddenMarkers = stringArray(
    contract?.privacy?.forbidden_storage_markers,
  );
  if (
    identifiers === null ||
    forbiddenMarkers === null ||
    !Array.isArray(contract?.cases)
  ) {
    addIssue("otlp-contract-invalid", contractPath);
    return null;
  }

  for (const value of [...identifiers, ...forbiddenMarkers]) {
    if (!value.startsWith("synthetic-")) {
      addIssue("otlp-contract-non-synthetic", contractPath);
    }
  }

  const registeredMarkers = new Set([...identifiers, ...forbiddenMarkers]);
  const sourceFixtures = new Set();
  const requiredJsonFixtures = new Set();
  for (const contractCase of contract.cases) {
    const request = contractCase?.request;
    const sourceFixture = request?.source_fixture;
    if (!isKnownSourceFixturePath(sourceFixture)) {
      addIssue("otlp-contract-source-path", contractPath);
      continue;
    }
    sourceFixtures.add(sourceFixture);
    const suffixes = knownBodyBuilderSuffixes.get(request.body_builder);
    if (
      suffixes === undefined ||
      !suffixes.some((suffix) => sourceFixture.endsWith(suffix))
    ) {
      addIssue("otlp-contract-fixture-type", contractPath);
    }
    if (String(request.body_builder).startsWith("otlp_")) {
      requiredJsonFixtures.add(sourceFixture);
    }

    const embeddedMarkers = stringArray(request.embedded_forbidden_markers);
    if (embeddedMarkers === null) {
      addIssue("otlp-contract-invalid", contractPath);
      continue;
    }
    for (const marker of embeddedMarkers) {
      if (!forbiddenMarkers.includes(marker)) {
        addIssue("otlp-contract-marker-registration", contractPath);
      }
    }
    if (
      request.unknown_wire_value !== undefined &&
      !forbiddenMarkers.includes(request.unknown_wire_value)
    ) {
      addIssue("otlp-contract-marker-registration", contractPath);
    }
  }

  return { registeredMarkers, requiredJsonFixtures, sourceFixtures };
}

function isIdentifierProperty(key) {
  return /^(?:id|uuid|[A-Za-z0-9.]+(?:_id|Id|ID))$/u.test(key);
}

function isIdentifierAttribute(key) {
  return /(?:^|[._])(?:account|conversation|response|session|thread|user)[._]?id$/iu.test(
    key,
  );
}

function isSensitiveProperty(key) {
  return /^(?:arguments|authorization|command|cookie|encrypted_reasoning|output|prompt|reasoning_summary|tool_output)$/iu.test(
    key,
  );
}

function isSensitiveAttribute(key) {
  return /(?:prompt|response\.text|tool\.|reasoning\.|command|workspace\.path|account\.id|user\.email|api\.key|cookie|authorization|future\.)/iu.test(
    key,
  );
}

function scalarString(value) {
  if (typeof value === "string") return value;
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    return null;
  }
  for (const key of ["stringValue", "string_value"]) {
    if (typeof value[key] === "string") return value[key];
  }
  return null;
}

function inspectJsonValue(value, repoPath, registeredMarkers, addIssue) {
  if (typeof value === "string") {
    inspectFixtureText(value, repoPath, registeredMarkers, addIssue);
    const trimmed = value.trim();
    if (trimmed.startsWith("{") || trimmed.startsWith("[")) {
      try {
        inspectJsonValue(
          JSON.parse(trimmed),
          repoPath,
          registeredMarkers,
          addIssue,
        );
      } catch {
        // An OTLP body may intentionally contain malformed JSON. Text-level
        // privacy rules still run over the enclosing fixture.
      }
    }
    return;
  }
  if (value === null || typeof value !== "object") return;
  if (Array.isArray(value)) {
    for (const entry of value) {
      inspectJsonValue(entry, repoPath, registeredMarkers, addIssue);
    }
    return;
  }

  if (typeof value.key === "string") {
    const attributeValue = scalarString(value.value);
    if (
      attributeValue !== null &&
      attributeValue !== "" &&
      isIdentifierAttribute(value.key) &&
      !attributeValue.startsWith("synthetic-")
    ) {
      addIssue("otlp-fixture-non-synthetic-id", repoPath);
    }
    if (
      attributeValue !== null &&
      isSensitiveAttribute(value.key) &&
      !registeredMarkers.has(attributeValue)
    ) {
      addIssue("otlp-fixture-unregistered-marker", repoPath);
    }
  }

  for (const [key, entry] of Object.entries(value)) {
    if (
      typeof entry === "string" &&
      entry !== "" &&
      isIdentifierProperty(key) &&
      !entry.startsWith("synthetic-")
    ) {
      addIssue("otlp-fixture-non-synthetic-id", repoPath);
    }
    if (
      typeof entry === "string" &&
      isSensitiveProperty(key) &&
      !registeredMarkers.has(entry)
    ) {
      addIssue("otlp-fixture-unregistered-marker", repoPath);
    }
    inspectJsonValue(entry, repoPath, registeredMarkers, addIssue);
  }
}

function inspectFixtureText(text, repoPath, registeredMarkers, addIssue) {
  windowsAbsolutePathPattern.lastIndex = 0;
  posixAbsolutePathPattern.lastIndex = 0;
  fileUrlPattern.lastIndex = 0;
  if (
    windowsAbsolutePathPattern.test(text) ||
    posixAbsolutePathPattern.test(text) ||
    fileUrlPattern.test(text)
  ) {
    addIssue("otlp-fixture-absolute-path", repoPath);
  }

  emailPattern.lastIndex = 0;
  for (const match of text.matchAll(emailPattern)) {
    if (match[1].toLowerCase() !== "example.invalid") {
      addIssue("otlp-fixture-real-email", repoPath);
    }
  }

  urlPattern.lastIndex = 0;
  for (const match of text.matchAll(urlPattern)) {
    try {
      const hostname = new URL(match[0]).hostname.toLowerCase();
      if (
        hostname !== "example.invalid" &&
        !hostname.endsWith(".example.invalid")
      ) {
        addIssue("otlp-fixture-real-url", repoPath);
      }
    } catch {
      addIssue("otlp-fixture-real-url", repoPath);
    }
  }

  uuidPattern.lastIndex = 0;
  if (uuidPattern.test(text)) addIssue("otlp-fixture-uuid", repoPath);

  registeredMarkerPattern.lastIndex = 0;
  for (const match of text.matchAll(registeredMarkerPattern)) {
    if (!registeredMarkers.has(match[0])) {
      addIssue("otlp-fixture-unregistered-marker", repoPath);
    }
  }
}

export function validateRepositoryFixtures(root, trackedInputs) {
  const issues = new Map();
  const addIssue = (rule, repoPath) => {
    const normalized = normalizeRepoPath(repoPath);
    issues.set(`${normalized}\0${rule}`, { path: normalized, rule });
  };
  const trackedPaths = new Set(
    trackedInputs
      .filter((entry) => entry.stage === "0")
      .map((entry) => entry.path),
  );

  for (const entry of trackedInputs) {
    if (entry.stage !== "0") {
      addIssue("tracked-index-conflict", entry.path);
      continue;
    }
    const rule = forbiddenTrackedFileRule(entry.path);
    if (rule !== null) addIssue(rule, entry.path);
  }

  const contract = loadContract(root, trackedInputs, trackedPaths, addIssue);
  if (contract !== null) {
    const allowedFixtures = new Set(contract.sourceFixtures);
    for (const sourceFixture of contract.sourceFixtures) {
      if (!trackedPaths.has(sourceFixture)) {
        addIssue("otlp-fixture-missing", sourceFixture);
      }
    }

    for (const entry of trackedInputs) {
      if (entry.stage !== "0") continue;
      if (!entry.path.startsWith(fixtureRoot)) continue;
      if (!allowedFixtures.has(entry.path)) {
        addIssue("otlp-fixture-unregistered", entry.path);
        continue;
      }
      if (entry.mode !== "100644" && entry.mode !== "100755") {
        addIssue("otlp-fixture-non-file", entry.path);
        continue;
      }

      let text;
      try {
        text = readTrackedText(root, entry);
      } catch {
        addIssue("otlp-fixture-unreadable", entry.path);
        continue;
      }
      inspectFixtureText(
        text,
        entry.path,
        contract.registeredMarkers,
        addIssue,
      );

      if (entry.path.endsWith(".json")) {
        try {
          const parsed = JSON.parse(text);
          if (
            contract.requiredJsonFixtures.has(entry.path) &&
            (parsed === null ||
              typeof parsed !== "object" ||
              Array.isArray(parsed))
          ) {
            addIssue("otlp-fixture-json-invalid", entry.path);
          }
          inspectJsonValue(
            parsed,
            entry.path,
            contract.registeredMarkers,
            addIssue,
          );
        } catch {
          if (contract.requiredJsonFixtures.has(entry.path)) {
            addIssue("otlp-fixture-json-invalid", entry.path);
          }
        }
      }
      if (entry.path.endsWith(".protobuf.hex")) {
        const compact = text.replaceAll(/\s/gu, "");
        if (
          compact.length === 0 ||
          compact.length % 2 !== 0 ||
          !/^[0-9a-f]+$/iu.test(compact)
        ) {
          addIssue("otlp-fixture-hex-invalid", entry.path);
        }
      }
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
  let worktree;
  let trackedInputs;
  try {
    ({ root, worktree } = parseArguments(process.argv.slice(2)));
    trackedInputs = worktree
      ? listWorktreeInputs(root)
      : listTrackedInputs(root);
  } catch (error) {
    const rule =
      error instanceof Error && error.message === "invalid_arguments"
        ? "invalid-arguments"
        : "tracked-input-enumeration";
    process.stderr.write(`repository fixture validation failed\n${rule}: .\n`);
    process.exitCode = 1;
    return;
  }

  const issues = validateRepositoryFixtures(root, trackedInputs);
  if (issues.length > 0) {
    process.stderr.write(
      `repository fixture validation failed\n${issues.map(({ path: repoPath, rule }) => `${rule}: ${repoPath}`).join("\n")}\n`,
    );
    process.exitCode = 1;
    return;
  }

  const fixtureCount = trackedInputs.filter((entry) =>
    entry.path.startsWith(fixtureRoot),
  ).length;
  const inputKind = worktree ? "worktree inputs" : "tracked inputs";
  process.stdout.write(
    `repository fixtures verified: ${trackedInputs.length} ${inputKind}, ${fixtureCount} OTLP fixture files\n`,
  );
}

if (
  process.argv[1] !== undefined &&
  import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href
) {
  main();
}

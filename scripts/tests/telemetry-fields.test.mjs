import assert from "node:assert/strict";
import {
  appendFileSync,
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

import { parse, stringify } from "yaml";

const root = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  "..",
  "..",
);
const generatorPath = path.join(
  root,
  "scripts",
  "generate-telemetry-fields.mjs",
);
const authorityPath = path.join(root, "docs", "_meta", "telemetry-fields.yaml");
const persistenceManifestPath = path.join(
  root,
  "crates",
  "codex-monitor-core",
  "telemetry-persistence-fields.yaml",
);

function stableCompare(left, right) {
  return left < right ? -1 : left > right ? 1 : 0;
}

function loadCatalog() {
  return parse(readFileSync(authorityPath, "utf8"));
}

function temporaryDirectory(t) {
  const directory = mkdtempSync(
    path.join(os.tmpdir(), "codex-monitor-telemetry-fields-"),
  );
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  return directory;
}

function writeCatalog(directory, catalog) {
  const catalogPath = path.join(directory, "telemetry-fields.yaml");
  writeFileSync(catalogPath, stringify(catalog), "utf8");
  return catalogPath;
}

function runGenerator(arguments_) {
  return spawnSync(process.execPath, [generatorPath, ...arguments_], {
    cwd: root,
    encoding: "utf8",
  });
}

function persistedFieldIds(catalog) {
  return catalog.fields
    .filter((field) => field.persisted)
    .map((field) => field.id)
    .sort(stableCompare);
}

function fieldById(catalog, fieldId) {
  const field = catalog.fields.find((candidate) => candidate.id === fieldId);
  assert.ok(field, `missing field ${fieldId}`);
  return field;
}

function sourceById(field, sourceId) {
  const source = field.sources.find(
    (candidate) => candidate.source_id === sourceId,
  );
  assert.ok(source, `missing source ${sourceId} for ${field.id}`);
  return source;
}

test("catalog preserves canonical ids, source ids, and review-approved domain types", () => {
  const catalog = loadCatalog();

  assert.deepEqual(fieldById(catalog, "conversation.id"), {
    id: "conversation.id",
    entities: ["Conversation", "Turn", "ModelCall", "ModelCallObservation"],
    domain_type: "ConversationId",
    nullable: false,
    sources: [
      {
        source_id: "domain-engine",
        mappings: ["derived:canonical_conversation_identity"],
      },
    ],
    classification: "pseudonymous_identifier",
    persisted: true,
    retention: "structured-monitoring",
    display_purpose: "将规范会话与轮次和模型调用关联起来。",
    safety_boundary:
      "只能生成内部 UUIDv7；不得将其与遥测来源提供的标识符混淆，也不得在系统通知中暴露。",
  });

  const conversationSourceId = fieldById(catalog, "conversation.source_id");
  assert.equal(conversationSourceId.domain_type, "ConversationSourceId");
  assert.equal(conversationSourceId.nullable, true);
  assert.equal(conversationSourceId.persisted, true);
  assert.deepEqual(
    conversationSourceId.sources.map((source) => source.source_id),
    ["otlp-logs", "codex-sessions", "legacy-import"],
  );
  assert.equal(
    fieldById(catalog, "conversation.thread_id").domain_type,
    "ThreadSourceId",
  );

  const turnId = fieldById(catalog, "turn.id");
  assert.equal(turnId.domain_type, "TurnId");
  assert.equal(turnId.nullable, false);
  assert.equal(turnId.persisted, true);
  assert.deepEqual(turnId.sources, [
    {
      source_id: "domain-engine",
      mappings: [
        "derived:canonical_turn_identity",
        "correlation.proven_turn_id",
      ],
    },
  ]);
  const turnSourceId = fieldById(catalog, "turn.source_id");
  assert.equal(turnSourceId.domain_type, "TurnSourceId");
  assert.equal(turnSourceId.nullable, true);
  assert.equal(turnSourceId.persisted, true);
  assert.deepEqual(turnSourceId.sources, [
    {
      source_id: "codex-sessions",
      mappings: ["session.turn.id"],
    },
  ]);

  const canonicalIdentity = fieldById(catalog, "model_call.canonical_identity");
  assert.equal(canonicalIdentity.domain_type, "SourceEventIdentity");
  assert.equal(canonicalIdentity.nullable, false);
  assert.equal(canonicalIdentity.persisted, true);
  assert.deepEqual(canonicalIdentity.sources, [
    {
      source_id: "domain-engine",
      mappings: ["derived:canonical_source_event_identity"],
    },
  ]);

  assert.equal(
    fieldById(catalog, "model_call.temporal_class").domain_type,
    "DataOrigin",
  );
  assert.equal(
    fieldById(catalog, "model_call_observation.id").domain_type,
    "ModelCallObservationId",
  );
  assert.equal(
    fieldById(catalog, "model_call_observation.source_event_identity_version")
      .domain_type,
    "IdentityAlgorithmVersion",
  );
  assert.equal(
    fieldById(catalog, "model_call_observation.field_quality").domain_type,
    "EvidenceQuality",
  );
  assert.equal(
    fieldById(catalog, "source_health.status").domain_type,
    "SourceHealthStatus",
  );
});

test("catalog registers stream-event, source-contract, and source-health evidence exactly", () => {
  const catalog = loadCatalog();
  const streamEventKind = fieldById(catalog, "model_call.stream_event_kind");
  assert.equal(streamEventKind.domain_type, "StreamEventKind");
  assert.equal(streamEventKind.nullable, true);
  assert.equal(streamEventKind.persisted, true);
  assert.deepEqual(sourceById(streamEventKind, "otlp-logs").mappings, [
    'log_record.attributes["stream_event_kind"]',
    'log_record.attributes["sse_event_kind"]',
    'log_record.attributes["event.kind"]',
    'log_record.attributes["kind"]',
    'log_record.attributes["type"]',
    'log_record.attributes["response.type"]',
    "log_record.body.type",
  ]);
  assert.deepEqual(sourceById(streamEventKind, "codex-sessions").mappings, [
    "session.model_call.stream_event_kind",
  ]);
  assert.deepEqual(sourceById(streamEventKind, "legacy-import").mappings, [
    "legacy.codex_token_events.stream_event_kind",
  ]);

  const observationContract = fieldById(
    catalog,
    "model_call_observation.source_contract_version",
  );
  assert.equal(observationContract.domain_type, "SourceContractVersion");
  assert.equal(observationContract.nullable, false);
  assert.equal(observationContract.persisted, true);
  assert.deepEqual(observationContract.sources, [
    {
      source_id: "source-adapter",
      mappings: ["source_event_identity.source.contract_version"],
    },
  ]);

  const sourceHealthContract = fieldById(
    catalog,
    "source_health.source_contract_version",
  );
  assert.equal(sourceHealthContract.domain_type, "SourceContractVersion");
  assert.equal(sourceHealthContract.nullable, false);
  assert.equal(sourceHealthContract.persisted, true);
  assert.deepEqual(sourceHealthContract.sources, [
    {
      source_id: "domain-engine",
      mappings: ["source_runtime.identity.contract_version"],
    },
  ]);

  const supportedFields = fieldById(catalog, "source_health.supported_fields");
  assert.equal(supportedFields.domain_type, "FieldIdSet");
  assert.equal(supportedFields.nullable, false);
  assert.equal(supportedFields.persisted, true);
  assert.deepEqual(supportedFields.sources, [
    {
      source_id: "domain-engine",
      mappings: ["source_runtime.supported_fields"],
    },
  ]);

  const lastErrorOccurredAt = fieldById(
    catalog,
    "source_health.last_error_occurred_at",
  );
  assert.equal(lastErrorOccurredAt.domain_type, "UtcTimestamp");
  assert.equal(lastErrorOccurredAt.nullable, true);
  assert.equal(lastErrorOccurredAt.persisted, true);
  assert.deepEqual(lastErrorOccurredAt.sources, [
    {
      source_id: "domain-engine",
      mappings: ["source_runtime.last_failure.occurred_at"],
    },
  ]);

  const fieldIds = catalog.fields.map((field) => field.id);
  assert.equal(fieldIds.includes("model_call.primary_source_id"), false);
  assert.equal(fieldIds.includes("model_call.total_tokens"), false);
  assert.equal(
    persistedFieldIds(catalog).some((fieldId) =>
      fieldId.endsWith(".total_tokens"),
    ),
    false,
  );
});

test("repository persistence manifest exactly matches the sorted catalog boundary", () => {
  const catalog = loadCatalog();
  const manifest = parse(readFileSync(persistenceManifestPath, "utf8"));
  const expected = persistedFieldIds(catalog);
  assert.equal(manifest.version, 1);
  assert.deepEqual(manifest.persisted_field_ids, expected);

  const check = runGenerator([
    "--check",
    "--persistence-manifest",
    persistenceManifestPath,
  ]);
  assert.equal(check.status, 0, check.stderr);
  assert.match(check.stdout, /verified/u);
});

test("generates and checks the reference with an exact persistence manifest", (t) => {
  const directory = temporaryDirectory(t);
  const catalog = loadCatalog();
  const catalogPath = writeCatalog(directory, catalog);
  const outputPath = path.join(directory, "telemetry-fields.md");
  const manifestPath = path.join(directory, "persistence-manifest.json");
  writeFileSync(
    manifestPath,
    JSON.stringify(
      { version: 1, persisted_field_ids: persistedFieldIds(catalog) },
      null,
      2,
    ),
    "utf8",
  );

  const generate = runGenerator([
    "--catalog",
    catalogPath,
    "--output",
    outputPath,
    "--persistence-manifest",
    manifestPath,
  ]);
  assert.equal(generate.status, 0, generate.stderr);
  const generated = readFileSync(outputPath, "utf8");
  assert.match(generated, /# 遥测字段目录/u);
  assert.match(generated, /`model_call\.input_tokens`/u);
  assert.match(generated, /## 永久禁止字段/u);

  const check = runGenerator([
    "--check",
    "--catalog",
    catalogPath,
    "--output",
    outputPath,
    "--persistence-manifest",
    manifestPath,
  ]);
  assert.equal(check.status, 0, check.stderr);
  assert.match(check.stdout, /verified/u);
});

test("rejects duplicate allowed field ids", (t) => {
  const directory = temporaryDirectory(t);
  const catalog = loadCatalog();
  catalog.fields.push(structuredClone(catalog.fields[0]));
  const catalogPath = writeCatalog(directory, catalog);
  const outputPath = path.join(directory, "telemetry-fields.md");

  const result = runGenerator([
    "--catalog",
    catalogPath,
    "--output",
    outputPath,
  ]);
  assert.equal(result.status, 1);
  assert.match(result.stderr, /duplicate field id: conversation\.id/u);
});

test("rejects a permanently forbidden field or source key in the allowlist", (t) => {
  const directory = temporaryDirectory(t);
  const catalog = loadCatalog();
  const forbiddenField = structuredClone(catalog.fields[0]);
  forbiddenField.id = catalog.deny_list.find(
    (entry) => entry.id === "user_content",
  ).field_ids[0];
  catalog.fields.push(forbiddenField);
  let catalogPath = writeCatalog(directory, catalog);
  const outputPath = path.join(directory, "telemetry-fields.md");

  const forbiddenIdResult = runGenerator([
    "--catalog",
    catalogPath,
    "--output",
    outputPath,
  ]);
  assert.equal(forbiddenIdResult.status, 1);
  assert.match(
    forbiddenIdResult.stderr,
    /allowed field is permanently denied: content\.prompt/u,
  );

  const forbiddenMappingCatalog = loadCatalog();
  forbiddenMappingCatalog.fields[0].sources[0].mappings.push(
    'log_record.attributes["user.prompt"]',
  );
  catalogPath = writeCatalog(directory, forbiddenMappingCatalog);
  const forbiddenMappingResult = runGenerator([
    "--catalog",
    catalogPath,
    "--output",
    outputPath,
  ]);
  assert.equal(forbiddenMappingResult.status, 1);
  assert.match(
    forbiddenMappingResult.stderr,
    /references permanently denied key: user\.prompt/u,
  );
});

test("detects generated Markdown drift in check mode", (t) => {
  const directory = temporaryDirectory(t);
  const catalogPath = writeCatalog(directory, loadCatalog());
  const outputPath = path.join(directory, "telemetry-fields.md");
  const generate = runGenerator([
    "--catalog",
    catalogPath,
    "--output",
    outputPath,
  ]);
  assert.equal(generate.status, 0, generate.stderr);
  appendFileSync(outputPath, "synthetic-generated-drift\n", "utf8");

  const check = runGenerator([
    "--check",
    "--catalog",
    catalogPath,
    "--output",
    outputPath,
  ]);
  assert.equal(check.status, 1);
  assert.match(check.stderr, /generated reference drift/u);
});

test("detects missing and unregistered persistence field ids", (t) => {
  const directory = temporaryDirectory(t);
  const catalog = loadCatalog();
  const catalogPath = writeCatalog(directory, catalog);
  const outputPath = path.join(directory, "telemetry-fields.md");
  const generate = runGenerator([
    "--catalog",
    catalogPath,
    "--output",
    outputPath,
  ]);
  assert.equal(generate.status, 0, generate.stderr);

  const expected = persistedFieldIds(catalog);
  const missing = expected[0];
  const unregistered = "synthetic.unregistered_field";
  const manifestPath = path.join(directory, "persistence-manifest.json");
  writeFileSync(
    manifestPath,
    JSON.stringify(
      {
        version: 1,
        persisted_field_ids: [...expected.slice(1), unregistered].sort(
          stableCompare,
        ),
      },
      null,
      2,
    ),
    "utf8",
  );

  const check = runGenerator([
    "--check",
    "--catalog",
    catalogPath,
    "--output",
    outputPath,
    "--persistence-manifest",
    manifestPath,
  ]);
  assert.equal(check.status, 1);
  assert.match(check.stderr, /persistence manifest drift/u);
  assert.match(
    check.stderr,
    new RegExp(`missing from manifest: ${missing}`, "u"),
  );
  assert.match(
    check.stderr,
    new RegExp(`unregistered persisted fields: ${unregistered}`, "u"),
  );
});

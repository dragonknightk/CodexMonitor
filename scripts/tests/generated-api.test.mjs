import assert from "node:assert/strict";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import YAML from "yaml";

import {
  checkGeneratedApi,
  generatedSchemasMatch,
  openApiPath,
} from "../check-generated-api.mjs";

test("OpenAPI is the complete v1 local Dashboard authority", async () => {
  const document = YAML.parse(await readFile(openApiPath, "utf8"));

  assert.equal(document.openapi, "3.1.0");
  for (const path of [
    "/api/v1/session/bootstrap",
    "/api/v1/session",
    "/api/v1/health",
    "/api/v1/capabilities",
    "/api/v1/model-calls",
    "/api/v1/conversations",
    "/api/v1/summaries",
    "/api/v1/sources",
    "/api/v1/sources/{source_id}/retry",
    "/api/v1/alerts",
    "/api/v1/settings",
    "/api/v1/events",
  ]) {
    assert.ok(document.paths[path], `missing ${path}`);
  }

  const errorProperties = document.components.schemas.ApiError.properties;
  assert.deepEqual(Object.keys(errorProperties).sort(), ["code", "params"]);
  assert.equal(
    document.components.schemas.Settings.additionalProperties,
    false,
  );
  assert.equal(
    document.components.schemas.TokenUsage.properties.total.description.includes(
      "input + output",
    ),
    true,
  );
});

test("clean generation check detects a stale committed schema", async (t) => {
  assert.equal(await checkGeneratedApi(), true);
  assert.equal(generatedSchemasMatch("generated\n", "generated\r\n"), true);
  assert.equal(generatedSchemasMatch("generated\n", "stale\n"), false);

  const temporaryDirectory = await mkdtemp(
    join(tmpdir(), "codex-monitor-stale-api-"),
  );
  t.after(() => rm(temporaryDirectory, { force: true, recursive: true }));

  const staleSchemaPath = join(temporaryDirectory, "schema.d.ts");
  await writeFile(staleSchemaPath, "stale generated schema\n", "utf8");
  assert.equal(await checkGeneratedApi(staleSchemaPath), false);
});

import { existsSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

import { parse } from "yaml";

import { generateDatabaseSchemaReference } from "./generate-database-schema.mjs";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const authorityPath = path.join(
  root,
  "docs",
  "_meta",
  "telemetry-sources.yaml",
);
const outputPath = path.join(root, "docs", "reference", "telemetry-sources.md");

function renderSourceReference(catalog) {
  if (catalog?.version !== 1 || !Array.isArray(catalog.sources)) {
    throw new Error(
      "telemetry source catalog must have version 1 and a sources list",
    );
  }

  const lines = [
    "<!-- 由 scripts/generate-references.mjs 生成。请勿直接编辑。 -->",
    "# 已注册的遥测来源",
    "",
    "| 来源 ID | 契约版本 | v1 状态 | 能力 |",
    "| --- | ---: | --- | --- |",
  ];

  for (const source of catalog.sources) {
    if (
      typeof source.id !== "string" ||
      !Number.isInteger(source.contract_version) ||
      typeof source.status !== "string" ||
      !Array.isArray(source.capabilities)
    ) {
      throw new Error(
        "every telemetry source needs an id, integer contract_version, status and capabilities",
      );
    }
    const capabilities = source.capabilities.map(String).join("; ");
    lines.push(
      `| \`${source.id}\` | ${source.contract_version} | \`${source.status}\` | ${capabilities} |`,
    );
  }

  if (Array.isArray(catalog.notes) && catalog.notes.length > 0) {
    lines.push("");
    for (const note of catalog.notes) {
      lines.push(String(note));
    }
  }

  lines.push("");
  return lines.join("\n");
}

const checkOnly = process.argv.includes("--check");
const unknownArguments = process.argv
  .slice(2)
  .filter((value) => value !== "--check");
if (unknownArguments.length > 0) {
  throw new Error(`unknown argument: ${unknownArguments[0]}`);
}

const rendered = renderSourceReference(
  parse(readFileSync(authorityPath, "utf8")),
);
let failed = false;

if (checkOnly) {
  if (
    !existsSync(outputPath) ||
    readFileSync(outputPath, "utf8") !== rendered
  ) {
    console.error(
      "generated reference drift: docs/reference/telemetry-sources.md",
    );
    failed = true;
  } else {
    console.log("verified docs/reference/telemetry-sources.md");
  }
} else {
  writeFileSync(outputPath, rendered, "utf8");
  console.log("generated docs/reference/telemetry-sources.md");
}

try {
  generateDatabaseSchemaReference({ check: checkOnly });
} catch (error) {
  console.error(error instanceof Error ? error.message : String(error));
  failed = true;
}

if (failed) process.exitCode = 1;

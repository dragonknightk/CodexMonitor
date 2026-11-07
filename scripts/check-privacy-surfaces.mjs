import { readFileSync } from "node:fs";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

import { parse } from "yaml";

import {
  defaultMigrationsDirectory,
  loadMigrations,
  parseSchema,
} from "./generate-database-schema.mjs";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const manifestPath = path.join(root, "docs", "_meta", "privacy-surfaces.yaml");
const catalogPath = path.join(root, "docs", "_meta", "telemetry-fields.yaml");
const privacyDocuments = new Map([
  ["en-US", path.join(root, "PRIVACY.md")],
  ["zh-CN", path.join(root, "PRIVACY.zh-CN.md")],
]);
const actionSourcePath = path.join(
  root,
  "crates",
  "codex-monitor-core",
  "src",
  "action.rs",
);

const allowedClassifications = new Set([
  "action_audit",
  "configuration",
  "migration_audit",
  "operational_metadata",
  "pseudonymous_identifier",
  "structured_monitoring",
]);

function fail(message) {
  throw new Error(`privacy surface review failed: ${message}`);
}

function requireObject(value, context) {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    fail(`${context} must be an object`);
  }
  return value;
}

function requireString(value, context) {
  if (typeof value !== "string" || value.trim() === "") {
    fail(`${context} must be a non-empty string`);
  }
  return value;
}

function requireStringList(value, context) {
  if (!Array.isArray(value) || value.length === 0) {
    fail(`${context} must be a non-empty string list`);
  }
  const values = value.map((entry, index) =>
    requireString(entry, `${context}[${index}]`),
  );
  if (new Set(values).size !== values.length)
    fail(`${context} contains duplicates`);
  return values;
}

function compareExact(actual, expected, context) {
  if (
    actual.length !== expected.length ||
    actual.some((value, index) => value !== expected[index])
  ) {
    fail(
      `${context} drifted\n  reviewed: ${expected.join(", ")}\n  actual: ${actual.join(", ")}`,
    );
  }
}

function notificationParameterFields() {
  const source = readFileSync(actionSourcePath, "utf8");
  const body = source.match(
    /pub struct SystemNotificationParams\s*\{(?<body>[\s\S]*?)\n\}/u,
  )?.groups?.body;
  if (body === undefined) fail("SystemNotificationParams was not found");
  const fields = [...body.matchAll(/^\s*(?<field>[a-z][a-z0-9_]*):/gmu)].map(
    (match) => match.groups.field,
  );
  if (fields.length === 0)
    fail("SystemNotificationParams has no parsed fields");
  return fields;
}

function main() {
  const manifest = requireObject(
    parse(readFileSync(manifestPath, "utf8")),
    "privacy surface manifest",
  );
  if (manifest.version !== 2) fail("manifest version must be 2");

  const schema = parseSchema(loadMigrations(defaultMigrationsDirectory));
  if (!Array.isArray(manifest.sqlite_tables)) {
    fail("sqlite_tables must be a list");
  }
  const reviewedTables = manifest.sqlite_tables.map((entry, index) => {
    const reviewed = requireObject(entry, `sqlite_tables[${index}]`);
    const table = requireString(
      reviewed.table,
      `sqlite_tables[${index}].table`,
    );
    const classification = requireString(
      reviewed.classification,
      `sqlite_tables[${index}].classification`,
    );
    if (!allowedClassifications.has(classification)) {
      fail(`${table} has unknown classification ${classification}`);
    }
    requireString(reviewed.rationale, `sqlite_tables[${index}].rationale`);
    return {
      table,
      columns: requireStringList(
        reviewed.columns,
        `sqlite_tables[${index}].columns`,
      ),
    };
  });
  compareExact(
    schema.tables.map((table) => table.name),
    reviewedTables.map((table) => table.table),
    "SQLite table review",
  );
  for (const [index, table] of schema.tables.entries()) {
    compareExact(
      table.columns.map((column) => column.name),
      reviewedTables[index].columns,
      `SQLite columns for ${table.name}`,
    );
  }

  const catalog = requireObject(
    parse(readFileSync(catalogPath, "utf8")),
    "telemetry field catalog",
  );
  const deniedTerms = new Set(
    catalog.deny_list.flatMap((entry) => entry.field_id_terms),
  );
  for (const table of schema.tables) {
    for (const column of table.columns) {
      if (deniedTerms.has(column.name)) {
        fail(
          `SQLite column ${table.name}.${column.name} uses a denied field term`,
        );
      }
    }
  }

  const notifications = requireObject(
    manifest.system_notification,
    "system_notification",
  );
  const reviewedParameters = requireStringList(
    notifications.dynamic_parameter_fields,
    "system_notification.dynamic_parameter_fields",
  );
  compareExact(
    notificationParameterFields(),
    reviewedParameters,
    "system notification parameter review",
  );
  requireString(notifications.rationale, "system_notification.rationale");
  const forbiddenContent = requireObject(
    notifications.forbidden_content,
    "system_notification.forbidden_content",
  );
  compareExact(
    Object.keys(forbiddenContent),
    [...privacyDocuments.keys()],
    "system notification privacy-document locales",
  );
  for (const [locale, privacyPath] of privacyDocuments) {
    const terms = requireStringList(
      forbiddenContent[locale],
      `system_notification.forbidden_content.${locale}`,
    );
    const privacy = readFileSync(privacyPath, "utf8").toLowerCase();
    for (const term of terms) {
      if (!privacy.includes(term.toLowerCase())) {
        fail(
          `${path.basename(privacyPath)} does not describe forbidden notification content: ${term}`,
        );
      }
    }
  }

  console.log(
    `privacy surfaces verified: ${schema.tables.length} SQLite tables, ${schema.tables.reduce((count, table) => count + table.columns.length, 0)} columns, ${reviewedParameters.length} notification parameters`,
  );
}

try {
  main();
} catch (error) {
  console.error(error instanceof Error ? error.message : String(error));
  process.exitCode = 1;
}

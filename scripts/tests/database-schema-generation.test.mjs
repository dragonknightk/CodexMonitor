import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import {
  appendFileSync,
  copyFileSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  rmSync,
} from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  canonicalizeMigrationSql,
  defaultMigrationsDirectory,
  defaultOutputPath,
  loadMigrations,
  migrationSha256,
  parseSchema,
  renderReference,
} from "../generate-database-schema.mjs";

const root = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  "..",
  "..",
);
const generatorPath = path.join(
  root,
  "scripts",
  "generate-database-schema.mjs",
);
const referenceOrchestratorPath = path.join(
  root,
  "scripts",
  "generate-references.mjs",
);

function temporaryDirectory(t) {
  const directory = mkdtempSync(
    path.join(os.tmpdir(), "codex-monitor-database-schema-"),
  );
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  return directory;
}

function copyRepositoryMigrations(directory) {
  const migrationsDirectory = path.join(directory, "migrations");
  mkdirSync(migrationsDirectory);
  const filenames = readdirSync(defaultMigrationsDirectory)
    .filter((name) => /^\d{4}_[a-z0-9_]+\.sql$/u.test(name))
    .sort();
  for (const name of filenames) {
    copyFileSync(
      path.join(defaultMigrationsDirectory, name),
      path.join(migrationsDirectory, name),
    );
  }
  return { migrationsDirectory, filenames };
}

function runNode(script, arguments_) {
  return spawnSync(process.execPath, [script, ...arguments_], {
    cwd: root,
    encoding: "utf8",
  });
}

function declaredNames(sql, expression) {
  return [...sql.matchAll(expression)].map((match) => match[1]).sort();
}

function syntheticMigration(sql) {
  return {
    version: 1,
    name: "0001_synthetic.sql",
    sql,
    sha256: migrationSha256(sql),
    canonicalBytes: Buffer.byteLength(sql, "utf8"),
  };
}

function parseSyntheticSchema(sql) {
  return parseSchema([syntheticMigration(sql)]);
}

function uniqueSorted(values) {
  return [...new Set(values)].sort();
}

test("checksum canonicalization exactly matches the Rust CRLF rule", () => {
  const lf = "CREATE TABLE synthetic (id INTEGER);\n";
  const crlf = lf.replaceAll("\n", "\r\n");
  const isolatedCr = lf.replace("\n", "\r");

  assert.equal(canonicalizeMigrationSql(crlf), lf);
  assert.equal(migrationSha256(crlf), migrationSha256(lf));
  assert.notEqual(migrationSha256(isolatedCr), migrationSha256(lf));
});

test("unsupported schema changes fail instead of disappearing from the reference", () => {
  for (const statement of [
    "ALTER TABLE synthetic ADD COLUMN value TEXT;",
    "ALTER TABLE synthetic RENAME COLUMN id TO identifier;",
    "DROP VIEW synthetic;",
    "DROP TABLE synthetic, other;",
  ]) {
    const sql = [
      "CREATE TABLE synthetic (id INTEGER PRIMARY KEY);",
      statement,
      "",
    ].join("\n");
    assert.throws(
      () => parseSyntheticSchema(sql),
      /unsupported schema-changing statement/u,
      statement,
    );
  }
});

test("ordered drop and rename operations produce only the rebuilt schema", () => {
  const sql = `
CREATE TABLE parent (
    id INTEGER PRIMARY KEY
);
CREATE TABLE child (
    id INTEGER PRIMARY KEY,
    parent_id INTEGER REFERENCES parent (id)
);
CREATE INDEX parent_old_idx ON parent (id);
CREATE TRIGGER parent_old_guard
BEFORE UPDATE ON parent
BEGIN
    SELECT RAISE(ABORT, 'old parent');
END;

CREATE TABLE parent_next (
    id INTEGER PRIMARY KEY,
    label TEXT NOT NULL,
    parent_next INTEGER NOT NULL,
    CONSTRAINT parent_next_positive CHECK (parent_next >= 0)
);
CREATE TABLE child_next (
    id INTEGER PRIMARY KEY,
    parent_id INTEGER REFERENCES "parent_next" (id),
    parent_next_shadow TEXT CHECK (parent_next_shadow <> 'parent_next'),
    CONSTRAINT child_parent_fk FOREIGN KEY (parent_id) REFERENCES parent_next (id),
    CHECK ('parent_next' <> parent_next_shadow)
);
CREATE INDEX parent_live_idx ON parent_next (label);
CREATE INDEX parent_disposable_idx ON parent_next (id);
CREATE TRIGGER parent_live_guard
BEFORE UPDATE ON parent_next
BEGIN
    SELECT RAISE(ABORT, 'parent_next string literal');
END;
CREATE TRIGGER parent_disposable_guard
BEFORE DELETE ON parent_next
BEGIN
    SELECT RAISE(ABORT, 'disposable');
END;

DROP INDEX IF EXISTS parent_disposable_idx;
DROP TRIGGER parent_disposable_guard;
DROP TABLE child;
DROP TABLE IF EXISTS parent;
ALTER TABLE parent_next RENAME TO parent;
ALTER TABLE child_next RENAME TO child;
`;

  const schema = parseSyntheticSchema(sql);
  assert.deepEqual(
    schema.tables.map((table) => table.name),
    ["parent", "child"],
  );
  assert.deepEqual(
    schema.indexes.map(({ name, table }) => ({ name, table })),
    [{ name: "parent_live_idx", table: "parent" }],
  );
  assert.deepEqual(
    schema.triggers.map(({ name, table }) => ({ name, table })),
    [{ name: "parent_live_guard", table: "parent" }],
  );

  const parent = schema.tables.find((table) => table.name === "parent");
  assert.ok(parent);
  assert.equal(
    parent.columns.find((column) => column.name === "parent_next").declaration,
    "INTEGER NOT NULL",
  );
  assert.deepEqual(parent.constraints, [
    "CONSTRAINT parent_next_positive CHECK (parent_next >= 0)",
  ]);

  const child = schema.tables.find((table) => table.name === "child");
  assert.ok(child);
  assert.equal(
    child.columns.find((column) => column.name === "parent_id").declaration,
    'INTEGER REFERENCES "parent" (id)',
  );
  assert.equal(
    child.columns.find((column) => column.name === "parent_next_shadow")
      .declaration,
    "TEXT CHECK (parent_next_shadow <> 'parent_next')",
  );
  assert.deepEqual(child.constraints, [
    "CONSTRAINT child_parent_fk FOREIGN KEY (parent_id) REFERENCES parent (id)",
    "CHECK ('parent_next' <> parent_next_shadow)",
  ]);
});

test("unknown drop and rename targets fail even with IF EXISTS", () => {
  const unknownOperations = [
    ["DROP TABLE IF EXISTS missing;", /cannot drop unknown table missing/u],
    ["DROP INDEX IF EXISTS missing;", /cannot drop unknown index missing/u],
    ["DROP TRIGGER IF EXISTS missing;", /cannot drop unknown trigger missing/u],
    [
      "ALTER TABLE missing RENAME TO renamed;",
      /cannot rename unknown table missing/u,
    ],
  ];
  for (const [statement, expectedError] of unknownOperations) {
    const sql = [
      "CREATE TABLE synthetic (id INTEGER PRIMARY KEY);",
      statement,
      "",
    ].join("\n");
    assert.throws(() => parseSyntheticSchema(sql), expectedError, statement);
  }

  const collision = [
    "CREATE TABLE first (id INTEGER PRIMARY KEY);",
    "CREATE TABLE second (id INTEGER PRIMARY KEY);",
    "ALTER TABLE first RENAME TO second;",
    "",
  ].join("\n");
  assert.throws(
    () => parseSyntheticSchema(collision),
    /cannot rename table first to existing schema object second/u,
  );

  const indexCollision = [
    "CREATE TABLE first (id INTEGER PRIMARY KEY);",
    "CREATE TABLE host (id INTEGER PRIMARY KEY);",
    "CREATE INDEX second ON host (id);",
    "ALTER TABLE first RENAME TO second;",
    "",
  ].join("\n");
  assert.throws(
    () => parseSyntheticSchema(indexCollision),
    /cannot rename table first to existing schema object second/u,
  );

  const triggerCollision = [
    "CREATE TABLE first (id INTEGER PRIMARY KEY);",
    "CREATE TABLE host (id INTEGER PRIMARY KEY);",
    "CREATE TRIGGER second BEFORE UPDATE ON host BEGIN SELECT 1; END;",
    "ALTER TABLE first RENAME TO second;",
    "",
  ].join("\n");
  assert.throws(
    () => parseSyntheticSchema(triggerCollision),
    /cannot rename table first to existing schema object second/u,
  );
});

test("object table references follow SQLite identifier casing rules", () => {
  const sql = `
CREATE TABLE MixedCase (id INTEGER PRIMARY KEY);
CREATE INDEX mixed_case_idx ON mixedcase (id);
CREATE TRIGGER mixed_case_guard
BEFORE UPDATE ON MIXEDCASE
BEGIN
    SELECT RAISE(ABORT, 'guard');
END;
`;

  const schema = parseSyntheticSchema(sql);
  assert.equal(schema.indexes[0].table, "mixedcase");
  assert.equal(schema.triggers[0].table, "MIXEDCASE");
});

test("every final table, explicit index, and trigger is rendered", () => {
  const migrations = loadMigrations();
  const schema = parseSchema(migrations);
  const rendered = renderReference(schema);
  const sql = migrations.map((migration) => migration.sql).join("\n");

  const expectedTables = uniqueSorted(
    declaredNames(sql, /^CREATE TABLE\s+([A-Za-z_][A-Za-z0-9_]*)/gmu).filter(
      (name) => !["action_outbox_v6", "action_attempts_v6"].includes(name),
    ),
  );
  const expectedIndexes = uniqueSorted(
    declaredNames(
      sql,
      /^CREATE (?:UNIQUE )?INDEX\s+([A-Za-z_][A-Za-z0-9_]*)/gmu,
    ).filter((name) => name !== "action_outbox_state_idx"),
  );
  const expectedTriggers = uniqueSorted(
    declaredNames(sql, /^CREATE TRIGGER\s+([A-Za-z_][A-Za-z0-9_]*)/gmu),
  );

  assert.deepEqual(
    schema.tables.map((table) => table.name).sort(),
    expectedTables,
  );
  assert.deepEqual(
    schema.indexes.map((index) => index.name).sort(),
    expectedIndexes,
  );
  assert.deepEqual(
    schema.triggers.map((trigger) => trigger.name).sort(),
    expectedTriggers,
  );
  assert.equal(
    schema.tables.some((table) => table.name.endsWith("_v6")),
    false,
  );
  for (const name of ["action_outbox", "action_attempts"]) {
    const table = schema.tables.find((candidate) => candidate.name === name);
    assert.ok(table, `${name} is present in the final schema`);
    assert.equal(table.migration.version, 6);
  }
  assert.equal(
    schema.indexes.some((index) => index.name === "action_outbox_state_idx"),
    false,
  );
  const attempts = schema.tables.find(
    (table) => table.name === "action_attempts",
  );
  assert.match(attempts.constraints.join(" "), /REFERENCES action_outbox\b/u);
  assert.doesNotMatch(
    attempts.constraints.join(" "),
    /REFERENCES action_outbox_v6\b/u,
  );
  for (const triggerName of [
    "action_outbox_historical_insert_guard",
    "action_outbox_historical_update_guard",
  ]) {
    const trigger = schema.triggers.find(
      (candidate) => candidate.name === triggerName,
    );
    assert.ok(trigger);
    assert.equal(trigger.migration.version, 6);
  }
  for (const table of schema.tables) {
    assert.ok(
      table.columns.length > 0,
      `${table.name} has no rendered columns`,
    );
    assert.match(rendered, new RegExp(`^### \`${table.name}\`$`, "mu"));
  }
  for (const migration of migrations) {
    assert.match(rendered, new RegExp(migration.sha256, "u"));
    assert.equal(migration.sha256.length, 64);
    assert.equal(migration.canonicalBytes, Buffer.byteLength(migration.sql));
  }
  assert.match(
    rendered,
    /只有遥测字段目录中\s*已注册的字段可以跨越持久化边界/u,
  );
  assert.doesNotMatch(rendered, /^### `action_(?:outbox|attempts)_v6`$/gmu);
  assert.doesNotMatch(rendered, /[A-Z]:\\/u);
});

test("repository reference and the existing reference orchestrator pass check mode", () => {
  const direct = runNode(generatorPath, ["--check"]);
  assert.equal(direct.status, 0, direct.stderr);
  assert.match(direct.stdout, /verified docs\/reference\/database-schema\.md/u);

  const orchestrated = runNode(referenceOrchestratorPath, ["--check"]);
  assert.equal(orchestrated.status, 0, orchestrated.stderr);
  assert.match(
    orchestrated.stdout,
    /verified docs\/reference\/database-schema\.md/u,
  );
  assert.equal(readFileSync(defaultOutputPath, "utf8").length > 0, true);
});

test("check mode reports the target and never rewrites stale output", (t) => {
  const directory = temporaryDirectory(t);
  const { migrationsDirectory, filenames } =
    copyRepositoryMigrations(directory);
  const outputPath = path.join(directory, "database-schema.md");
  const arguments_ = [
    "--migrations-dir",
    migrationsDirectory,
    "--output",
    outputPath,
  ];

  const generate = runNode(generatorPath, arguments_);
  assert.equal(generate.status, 0, generate.stderr);
  const generated = readFileSync(outputPath, "utf8");

  appendFileSync(outputPath, "synthetic generated drift\n", "utf8");
  const staleOutput = readFileSync(outputPath, "utf8");
  const outputCheck = runNode(generatorPath, ["--check", ...arguments_]);
  assert.equal(outputCheck.status, 1);
  assert.match(outputCheck.stderr, /generated reference drift:/u);
  assert.match(outputCheck.stderr, /database-schema\.md/u);
  assert.equal(readFileSync(outputPath, "utf8"), staleOutput);

  rmSync(outputPath);
  const missingCheck = runNode(generatorPath, ["--check", ...arguments_]);
  assert.equal(missingCheck.status, 1);
  assert.equal(existsSync(outputPath), false);

  const regenerate = runNode(generatorPath, arguments_);
  assert.equal(regenerate.status, 0, regenerate.stderr);
  assert.equal(readFileSync(outputPath, "utf8"), generated);
  appendFileSync(
    path.join(migrationsDirectory, filenames[0]),
    "\n-- synthetic migration checksum drift\n",
    "utf8",
  );
  const migrationCheck = runNode(generatorPath, ["--check", ...arguments_]);
  assert.equal(migrationCheck.status, 1);
  assert.match(migrationCheck.stderr, /generated reference drift:/u);
  assert.equal(readFileSync(outputPath, "utf8"), generated);
});

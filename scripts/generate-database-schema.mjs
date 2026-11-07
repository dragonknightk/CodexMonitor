import { createHash } from "node:crypto";
import {
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  writeFileSync,
} from "node:fs";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const scriptPath = fileURLToPath(import.meta.url);
const root = path.resolve(path.dirname(scriptPath), "..");

export const defaultMigrationsDirectory = path.join(
  root,
  "crates",
  "codex-monitor-storage",
  "migrations",
);
export const defaultOutputPath = path.join(
  root,
  "docs",
  "reference",
  "database-schema.md",
);

const MIGRATION_FILE_PATTERN = /^(\d{4})_[a-z0-9_]+\.sql$/u;
const IDENTIFIER_PATTERN = "[A-Za-z_][A-Za-z0-9_]*";

export function canonicalizeMigrationSql(sql) {
  return sql.replaceAll("\r\n", "\n");
}

export function migrationSha256(sql) {
  return createHash("sha256")
    .update(canonicalizeMigrationSql(sql), "utf8")
    .digest("hex");
}

function maskSql(sql) {
  const characters = sql.split("");
  const masked = [...characters];

  function blank(index) {
    if (characters[index] !== "\n" && characters[index] !== "\r") {
      masked[index] = " ";
    }
  }

  for (let index = 0; index < characters.length; index += 1) {
    const character = characters[index];
    const next = characters[index + 1];

    if (character === "-" && next === "-") {
      blank(index);
      blank(index + 1);
      index += 2;
      while (index < characters.length && characters[index] !== "\n") {
        blank(index);
        index += 1;
      }
      index -= 1;
      continue;
    }

    if (character === "/" && next === "*") {
      blank(index);
      blank(index + 1);
      index += 2;
      let closed = false;
      while (index < characters.length) {
        if (characters[index] === "*" && characters[index + 1] === "/") {
          blank(index);
          blank(index + 1);
          index += 1;
          closed = true;
          break;
        }
        blank(index);
        index += 1;
      }
      if (!closed) throw new Error("unterminated SQL block comment");
      continue;
    }

    const quoteEnd =
      character === "'"
        ? "'"
        : character === '"'
          ? '"'
          : character === "`"
            ? "`"
            : character === "["
              ? "]"
              : undefined;
    if (quoteEnd === undefined) continue;

    blank(index);
    let closed = false;
    for (index += 1; index < characters.length; index += 1) {
      blank(index);
      if (characters[index] !== quoteEnd) continue;
      if (quoteEnd !== "]" && characters[index + 1] === quoteEnd) {
        blank(index + 1);
        index += 1;
        continue;
      }
      closed = true;
      break;
    }
    if (!closed) throw new Error("unterminated SQL quoted value");
  }

  return masked.join("");
}

function normalizeSqlFragment(sql) {
  const characters = sql.split("");
  const output = [];
  let pendingSpace = false;

  function append(character) {
    if (pendingSpace && output.length > 0 && output.at(-1) !== " ") {
      output.push(" ");
    }
    pendingSpace = false;
    output.push(character);
  }

  for (let index = 0; index < characters.length; index += 1) {
    const character = characters[index];
    const next = characters[index + 1];

    if (character === "-" && next === "-") {
      pendingSpace = true;
      index += 2;
      while (index < characters.length && characters[index] !== "\n") {
        index += 1;
      }
      index -= 1;
      continue;
    }

    if (character === "/" && next === "*") {
      pendingSpace = true;
      index += 2;
      let closed = false;
      while (index < characters.length) {
        if (characters[index] === "*" && characters[index + 1] === "/") {
          index += 1;
          closed = true;
          break;
        }
        index += 1;
      }
      if (!closed) throw new Error("unterminated SQL block comment");
      continue;
    }

    if (/\s/u.test(character)) {
      pendingSpace = true;
      continue;
    }

    const quoteEnd =
      character === "'"
        ? "'"
        : character === '"'
          ? '"'
          : character === "`"
            ? "`"
            : character === "["
              ? "]"
              : undefined;
    if (quoteEnd === undefined) {
      append(character);
      continue;
    }

    append(character);
    let closed = false;
    for (index += 1; index < characters.length; index += 1) {
      output.push(characters[index]);
      if (characters[index] !== quoteEnd) continue;
      if (quoteEnd !== "]" && characters[index + 1] === quoteEnd) {
        output.push(characters[index + 1]);
        index += 1;
        continue;
      }
      closed = true;
      break;
    }
    if (!closed) throw new Error("unterminated SQL quoted value");
  }

  return output.join("").trim().replace(/;$/u, "").trim();
}

function splitSqlStatements(sql) {
  const masked = maskSql(sql);
  const statements = [];
  let statementStart = 0;
  let words = [];
  let isTrigger = false;
  const triggerBlocks = [];

  function reset() {
    words = [];
    isTrigger = false;
    triggerBlocks.length = 0;
  }

  for (let index = 0; index < masked.length; index += 1) {
    const character = masked[index];
    if (/[A-Za-z_]/u.test(character)) {
      let end = index + 1;
      while (end < masked.length && /[A-Za-z0-9_]/u.test(masked[end])) {
        end += 1;
      }
      const word = masked.slice(index, end).toUpperCase();
      words.push(word);
      if (
        (words[0] === "CREATE" && words[1] === "TRIGGER") ||
        (words[0] === "CREATE" &&
          ["TEMP", "TEMPORARY"].includes(words[1]) &&
          words[2] === "TRIGGER")
      ) {
        isTrigger = true;
      }
      if (isTrigger && (word === "BEGIN" || word === "CASE")) {
        triggerBlocks.push(word);
      } else if (isTrigger && word === "END") {
        if (triggerBlocks.length === 0) {
          throw new Error("unexpected END in CREATE TRIGGER");
        }
        triggerBlocks.pop();
      }
      index = end - 1;
      continue;
    }

    if (character !== ";") continue;
    if (isTrigger && triggerBlocks.length > 0) continue;

    const statement = sql.slice(statementStart, index + 1);
    if (normalizeSqlFragment(statement) !== "") statements.push(statement);
    statementStart = index + 1;
    reset();
  }

  const remainder = sql.slice(statementStart);
  if (normalizeSqlFragment(remainder) !== "") {
    throw new Error("SQL migration has an unterminated statement");
  }
  if (triggerBlocks.length > 0) {
    throw new Error("unterminated CREATE TRIGGER body");
  }
  return statements;
}

function matchingParenthesis(sql, openIndex) {
  const masked = maskSql(sql);
  let depth = 0;
  for (let index = openIndex; index < masked.length; index += 1) {
    if (masked[index] === "(") depth += 1;
    else if (masked[index] === ")") {
      depth -= 1;
      if (depth === 0) return index;
      if (depth < 0) break;
    }
  }
  throw new Error("unbalanced SQL parentheses");
}

function splitTopLevelList(sql) {
  const masked = maskSql(sql);
  const items = [];
  let depth = 0;
  let start = 0;
  for (let index = 0; index < masked.length; index += 1) {
    if (masked[index] === "(") depth += 1;
    else if (masked[index] === ")") depth -= 1;
    else if (masked[index] === "," && depth === 0) {
      items.push(normalizeSqlFragment(sql.slice(start, index)));
      start = index + 1;
    }
    if (depth < 0) throw new Error("unbalanced SQL list parentheses");
  }
  if (depth !== 0) throw new Error("unbalanced SQL list parentheses");
  items.push(normalizeSqlFragment(sql.slice(start)));
  return items.filter(Boolean);
}

function parseTable(statement, migration) {
  const normalized = normalizeSqlFragment(statement);
  const header = new RegExp(
    `^CREATE\\s+TABLE\\s+(?:IF\\s+NOT\\s+EXISTS\\s+)?(${IDENTIFIER_PATTERN})\\s*\\(`,
    "iu",
  ).exec(normalized);
  if (header === null) {
    throw new Error(`unsupported CREATE TABLE in ${migration.name}`);
  }
  const openIndex = normalized.indexOf("(", header.index);
  const closeIndex = matchingParenthesis(normalized, openIndex);
  const body = normalized.slice(openIndex + 1, closeIndex);
  const options = normalized.slice(closeIndex + 1).trim();
  const columns = [];
  const constraints = [];

  for (const item of splitTopLevelList(body)) {
    if (
      /^(?:CONSTRAINT\s+[A-Za-z_][A-Za-z0-9_]*\s+)?(?:PRIMARY\s+KEY|UNIQUE|CHECK|FOREIGN\s+KEY)\b/iu.test(
        item,
      )
    ) {
      constraints.push(item);
      continue;
    }
    const column = new RegExp(`^(${IDENTIFIER_PATTERN})\\s+(.+)$`, "u").exec(
      item,
    );
    if (column === null) {
      throw new Error(
        `unsupported column declaration in ${migration.name}: ${item}`,
      );
    }
    columns.push({ name: column[1], declaration: column[2] });
  }
  if (columns.length === 0) {
    throw new Error(`table ${header[1]} has no columns in ${migration.name}`);
  }

  return {
    name: header[1],
    migration,
    columns,
    constraints,
    options: options || "none",
  };
}

function parseIndex(statement, migration) {
  const normalized = normalizeSqlFragment(statement);
  const header = new RegExp(
    `^CREATE\\s+(UNIQUE\\s+)?INDEX\\s+(?:IF\\s+NOT\\s+EXISTS\\s+)?(${IDENTIFIER_PATTERN})\\s+ON\\s+(${IDENTIFIER_PATTERN})\\s*\\(`,
    "iu",
  ).exec(normalized);
  if (header === null) {
    throw new Error(`unsupported CREATE INDEX in ${migration.name}`);
  }
  const openIndex = normalized.indexOf("(", header.index);
  const closeIndex = matchingParenthesis(normalized, openIndex);
  const predicate = normalized.slice(closeIndex + 1).trim();
  return {
    name: header[2],
    table: header[3],
    unique: header[1] !== undefined,
    expression: normalized.slice(openIndex + 1, closeIndex).trim(),
    predicate: predicate || "none",
    migration,
  };
}

function parseTrigger(statement, migration) {
  const normalized = normalizeSqlFragment(statement);
  const masked = maskSql(normalized);
  const beginMatch = /\bBEGIN\b/iu.exec(masked);
  if (beginMatch === null) {
    throw new Error(`CREATE TRIGGER has no BEGIN in ${migration.name}`);
  }
  const header = normalized.slice(0, beginMatch.index).trim();
  const match = new RegExp(
    `^CREATE\\s+(?:TEMP(?:ORARY)?\\s+)?TRIGGER\\s+(?:IF\\s+NOT\\s+EXISTS\\s+)?(${IDENTIFIER_PATTERN})\\s+(.+?)\\s+ON\\s+(${IDENTIFIER_PATTERN})(?:\\s+FOR\\s+EACH\\s+ROW)?(?:\\s+WHEN\\s+.+)?$`,
    "iu",
  ).exec(header);
  if (match === null) {
    throw new Error(`unsupported CREATE TRIGGER in ${migration.name}`);
  }
  return {
    name: match[1],
    event: match[2],
    table: match[3],
    migration,
  };
}

function assertUniqueObjects(objects, kind) {
  const names = new Set();
  for (const object of objects) {
    const normalizedName = object.name.toLowerCase();
    if (names.has(normalizedName)) {
      throw new Error(`duplicate ${kind}: ${object.name}`);
    }
    names.add(normalizedName);
  }
}

function identifiersEqual(left, right) {
  return left.toLowerCase() === right.toLowerCase();
}

function findNamedObject(objects, name) {
  return objects.findIndex((object) => identifiersEqual(object.name, name));
}

function addObject(objects, object, kind) {
  if (findNamedObject(objects, object.name) !== -1) {
    throw new Error(`duplicate ${kind}: ${object.name}`);
  }
  objects.push(object);
}

function removeNamedObject(objects, name, kind, migration) {
  const index = findNamedObject(objects, name);
  if (index === -1) {
    throw new Error(`cannot drop unknown ${kind} ${name} in ${migration.name}`);
  }
  objects.splice(index, 1);
}

function removeObjectsForTable(objects, tableName) {
  for (let index = objects.length - 1; index >= 0; index -= 1) {
    if (identifiersEqual(objects[index].table, tableName)) {
      objects.splice(index, 1);
    }
  }
}

function replaceTableReferences(sql, oldName, newName) {
  let output = "";
  let expectsReferencedTable = false;

  for (let index = 0; index < sql.length;) {
    const character = sql[index];

    if (character === "'") {
      const start = index;
      index += 1;
      let closed = false;
      while (index < sql.length) {
        if (sql[index] !== "'") {
          index += 1;
          continue;
        }
        if (sql[index + 1] === "'") {
          index += 2;
          continue;
        }
        index += 1;
        closed = true;
        break;
      }
      if (!closed) throw new Error("unterminated SQL string literal");
      output += sql.slice(start, index);
      expectsReferencedTable = false;
      continue;
    }

    const quoteEnd =
      character === '"'
        ? '"'
        : character === "`"
          ? "`"
          : character === "["
            ? "]"
            : undefined;
    if (quoteEnd !== undefined) {
      const start = index;
      index += 1;
      let closed = false;
      while (index < sql.length) {
        if (sql[index] !== quoteEnd) {
          index += 1;
          continue;
        }
        if (quoteEnd !== "]" && sql[index + 1] === quoteEnd) {
          index += 2;
          continue;
        }
        index += 1;
        closed = true;
        break;
      }
      if (!closed) throw new Error("unterminated SQL quoted identifier");
      const quoted = sql.slice(start + 1, index - 1);
      output +=
        expectsReferencedTable && identifiersEqual(quoted, oldName)
          ? `${character}${newName}${quoteEnd}`
          : sql.slice(start, index);
      expectsReferencedTable = false;
      continue;
    }

    if (/[A-Za-z_]/u.test(character)) {
      let end = index + 1;
      while (end < sql.length && /[A-Za-z0-9_]/u.test(sql[end])) end += 1;
      const identifier = sql.slice(index, end);
      output +=
        expectsReferencedTable && identifiersEqual(identifier, oldName)
          ? newName
          : identifier;
      expectsReferencedTable =
        !expectsReferencedTable && identifiersEqual(identifier, "REFERENCES");
      index = end;
      continue;
    }

    output += character;
    if (!/\s/u.test(character)) expectsReferencedTable = false;
    index += 1;
  }

  return output;
}

function dropSchemaObject(normalized, migration, schema) {
  const match = new RegExp(
    `^DROP\\s+(TABLE|INDEX|TRIGGER)\\s+(?:IF\\s+EXISTS\\s+)?(${IDENTIFIER_PATTERN})$`,
    "iu",
  ).exec(normalized);
  if (match === null) {
    throw new Error(
      `unsupported schema-changing statement in ${migration.name}: ${normalized.slice(0, 80)}`,
    );
  }

  const kind = match[1].toLowerCase();
  const name = match[2];
  if (kind === "index") {
    removeNamedObject(schema.indexes, name, kind, migration);
    return;
  }
  if (kind === "trigger") {
    removeNamedObject(schema.triggers, name, kind, migration);
    return;
  }

  const tableIndex = findNamedObject(schema.tables, name);
  if (tableIndex === -1) {
    throw new Error(`cannot drop unknown table ${name} in ${migration.name}`);
  }
  const [table] = schema.tables.splice(tableIndex, 1);
  removeObjectsForTable(schema.indexes, table.name);
  removeObjectsForTable(schema.triggers, table.name);
}

function renameTable(normalized, migration, schema) {
  const match = new RegExp(
    `^ALTER\\s+TABLE\\s+(${IDENTIFIER_PATTERN})\\s+RENAME\\s+TO\\s+(${IDENTIFIER_PATTERN})$`,
    "iu",
  ).exec(normalized);
  if (match === null) {
    throw new Error(
      `unsupported schema-changing statement in ${migration.name}: ${normalized.slice(0, 80)}`,
    );
  }

  const oldName = match[1];
  const newName = match[2];
  const tableIndex = findNamedObject(schema.tables, oldName);
  if (tableIndex === -1) {
    throw new Error(
      `cannot rename unknown table ${oldName} in ${migration.name}`,
    );
  }
  if (
    [schema.tables, schema.indexes, schema.triggers].some(
      (objects) => findNamedObject(objects, newName) !== -1,
    )
  ) {
    throw new Error(
      `cannot rename table ${oldName} to existing schema object ${newName} in ${migration.name}`,
    );
  }

  const storedOldName = schema.tables[tableIndex].name;
  schema.tables[tableIndex].name = newName;
  for (const object of [...schema.indexes, ...schema.triggers]) {
    if (identifiersEqual(object.table, storedOldName)) object.table = newName;
  }
  for (const table of schema.tables) {
    for (const column of table.columns) {
      column.declaration = replaceTableReferences(
        column.declaration,
        storedOldName,
        newName,
      );
    }
    table.constraints = table.constraints.map((constraint) =>
      replaceTableReferences(constraint, storedOldName, newName),
    );
  }
}

export function loadMigrations(
  migrationsDirectory = defaultMigrationsDirectory,
) {
  const entries = readdirSync(migrationsDirectory, { withFileTypes: true });
  const invalidSqlFiles = entries
    .filter((entry) => entry.isFile() && entry.name.endsWith(".sql"))
    .map((entry) => entry.name)
    .filter((name) => !MIGRATION_FILE_PATTERN.test(name));
  if (invalidSqlFiles.length > 0) {
    throw new Error(
      `migration filenames must match NNNN_name.sql: ${invalidSqlFiles.join(", ")}`,
    );
  }

  const filenames = entries
    .filter(
      (entry) => entry.isFile() && MIGRATION_FILE_PATTERN.test(entry.name),
    )
    .map((entry) => entry.name)
    .sort();
  if (filenames.length === 0) throw new Error("no SQL migrations found");

  return filenames.map((name, index) => {
    const version = Number(MIGRATION_FILE_PATTERN.exec(name)[1]);
    const expectedVersion = index + 1;
    if (version !== expectedVersion) {
      throw new Error(
        `migration order is not contiguous: expected ${expectedVersion}, found ${version}`,
      );
    }
    const sql = canonicalizeMigrationSql(
      readFileSync(path.join(migrationsDirectory, name), "utf8"),
    );
    return {
      version,
      name,
      sql,
      sha256: migrationSha256(sql),
      canonicalBytes: Buffer.byteLength(sql, "utf8"),
    };
  });
}

export function parseSchema(migrations) {
  const tables = [];
  const indexes = [];
  const triggers = [];
  const schema = { migrations, tables, indexes, triggers };

  for (const migration of migrations) {
    for (const statement of splitSqlStatements(migration.sql)) {
      const normalized = normalizeSqlFragment(statement);
      if (/^CREATE\s+TABLE\b/iu.test(normalized)) {
        addObject(tables, parseTable(statement, migration), "table");
      } else if (/^CREATE\s+(?:UNIQUE\s+)?INDEX\b/iu.test(normalized)) {
        addObject(indexes, parseIndex(statement, migration), "index");
      } else if (
        /^CREATE\s+(?:TEMP(?:ORARY)?\s+)?TRIGGER\b/iu.test(normalized)
      ) {
        addObject(triggers, parseTrigger(statement, migration), "trigger");
      } else if (/^CREATE\b/iu.test(normalized)) {
        throw new Error(
          `unsupported CREATE statement in ${migration.name}: ${normalized.slice(0, 80)}`,
        );
      } else if (/^DROP\b/iu.test(normalized)) {
        dropSchemaObject(normalized, migration, schema);
      } else if (/^ALTER\b/iu.test(normalized)) {
        renameTable(normalized, migration, schema);
      }
    }
  }

  assertUniqueObjects(tables, "table");
  assertUniqueObjects(indexes, "index");
  assertUniqueObjects(triggers, "trigger");
  for (const object of [...indexes, ...triggers]) {
    if (!tables.some((table) => identifiersEqual(table.name, object.table))) {
      throw new Error(
        `${object.name} references unknown table ${object.table}`,
      );
    }
  }

  return schema;
}

function inlineCode(value) {
  const escaped = String(value).replaceAll("`", "\\`").replaceAll("|", "\\|");
  return `\`${escaped}\``;
}

export function renderReference(schema) {
  const lines = [
    "<!-- 由 scripts/generate-database-schema.mjs 生成。请勿直接编辑。 -->",
    "",
    "# 数据库模式",
    "",
    "本参考文档根据 `crates/codex-monitor-storage/migrations/` 中按顺序执行的 SQL",
    "迁移生成。这些迁移是机器可读权威。使用 `pnpm generate:references` 重新生成；",
    "使用 `node scripts/generate-database-schema.mjs --check` 进行不写文件的验证。",
    "",
    "生成器采用与 Rust 迁移运行时相同的校验和规范化方式：计算 SHA-256 和字节长度前，",
    "每个 CRLF 序列都会转换为 LF；单独出现的 CR 不会被改写。",
    "",
    "隐私边界：本文档只描述存储结构。列的存在不代表允许采集。只有遥测字段目录中",
    "已注册的字段可以跨越持久化边界。原始 OTLP 正文、提示词、响应、工具数据、",
    "推理内容、路径、账户标识符和认证材料绝不能持久化或记录到日志。",
    "",
    "## 迁移清单",
    "",
    "<!-- prettier-ignore -->",
    "| 版本 | 文件 | SHA-256 | 规范化字节数 |",
    "| ---: | --- | --- | ---: |",
  ];

  for (const migration of schema.migrations) {
    lines.push(
      `| ${migration.version} | ${inlineCode(migration.name)} | ${inlineCode(migration.sha256)} | ${migration.canonicalBytes} |`,
    );
  }

  lines.push(
    "",
    "## 模式概览",
    "",
    `- 表：${schema.tables.length}`,
    `- 显式索引：${schema.indexes.length}`,
    `- 触发器：${schema.triggers.length}`,
    "",
    "下方各表按迁移创建顺序排列。列声明包含内联的可空性、唯一性、引用、默认值和检查。",
    "单独的表级约束以规范化 SQL 形式原样列出。",
    "",
    "## 表",
    "",
  );

  for (const table of schema.tables) {
    lines.push(
      `### ${inlineCode(table.name)}`,
      "",
      `迁移：${table.migration.version}（${inlineCode(table.migration.name)}）。表选项：${inlineCode(table.options)}。`,
      "",
      "<!-- prettier-ignore -->",
      "| 列 | 声明 |",
      "| --- | --- |",
    );
    for (const column of table.columns) {
      lines.push(
        `| ${inlineCode(column.name)} | ${inlineCode(column.declaration)} |`,
      );
    }
    lines.push("", "表级约束：", "");
    if (table.constraints.length === 0) {
      lines.push("- 除列声明外没有其他约束。");
    } else {
      for (const constraint of table.constraints) {
        lines.push(`- ${inlineCode(constraint)}`);
      }
    }
    lines.push("");
  }

  lines.push(
    "## 显式索引",
    "",
    "`PRIMARY KEY` 或 `UNIQUE` 声明隐含的 SQLite 索引保留在上方表声明中。",
    "本节列出显式的 `CREATE INDEX` 语句。",
    "",
  );
  if (schema.indexes.length === 0) {
    lines.push("未声明显式索引。", "");
  } else {
    lines.push(
      "<!-- prettier-ignore -->",
      "| 索引 | 表 | 唯一 | 列或表达式 | 谓词 | 迁移 |",
      "| --- | --- | --- | --- | --- | --- |",
    );
    for (const index of schema.indexes) {
      lines.push(
        `| ${inlineCode(index.name)} | ${inlineCode(index.table)} | ${index.unique ? "是" : "否"} | ${inlineCode(index.expression)} | ${inlineCode(index.predicate)} | ${index.migration.version}（${inlineCode(index.migration.name)}） |`,
      );
    }
    lines.push("");
  }

  lines.push("## 触发器", "");
  if (schema.triggers.length === 0) {
    lines.push("未声明触发器。", "");
  } else {
    lines.push(
      "<!-- prettier-ignore -->",
      "| 触发器 | 表 | 事件 | 迁移 |",
      "| --- | --- | --- | --- |",
    );
    for (const trigger of schema.triggers) {
      lines.push(
        `| ${inlineCode(trigger.name)} | ${inlineCode(trigger.table)} | ${inlineCode(trigger.event)} | ${trigger.migration.version}（${inlineCode(trigger.migration.name)}） |`,
      );
    }
    lines.push("");
  }

  return lines.join("\n");
}

function displayPath(filePath) {
  const relative = path.relative(root, filePath);
  if (relative === "" || relative.startsWith(`..${path.sep}`)) return filePath;
  return relative.split(path.sep).join("/");
}

export function generateDatabaseSchemaReference({
  migrationsDirectory = defaultMigrationsDirectory,
  outputPath = defaultOutputPath,
  check = false,
} = {}) {
  const schema = parseSchema(loadMigrations(migrationsDirectory));
  const rendered = renderReference(schema);
  const target = displayPath(outputPath);

  if (check) {
    if (
      !existsSync(outputPath) ||
      readFileSync(outputPath, "utf8") !== rendered
    ) {
      throw new Error(`generated reference drift: ${target}`);
    }
    console.log(`verified ${target}`);
  } else {
    mkdirSync(path.dirname(outputPath), { recursive: true });
    writeFileSync(outputPath, rendered, "utf8");
    console.log(`generated ${target}`);
  }
  return { schema, rendered };
}

function requireArgumentValue(argv, index, argument) {
  const value = argv[index + 1];
  if (value === undefined || value.startsWith("--")) {
    throw new Error(`${argument} requires a path`);
  }
  return value;
}

function parseArguments(argv) {
  const options = {};
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === "--check") options.check = true;
    else if (argument === "--migrations-dir") {
      options.migrationsDirectory = path.resolve(
        requireArgumentValue(argv, index, argument),
      );
      index += 1;
    } else if (argument === "--output") {
      options.outputPath = path.resolve(
        requireArgumentValue(argv, index, argument),
      );
      index += 1;
    } else {
      throw new Error(`unknown argument: ${String(argument)}`);
    }
  }
  return options;
}

function main() {
  try {
    generateDatabaseSchemaReference(parseArguments(process.argv.slice(2)));
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 1;
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === scriptPath) main();

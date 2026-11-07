import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

import { parse } from "yaml";

const scriptPath = fileURLToPath(import.meta.url);
const root = path.resolve(path.dirname(scriptPath), "..");
const defaultCatalogPath = path.join(
  root,
  "docs",
  "_meta",
  "telemetry-fields.yaml",
);
const defaultOutputPath = path.join(
  root,
  "docs",
  "reference",
  "telemetry-fields.md",
);

const FIELD_ID_PATTERN = /^[a-z][a-z0-9_]*(?:\.[a-z][a-z0-9_]*)+$/u;
const SIMPLE_ID_PATTERN = /^[a-z][a-z0-9_-]*$/u;
const REQUIRED_ENTITIES = new Set([
  "Conversation",
  "Turn",
  "ModelCall",
  "ModelCallObservation",
  "SourceHealth",
]);

function stableCompare(left, right) {
  return left < right ? -1 : left > right ? 1 : 0;
}

function isObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function requireObject(value, context) {
  if (!isObject(value)) throw new Error(`${context} must be an object`);
  return value;
}

function requireString(value, context) {
  if (typeof value !== "string" || value.trim() === "") {
    throw new Error(`${context} must be a non-empty string`);
  }
  return value;
}

function requireSimpleId(value, context) {
  const id = requireString(value, context);
  if (!SIMPLE_ID_PATTERN.test(id)) {
    throw new Error(`${context} is not a stable id: ${id}`);
  }
  return id;
}

function requireFieldId(value, context) {
  const id = requireString(value, context);
  if (!FIELD_ID_PATTERN.test(id)) {
    throw new Error(`${context} is not a stable field id: ${id}`);
  }
  return id;
}

function requireStringList(value, context) {
  if (!Array.isArray(value) || value.length === 0) {
    throw new Error(`${context} must be a non-empty list`);
  }
  const result = value.map((item, index) =>
    requireString(item, `${context}[${index}]`),
  );
  const duplicate = result.find(
    (item, index) => result.indexOf(item) !== index,
  );
  if (duplicate !== undefined) {
    throw new Error(`${context} contains duplicate value: ${duplicate}`);
  }
  return result;
}

function requireObjectList(value, context) {
  if (!Array.isArray(value) || value.length === 0) {
    throw new Error(`${context} must be a non-empty list`);
  }
  return value.map((item, index) =>
    requireObject(item, `${context}[${index}]`),
  );
}

function assertUnique(id, seen, context) {
  if (seen.has(id)) throw new Error(`duplicate ${context}: ${id}`);
  seen.add(id);
}

function mappingKeys(mapping) {
  const keys = [];
  for (const match of mapping.matchAll(/\["([^"]+)"\]/gu)) {
    keys.push(match[1]);
  }
  return keys;
}

function validateExplicitMapping(mapping, context, deniedSourceKeys) {
  if (mapping.includes("*") || mapping.includes("...")) {
    throw new Error(`${context} must not use wildcard or spread syntax`);
  }
  if (/(?:^|[.:])(?:attributes|fields|json|payload|body)$/u.test(mapping)) {
    throw new Error(`${context} points to a generic container: ${mapping}`);
  }

  const candidates = new Set([mapping, ...mappingKeys(mapping)]);
  for (const candidate of candidates) {
    if (deniedSourceKeys.has(candidate)) {
      throw new Error(
        `${context} references permanently denied key: ${candidate}`,
      );
    }
  }
}

function validateCatalogShape(catalog) {
  requireObject(catalog, "telemetry field catalog");
  if (catalog.version !== 1) {
    throw new Error("telemetry field catalog version must be 1");
  }
  requireSimpleId(catalog.catalog_id, "catalog_id");
  requireString(catalog.description, "catalog description");

  const classificationIds = new Set();
  for (const [index, classification] of requireObjectList(
    catalog.privacy_classifications,
    "privacy_classifications",
  ).entries()) {
    const context = `privacy_classifications[${index}]`;
    const id = requireSimpleId(classification.id, `${context}.id`);
    assertUnique(id, classificationIds, "privacy classification id");
    requireString(classification.description, `${context}.description`);
  }

  const retentionById = new Map();
  for (const [index, retention] of requireObjectList(
    catalog.retention_policies,
    "retention_policies",
  ).entries()) {
    const context = `retention_policies[${index}]`;
    const id = requireSimpleId(retention.id, `${context}.id`);
    if (retentionById.has(id)) {
      throw new Error(`duplicate retention policy id: ${id}`);
    }
    if (typeof retention.persisted !== "boolean") {
      throw new Error(`${context}.persisted must be a boolean`);
    }
    requireString(retention.description, `${context}.description`);
    retentionById.set(id, retention);
  }

  const deniedFieldIds = new Set();
  const deniedFieldTerms = new Set();
  const deniedSourceKeys = new Set();
  const denyIds = new Set();
  for (const [index, denied] of requireObjectList(
    catalog.deny_list,
    "deny_list",
  ).entries()) {
    const context = `deny_list[${index}]`;
    const id = requireSimpleId(denied.id, `${context}.id`);
    assertUnique(id, denyIds, "deny id");
    const fieldIds = requireStringList(
      denied.field_ids,
      `${context}.field_ids`,
    );
    const fieldTerms = requireStringList(
      denied.field_id_terms,
      `${context}.field_id_terms`,
    );
    const sourceKeys = requireStringList(
      denied.source_keys,
      `${context}.source_keys`,
    );
    requireString(denied.reason, `${context}.reason`);
    requireString(denied.enforcement, `${context}.enforcement`);

    for (const fieldId of fieldIds) {
      requireFieldId(fieldId, `${context}.field_ids`);
      assertUnique(fieldId, deniedFieldIds, "denied field id");
    }
    for (const term of fieldTerms) {
      if (!/^[a-z][a-z0-9_]*$/u.test(term)) {
        throw new Error(`${context}.field_id_terms has invalid term: ${term}`);
      }
      deniedFieldTerms.add(term);
    }
    for (const sourceKey of sourceKeys) deniedSourceKeys.add(sourceKey);
  }

  const fieldIds = new Set();
  const coveredEntities = new Set();
  for (const [index, field] of requireObjectList(
    catalog.fields,
    "fields",
  ).entries()) {
    const context = `fields[${index}]`;
    const id = requireFieldId(field.id, `${context}.id`);
    assertUnique(id, fieldIds, "field id");
    if (deniedFieldIds.has(id)) {
      throw new Error(`allowed field is permanently denied: ${id}`);
    }
    for (const term of id.split(".")) {
      if (deniedFieldTerms.has(term)) {
        throw new Error(
          `allowed field contains permanently denied term: ${id}`,
        );
      }
    }

    const entities = requireStringList(field.entities, `${context}.entities`);
    for (const entity of entities) {
      if (!REQUIRED_ENTITIES.has(entity)) {
        throw new Error(`${context}.entities has unknown entity: ${entity}`);
      }
      coveredEntities.add(entity);
    }
    requireString(field.domain_type, `${context}.domain_type`);
    if (typeof field.nullable !== "boolean") {
      throw new Error(`${context}.nullable must be a boolean`);
    }

    const sourceIds = new Set();
    for (const [sourceIndex, source] of requireObjectList(
      field.sources,
      `${context}.sources`,
    ).entries()) {
      const sourceContext = `${context}.sources[${sourceIndex}]`;
      const sourceId = requireSimpleId(
        source.source_id,
        `${sourceContext}.source_id`,
      );
      assertUnique(sourceId, sourceIds, `source id for ${id}`);
      const mappings = requireStringList(
        source.mappings,
        `${sourceContext}.mappings`,
      );
      for (const mapping of mappings) {
        validateExplicitMapping(mapping, sourceContext, deniedSourceKeys);
      }
    }

    const classification = requireSimpleId(
      field.classification,
      `${context}.classification`,
    );
    if (!classificationIds.has(classification)) {
      throw new Error(
        `${context} references unknown classification: ${classification}`,
      );
    }
    if (typeof field.persisted !== "boolean") {
      throw new Error(`${context}.persisted must be a boolean`);
    }
    const retentionId = requireSimpleId(
      field.retention,
      `${context}.retention`,
    );
    const retention = retentionById.get(retentionId);
    if (retention === undefined) {
      throw new Error(
        `${context} references unknown retention policy: ${retentionId}`,
      );
    }
    if (retention.persisted !== field.persisted) {
      throw new Error(
        `${context} persisted=${field.persisted} conflicts with retention ${retentionId}`,
      );
    }
    requireString(field.display_purpose, `${context}.display_purpose`);
    requireString(field.safety_boundary, `${context}.safety_boundary`);
  }

  const missingEntities = [...REQUIRED_ENTITIES].filter(
    (entity) => !coveredEntities.has(entity),
  );
  if (missingEntities.length > 0) {
    throw new Error(
      `catalog has no fields for required entities: ${missingEntities.join(", ")}`,
    );
  }

  return catalog;
}

function markdownCell(value) {
  return String(value).replaceAll("|", "\\|").replace(/\r?\n/gu, " ").trim();
}

function code(value) {
  return `\`${String(value).replaceAll("`", "\\`")}\``;
}

function sortedById(items) {
  return [...items].sort((left, right) => stableCompare(left.id, right.id));
}

export function renderReference(catalog) {
  validateCatalogShape(catalog);
  const lines = [
    "<!-- 由 scripts/generate-telemetry-fields.mjs 生成。请勿直接编辑。 -->",
    "",
    "# 遥测字段目录",
    "",
    "权威来源为 `docs/_meta/telemetry-fields.yaml`。只有下方允许列表中的字段可以跨越",
    "持久化边界。来源映射是明确的提取路径；它们不会授权通用属性映射或 JSON 映射。",
    "",
    `目录：${code(catalog.catalog_id)}（模式版本 ${catalog.version}）。`,
    "",
    markdownCell(catalog.description),
    "",
    "## 隐私分类",
    "",
    "<!-- prettier-ignore -->",
    "| 分类 | 含义 |",
    "| --- | --- |",
  ];

  for (const classification of sortedById(catalog.privacy_classifications)) {
    lines.push(
      `| ${code(classification.id)} | ${markdownCell(classification.description)} |`,
    );
  }

  lines.push(
    "",
    "## 保留策略",
    "",
    "<!-- prettier-ignore -->",
    "| 策略 | 是否持久化 | 规则 |",
    "| --- | --- | --- |",
  );
  for (const retention of sortedById(catalog.retention_policies)) {
    lines.push(
      `| ${code(retention.id)} | ${retention.persisted ? "是" : "否"} | ${markdownCell(retention.description)} |`,
    );
  }

  lines.push(
    "",
    "## 允许字段",
    "",
    "<!-- prettier-ignore -->",
    "| 字段 ID | 领域实体 | 领域类型 | 可空 | 来源 | 隐私分类 | 是否持久化 | 保留策略 | 显示用途 | 安全边界 |",
    "| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |",
  );

  const fields = sortedById(catalog.fields);
  for (const field of fields) {
    const entities = field.entities.map(code).join("<br>");
    const sources = field.sources
      .map((source) => source.source_id)
      .sort(stableCompare)
      .map(code)
      .join("<br>");
    lines.push(
      `| ${code(field.id)} | ${entities} | ${code(field.domain_type)} | ${field.nullable ? "是" : "否"} | ${sources} | ${code(field.classification)} | ${field.persisted ? "是" : "否"} | ${code(field.retention)} | ${markdownCell(field.display_purpose)} | ${markdownCell(field.safety_boundary)} |`,
    );
  }

  lines.push(
    "",
    "## 明确的来源映射",
    "",
    "这里列出了每个可接受的同义键。来源适配器必须先丢弃未列出的键，",
    "然后才能构造类型化领域值。",
    "",
    "<!-- prettier-ignore -->",
    "| 字段 ID | 来源 | 接受的映射 |",
    "| --- | --- | --- |",
  );
  for (const field of fields) {
    const sources = [...field.sources].sort((left, right) =>
      stableCompare(left.source_id, right.source_id),
    );
    for (const source of sources) {
      const mappings = [...source.mappings]
        .sort(stableCompare)
        .map(code)
        .join("<br>");
      lines.push(
        `| ${code(field.id)} | ${code(source.source_id)} | ${mappings} |`,
      );
    }
  }

  lines.push(
    "",
    "## 永久禁止字段",
    "",
    "禁止字段不属于“允许但不持久化”的条目。它们会在进入类型化领域前被拒绝；",
    "若不修改这份隐私权威，就不能添加这些字段。",
    "",
    "<!-- prettier-ignore -->",
    "| 禁止规则 | 禁止的字段 ID | 禁止的 ID 词项 | 来源键 | 原因 | 强制措施 |",
    "| --- | --- | --- | --- | --- | --- |",
  );
  for (const denied of sortedById(catalog.deny_list)) {
    lines.push(
      `| ${code(denied.id)} | ${denied.field_ids.map(code).join("<br>")} | ${denied.field_id_terms.map(code).join("<br>")} | ${denied.source_keys.map(code).join("<br>")} | ${markdownCell(denied.reason)} | ${markdownCell(denied.enforcement)} |`,
    );
  }

  lines.push(
    "",
    "## 持久化验证",
    "",
    "持久化实现会导出版本 1 的 YAML 或 JSON 清单，其中含有",
    "`persisted_field_ids` 列表。请验证它与目录中 `persisted` 为 `true` 的",
    "全部字段完全一致：",
    "",
    "```text",
    "node scripts/generate-telemetry-fields.mjs --check --persistence-manifest <path>",
    "```",
    "",
    "缺失字段 ID 或出现未注册字段 ID 都会使命令失败。",
    "",
  );
  return lines.join("\n");
}

export function validateCatalog(catalog) {
  return validateCatalogShape(catalog);
}

export function comparePersistenceManifest(catalog, manifest) {
  validateCatalogShape(catalog);
  requireObject(manifest, "persistence manifest");
  if (manifest.version !== 1) {
    throw new Error("persistence manifest version must be 1");
  }
  const actual = requireStringList(
    manifest.persisted_field_ids,
    "persistence manifest persisted_field_ids",
  ).map((fieldId) => requireFieldId(fieldId, "persistence manifest field id"));
  const sortedActual = [...actual].sort(stableCompare);
  if (actual.some((fieldId, index) => fieldId !== sortedActual[index])) {
    throw new Error(
      "persistence manifest persisted_field_ids must use stable ascending order",
    );
  }
  const expected = catalog.fields
    .filter((field) => field.persisted)
    .map((field) => field.id)
    .sort(stableCompare);
  const actualSet = new Set(actual);
  const expectedSet = new Set(expected);
  const missing = expected.filter((fieldId) => !actualSet.has(fieldId));
  const unregistered = [...actualSet]
    .filter((fieldId) => !expectedSet.has(fieldId))
    .sort(stableCompare);

  if (missing.length > 0 || unregistered.length > 0) {
    const details = ["persistence manifest drift:"];
    if (missing.length > 0) {
      details.push(`missing from manifest: ${missing.join(", ")}`);
    }
    if (unregistered.length > 0) {
      details.push(`unregistered persisted fields: ${unregistered.join(", ")}`);
    }
    throw new Error(details.join("\n"));
  }
}

function parseArguments(argv) {
  const options = {
    catalogPath: defaultCatalogPath,
    outputPath: defaultOutputPath,
    check: false,
  };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === "--check") options.check = true;
    else if (argument === "--catalog") {
      options.catalogPath = path.resolve(
        requireString(argv[++index], "--catalog value"),
      );
    } else if (argument === "--output") {
      options.outputPath = path.resolve(
        requireString(argv[++index], "--output value"),
      );
    } else if (argument === "--persistence-manifest") {
      options.persistenceManifestPath = path.resolve(
        requireString(argv[++index], "--persistence-manifest value"),
      );
    } else {
      throw new Error(`unknown argument: ${String(argument)}`);
    }
  }
  return options;
}

function displayPath(filePath) {
  const relative = path.relative(root, filePath);
  if (
    relative !== "" &&
    !relative.startsWith(`..${path.sep}`) &&
    relative !== ".."
  ) {
    return relative.replaceAll("\\", "/");
  }
  return filePath;
}

export function run(options) {
  const catalog = validateCatalogShape(
    parse(readFileSync(options.catalogPath, "utf8")),
  );
  if (options.persistenceManifestPath !== undefined) {
    const manifest = parse(
      readFileSync(options.persistenceManifestPath, "utf8"),
    );
    comparePersistenceManifest(catalog, manifest);
  }

  const rendered = renderReference(catalog);
  if (options.check) {
    if (
      !existsSync(options.outputPath) ||
      readFileSync(options.outputPath, "utf8") !== rendered
    ) {
      throw new Error(
        `generated reference drift: ${displayPath(options.outputPath)}`,
      );
    }
    return `verified ${displayPath(options.outputPath)}`;
  }

  mkdirSync(path.dirname(options.outputPath), { recursive: true });
  writeFileSync(options.outputPath, rendered, "utf8");
  return `generated ${displayPath(options.outputPath)}`;
}

if (process.argv[1] && path.resolve(process.argv[1]) === scriptPath) {
  try {
    const options = parseArguments(process.argv.slice(2));
    console.log(run(options));
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 1;
  }
}

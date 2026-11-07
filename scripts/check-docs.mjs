import { existsSync, readFileSync, readdirSync } from "node:fs";
import path from "node:path";
import process from "node:process";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

import { minimatch } from "minimatch";
import { parse } from "yaml";

const defaultRoot = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  "..",
);
const ignoredDirectories = new Set([
  ".git",
  ".idea",
  ".pytest_cache",
  ".venv",
  "build",
  "dist",
  "node_modules",
  "target",
]);
const documentCategories = new Set([
  "architecture",
  "decision",
  "development",
  "domain",
  "fixed-entry",
  "generated-reference",
  "how-to",
  "navigation",
  "user",
]);
const documentLanguages = new Set(["en-US", "zh-CN"]);
const historicalStates = new Set(["immutable", "mutable"]);
const navigationKinds = new Set(["adr-index", "docs-index", "fixed-entry"]);
const chineseAuthorityCategories = new Set([
  "architecture",
  "development",
  "domain",
  "fixed-entry",
  "generated-reference",
  "how-to",
  "navigation",
  "user",
]);

function parseArguments(argv) {
  const result = { changed: [] };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === "--base") result.base = argv[++index];
    else if (argument === "--changed") result.changed.push(argv[++index]);
    else if (argument === "--docs-impact") result.docsImpact = argv[++index];
    else if (argument === "--reason") result.reason = argv[++index];
    else if (argument === "--root") result.root = argv[++index];
    else throw new Error(`unknown argument: ${argument}`);
  }
  return result;
}

function normalizeRepoPath(value) {
  return value.replaceAll("\\", "/").replace(/^\.\//u, "");
}

function resolveInsideRoot(repositoryRoot, repoPath) {
  const resolved = path.resolve(repositoryRoot, repoPath);
  if (
    resolved !== repositoryRoot &&
    !resolved.startsWith(`${repositoryRoot}${path.sep}`)
  ) {
    throw new Error(`path escapes repository root: ${repoPath}`);
  }
  return resolved;
}

function requireFile(repositoryRoot, repoPath, context) {
  if (!existsSync(resolveInsideRoot(repositoryRoot, repoPath))) {
    throw new Error(`${context} references missing target: ${repoPath}`);
  }
}

function validateStringList(value, context, { allowEmpty = false } = {}) {
  if (!Array.isArray(value) || (!allowEmpty && value.length === 0)) {
    throw new Error(
      `${context} must be ${allowEmpty ? "a" : "a non-empty"} list`,
    );
  }
  if (value.some((item) => typeof item !== "string" || item.trim() === "")) {
    throw new Error(`${context} contains an empty or non-string value`);
  }
}

function walkFiles(directory, predicate, files = []) {
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    if (entry.isDirectory() && ignoredDirectories.has(entry.name)) continue;
    const fullPath = path.join(directory, entry.name);
    if (entry.isDirectory()) walkFiles(fullPath, predicate, files);
    else if (entry.isFile() && predicate(entry.name)) files.push(fullPath);
  }
  return files;
}

function markdownPaths(repositoryRoot) {
  return walkFiles(repositoryRoot, (name) => name.endsWith(".md")).map(
    (fullPath) => normalizeRepoPath(path.relative(repositoryRoot, fullPath)),
  );
}

function matchesDocument(entry, repoPath) {
  if (entry.path) return repoPath === normalizeRepoPath(entry.path);
  return minimatch(repoPath, entry.glob, { dot: true });
}

function validateDocumentEntry(entry, repositoryRoot, generatedIds) {
  const context = `document entry ${String(entry?.id)}`;
  const selectors = [entry?.path, entry?.glob].filter(
    (value) => typeof value === "string",
  );
  if (selectors.length !== 1) {
    throw new Error(`${context} needs exactly one path or glob`);
  }
  const selector = normalizeRepoPath(selectors[0]);
  if (
    selector.startsWith("/") ||
    selector.includes("../") ||
    selector.includes("\\")
  ) {
    throw new Error(`${context} has an unsafe selector: ${selector}`);
  }
  if (entry.path) {
    if (selector !== entry.path || !selector.endsWith(".md")) {
      throw new Error(`${context} path must be a normalized Markdown path`);
    }
    requireFile(repositoryRoot, selector, context);
  } else if (
    selector !== entry.glob ||
    !/[?*[]/u.test(selector) ||
    !selector.endsWith(".md") ||
    !selector.startsWith("docs/")
  ) {
    throw new Error(`${context} glob is not a controlled Markdown glob`);
  }
  if (!documentCategories.has(entry.category)) {
    throw new Error(
      `${context} has invalid category: ${String(entry.category)}`,
    );
  }
  if (!documentLanguages.has(entry.language)) {
    throw new Error(
      `${context} has invalid language: ${String(entry.language)}`,
    );
  }
  if (!documentLanguages.has(entry.authority_language)) {
    throw new Error(
      `${context} has invalid authority_language: ${String(entry.authority_language)}`,
    );
  }
  if (!historicalStates.has(entry.historical)) {
    throw new Error(`${context} has invalid historical state`);
  }
  if (!navigationKinds.has(entry.navigation)) {
    throw new Error(`${context} has invalid navigation kind`);
  }
  if (entry.translation_of !== undefined) {
    if (!entry.path || typeof entry.translation_of !== "string") {
      throw new Error(`${context} translations require explicit paths`);
    }
    requireFile(repositoryRoot, entry.translation_of, context);
  }
  if (entry.generator !== undefined && !generatedIds.has(entry.generator)) {
    throw new Error(
      `${context} references unknown generator: ${String(entry.generator)}`,
    );
  }
}

function validateMap(config, repositoryRoot, allMarkdown) {
  if (config?.version !== 2) {
    throw new Error("documentation map version must be 2");
  }
  if (
    !Array.isArray(config.generated) ||
    !Array.isArray(config.documents) ||
    !Array.isArray(config.mappings)
  ) {
    throw new Error(
      "documentation map needs generated, documents and mappings lists",
    );
  }

  const generatedIds = new Set();
  for (const entry of config.generated) {
    if (typeof entry.id !== "string" || generatedIds.has(entry.id)) {
      throw new Error(`invalid or duplicate generated id: ${String(entry.id)}`);
    }
    generatedIds.add(entry.id);
    requireFile(repositoryRoot, entry.authority, `generated entry ${entry.id}`);
    validateStringList(entry.outputs, `generated entry ${entry.id} outputs`);
    entry.outputs.forEach((output) =>
      requireFile(repositoryRoot, output, `generated entry ${entry.id}`),
    );
    if (typeof entry.command !== "string" || typeof entry.check !== "string") {
      throw new Error(
        `generated entry ${entry.id} needs command and check strings`,
      );
    }
  }

  const documentIds = new Set();
  for (const entry of config.documents) {
    if (typeof entry.id !== "string" || documentIds.has(entry.id)) {
      throw new Error(`invalid or duplicate document id: ${String(entry.id)}`);
    }
    documentIds.add(entry.id);
    validateDocumentEntry(entry, repositoryRoot, generatedIds);
  }

  const inventory = new Map();
  const matchCount = new Map(config.documents.map((entry) => [entry.id, 0]));
  const inventoryErrors = [];
  for (const repoPath of allMarkdown) {
    const matches = config.documents.filter((entry) =>
      matchesDocument(entry, repoPath),
    );
    if (matches.length === 0) {
      inventoryErrors.push(`${repoPath}: unregistered Markdown document`);
      continue;
    }
    if (matches.length > 1) {
      inventoryErrors.push(
        `${repoPath}: registered by multiple entries (${matches.map((entry) => entry.id).join(", ")})`,
      );
      continue;
    }
    inventory.set(repoPath, matches[0]);
    matchCount.set(matches[0].id, matchCount.get(matches[0].id) + 1);
  }
  for (const [id, count] of matchCount) {
    if (count === 0)
      inventoryErrors.push(`${id}: document selector matches no files`);
  }
  if (inventoryErrors.length > 0) {
    throw new Error(
      `document inventory errors:\n${inventoryErrors.join("\n")}`,
    );
  }

  for (const entry of config.documents) {
    if (entry.translation_of === undefined) continue;
    const authorityPath = normalizeRepoPath(entry.translation_of);
    const authority = inventory.get(authorityPath);
    if (!authority) {
      throw new Error(`${entry.id}: translation authority is not registered`);
    }
    if (authority.translation_of !== undefined) {
      throw new Error(
        `${entry.id}: translation authority cannot be a translation`,
      );
    }
    if (
      entry.language === authority.language ||
      entry.authority_language !== authority.language ||
      authority.authority_language !== authority.language
    ) {
      throw new Error(
        `${entry.id}: inconsistent translation authority relation`,
      );
    }
  }

  for (const generated of config.generated) {
    for (const output of generated.outputs) {
      if (!output.endsWith(".md")) continue;
      const document = inventory.get(normalizeRepoPath(output));
      if (document?.generator !== generated.id) {
        throw new Error(
          `${output}: generated document must declare generator ${generated.id}`,
        );
      }
    }
  }

  const mappingIds = new Set();
  for (const mapping of config.mappings) {
    if (typeof mapping.id !== "string" || mappingIds.has(mapping.id)) {
      throw new Error(`invalid or duplicate mapping id: ${String(mapping.id)}`);
    }
    mappingIds.add(mapping.id);
    validateStringList(mapping.globs, `mapping ${mapping.id} globs`);
    validateStringList(mapping.required, `mapping ${mapping.id} required`);
    validateStringList(
      mapping.optional ?? [],
      `mapping ${mapping.id} optional`,
      {
        allowEmpty: true,
      },
    );
    validateStringList(
      mapping.generators ?? [],
      `mapping ${mapping.id} generators`,
      { allowEmpty: true },
    );
    validateStringList(mapping.checks ?? [], `mapping ${mapping.id} checks`, {
      allowEmpty: true,
    });
    [...mapping.required, ...(mapping.optional ?? [])].forEach((target) =>
      requireFile(repositoryRoot, target, `mapping ${mapping.id}`),
    );
    for (const generatorId of mapping.generators ?? []) {
      if (!generatedIds.has(generatorId)) {
        throw new Error(
          `mapping ${mapping.id} references unknown generator: ${generatorId}`,
        );
      }
    }
  }
  return inventory;
}

function headingAnchors(markdown) {
  const anchors = new Set();
  const counts = new Map();
  for (const line of markdown.split(/\r?\n/u)) {
    const match = /^(#{1,6})\s+(.+?)\s*#*$/u.exec(line);
    if (!match) continue;
    const base = match[2]
      .trim()
      .toLowerCase()
      .replace(/[`*_~]/gu, "")
      .replace(/[^\p{Letter}\p{Number}\s-]/gu, "")
      .replace(/\s+/gu, "-")
      .replace(/-+/gu, "-");
    const count = counts.get(base) ?? 0;
    counts.set(base, count + 1);
    anchors.add(count === 0 ? base : `${base}-${count}`);
  }
  return anchors;
}

function headingShape(markdown) {
  return [...markdown.matchAll(/^(#{1,6})\s+.+?\s*#*$/gmu)].map(
    (match) => match[1].length,
  );
}

function markdownLinkTargets(repositoryRoot, repoPath, markdown) {
  const targets = new Set();
  for (const match of markdown.matchAll(/!?\[[^\]]*\]\(([^)]+)\)/gu)) {
    let target = match[1].trim();
    if (target.startsWith("<") && target.endsWith(">")) {
      target = target.slice(1, -1);
    }
    if (/^(?:https?:|mailto:|data:|#)/iu.test(target)) continue;
    const [pathPart] = target.split("#", 1);
    if (!pathPart) continue;
    const resolved = path.resolve(
      path.dirname(resolveInsideRoot(repositoryRoot, repoPath)),
      decodeURIComponent(pathPart),
    );
    if (
      resolved === repositoryRoot ||
      resolved.startsWith(`${repositoryRoot}${path.sep}`)
    ) {
      targets.add(normalizeRepoPath(path.relative(repositoryRoot, resolved)));
    }
  }
  return targets;
}

function checkLinks(repositoryRoot, allMarkdown) {
  const errors = [];
  for (const repoPath of allMarkdown) {
    const markdownPath = resolveInsideRoot(repositoryRoot, repoPath);
    const text = readFileSync(markdownPath, "utf8");
    for (const match of text.matchAll(/!?\[[^\]]*\]\(([^)]+)\)/gu)) {
      let target = match[1].trim();
      if (target.startsWith("<") && target.endsWith(">")) {
        target = target.slice(1, -1);
      }
      if (/^(?:https?:|mailto:|data:)/iu.test(target)) continue;
      const [pathPart, fragment] = target.split("#", 2);
      const decodedPath = decodeURIComponent(pathPart);
      const resolved = decodedPath
        ? path.resolve(path.dirname(markdownPath), decodedPath)
        : markdownPath;
      if (!existsSync(resolved)) {
        errors.push(`${repoPath} -> missing ${target}`);
        continue;
      }
      if (fragment && path.extname(resolved).toLowerCase() === ".md") {
        const anchors = headingAnchors(readFileSync(resolved, "utf8"));
        if (!anchors.has(decodeURIComponent(fragment).toLowerCase())) {
          errors.push(`${repoPath} -> missing anchor ${target}`);
        }
      }
    }
  }
  if (errors.length > 0) {
    throw new Error(`documentation link errors:\n${errors.join("\n")}`);
  }
}

function checkImmutableChanges(inventory, changedEntries) {
  const errors = [];
  for (const { status, path: changedPath } of changedEntries) {
    const repoPath = normalizeRepoPath(changedPath);
    const document = inventory.get(repoPath);
    if (document?.historical === "immutable" && status !== "A") {
      errors.push(`${repoPath}: accepted historical document is immutable`);
    }
  }
  if (errors.length > 0) {
    throw new Error(`historical document errors:\n${errors.join("\n")}`);
  }
}

function checkInventory(
  repositoryRoot,
  allMarkdown,
  inventory,
  changedEntries,
) {
  const errors = [];
  const agentFiles = walkFiles(
    repositoryRoot,
    (name) => name === "AGENTS.md",
  ).map((fullPath) =>
    normalizeRepoPath(path.relative(repositoryRoot, fullPath)),
  );
  if (agentFiles.length > 0) {
    errors.push(
      `AGENTS.md files are not allowed in the repository; found: ${agentFiles.join(", ")}`,
    );
  }

  for (const [repoPath, entry] of inventory) {
    const markdown = readFileSync(
      resolveInsideRoot(repositoryRoot, repoPath),
      "utf8",
    );
    if (entry.language === "zh-CN" && !/[\u3400-\u9fff]{2}/u.test(markdown)) {
      errors.push(`${repoPath}: zh-CN document has no Chinese narrative`);
    }
    if (
      entry.translation_of === undefined &&
      chineseAuthorityCategories.has(entry.category) &&
      (entry.language !== "zh-CN" || entry.authority_language !== "zh-CN")
    ) {
      errors.push(`${repoPath}: this category requires a Chinese authority`);
    }
    if (
      entry.category === "decision" &&
      entry.translation_of === undefined &&
      entry.language === "en-US" &&
      entry.historical !== "immutable"
    ) {
      errors.push(
        `${repoPath}: an English ADR authority must be immutable history`,
      );
    }
    if (entry.translation_of !== undefined) {
      const authorityPath = normalizeRepoPath(entry.translation_of);
      const authorityMarkdown = readFileSync(
        resolveInsideRoot(repositoryRoot, authorityPath),
        "utf8",
      );
      const translationTargets = markdownLinkTargets(
        repositoryRoot,
        repoPath,
        markdown,
      );
      if (!translationTargets.has(authorityPath)) {
        errors.push(`${repoPath}: missing link to authority ${authorityPath}`);
      }
      const authority = inventory.get(authorityPath);
      if (authority?.historical !== "immutable") {
        const authorityTargets = markdownLinkTargets(
          repositoryRoot,
          authorityPath,
          authorityMarkdown,
        );
        if (!authorityTargets.has(repoPath)) {
          errors.push(
            `${authorityPath}: missing reciprocal translation link to ${repoPath}`,
          );
        }
      }
      if (
        JSON.stringify(headingShape(markdown)) !==
        JSON.stringify(headingShape(authorityMarkdown))
      ) {
        errors.push(
          `${repoPath}: heading structure differs from ${authorityPath}`,
        );
      }
    }
  }

  const indexPath = "docs/index.md";
  const reachable = new Set([indexPath]);
  const pending = [indexPath];
  while (pending.length > 0) {
    const current = pending.shift();
    const markdown = readFileSync(
      resolveInsideRoot(repositoryRoot, current),
      "utf8",
    );
    for (const target of markdownLinkTargets(
      repositoryRoot,
      current,
      markdown,
    )) {
      if (!inventory.has(target) || reachable.has(target)) continue;
      reachable.add(target);
      pending.push(target);
    }
  }
  for (const repoPath of allMarkdown) {
    if (
      repoPath.startsWith("docs/") &&
      repoPath !== indexPath &&
      !reachable.has(repoPath)
    ) {
      errors.push(`${repoPath}: not reachable from docs/index.md`);
    }
  }

  if (errors.length > 0) {
    throw new Error(`document governance errors:\n${errors.join("\n")}`);
  }
  checkImmutableChanges(inventory, changedEntries);
}

function checkGenerated(config, repositoryRoot) {
  for (const entry of config.generated) {
    const result = spawnSync(entry.check, {
      cwd: repositoryRoot,
      encoding: "utf8",
      shell: true,
      stdio: "pipe",
    });
    if (result.status !== 0) {
      throw new Error(
        `generated check failed for ${entry.id}:\n${result.stdout}${result.stderr}`,
      );
    }
  }
}

function changedFromGit(repositoryRoot, base) {
  const result = spawnSync("git", ["diff", "--name-status", `${base}...HEAD`], {
    cwd: repositoryRoot,
    encoding: "utf8",
  });
  if (result.status !== 0) {
    throw new Error(
      `unable to read changed files from ${base}: ${result.stderr.trim()}`,
    );
  }
  const entries = [];
  for (const line of result.stdout.split(/\r?\n/u).filter(Boolean)) {
    const [rawStatus, firstPath, secondPath] = line.split("\t");
    const status = rawStatus[0];
    entries.push({ status, path: secondPath ?? firstPath });
  }
  return entries;
}

function checkImpact(config, options, repositoryRoot, changedEntries) {
  const uniqueChanged = [
    ...new Set(
      [...options.changed, ...changedEntries.map((entry) => entry.path)].map(
        normalizeRepoPath,
      ),
    ),
  ];
  if (uniqueChanged.length === 0) return;

  const implementationChanged = uniqueChanged.some(
    (file) => !file.startsWith("docs/"),
  );
  if (!implementationChanged) return;

  const docsImpact = options.docsImpact ?? process.env.DOCS_IMPACT;
  const reason = options.reason ?? process.env.DOCS_IMPACT_REASON ?? "";
  if (!new Set(["required", "optional", "none"]).has(docsImpact)) {
    throw new Error(
      "implementation changes require docs impact: required, optional or none",
    );
  }

  const matched = config.mappings.filter((mapping) =>
    uniqueChanged.some((file) =>
      mapping.globs.some((glob) => minimatch(file, glob)),
    ),
  );
  const missing = matched.filter(
    (mapping) =>
      !mapping.required.some((target) => uniqueChanged.includes(target)),
  );

  if (docsImpact === "none") {
    const generic =
      /^(?:n\/?a|none|no docs?|not needed|required docs unchanged)\.?$/iu;
    if (reason.trim().length < 20 || generic.test(reason.trim())) {
      throw new Error(
        "docs-impact none requires a concrete reason of at least 20 characters",
      );
    }
    return;
  }

  if (missing.length > 0) {
    const details = missing
      .map((mapping) => `${mapping.id}: ${mapping.required.join(", ")}`)
      .join("\n");
    throw new Error(
      `mapped implementation changes lack required documentation:\n${details}`,
    );
  }
}

export function validateDocumentation(
  config,
  repositoryRoot,
  { changedEntries = [] } = {},
) {
  const resolvedRoot = path.resolve(repositoryRoot);
  const allMarkdown = markdownPaths(resolvedRoot);
  const inventory = validateMap(config, resolvedRoot, allMarkdown);
  checkLinks(resolvedRoot, allMarkdown);
  checkInventory(resolvedRoot, allMarkdown, inventory, changedEntries);
  return inventory;
}

export { checkImmutableChanges };

function main() {
  const options = parseArguments(process.argv.slice(2));
  const repositoryRoot = path.resolve(options.root ?? defaultRoot);
  const mapPath = path.join(
    repositoryRoot,
    "docs",
    "_meta",
    "documentation-map.yaml",
  );
  const config = parse(readFileSync(mapPath, "utf8"));
  const changedEntries = options.base
    ? changedFromGit(repositoryRoot, options.base)
    : [];
  validateDocumentation(config, repositoryRoot, { changedEntries });
  checkGenerated(config, repositoryRoot);
  checkImpact(config, options, repositoryRoot, changedEntries);
  console.log("documentation validation passed");
}

if (
  process.argv[1] &&
  path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
  try {
    main();
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 1;
  }
}

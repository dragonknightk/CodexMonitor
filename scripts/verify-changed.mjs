import { readFileSync } from "node:fs";
import path from "node:path";
import process from "node:process";
import { spawnSync } from "node:child_process";
import { performance } from "node:perf_hooks";
import { fileURLToPath } from "node:url";

import { minimatch } from "minimatch";
import { parse } from "yaml";

const defaultRoot = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  "..",
);

function normalizeRepoPath(value) {
  const normalized = value.replaceAll("\\", "/").replace(/^\.\//u, "");
  if (
    normalized === "" ||
    normalized.startsWith("/") ||
    normalized === ".." ||
    normalized.startsWith("../") ||
    normalized.includes("/../")
  ) {
    throw new Error(`invalid changed path: ${value}`);
  }
  return normalized;
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

export function validateVerificationMap(config) {
  if (config?.version !== 1) {
    throw new Error("verification map version must be 1");
  }
  if (
    !Array.isArray(config.stages) ||
    !Array.isArray(config.checks) ||
    !Array.isArray(config.full_checks) ||
    !Array.isArray(config.mappings)
  ) {
    throw new Error(
      "verification map needs stages, checks, full_checks and mappings lists",
    );
  }

  const stageIds = new Set();
  for (const stage of config.stages) {
    if (
      typeof stage.id !== "string" ||
      stageIds.has(stage.id) ||
      typeof stage.label !== "string" ||
      stage.label.trim() === ""
    ) {
      throw new Error(
        `invalid or duplicate verification stage: ${String(stage.id)}`,
      );
    }
    stageIds.add(stage.id);
  }

  const checkIds = new Set();
  const coversById = new Map();
  for (const check of config.checks) {
    if (typeof check.id !== "string" || checkIds.has(check.id)) {
      throw new Error(
        `invalid or duplicate verification check: ${String(check.id)}`,
      );
    }
    if (!stageIds.has(check.stage)) {
      throw new Error(`${check.id}: unknown stage ${String(check.stage)}`);
    }
    if (typeof check.command !== "string" || check.command.trim() === "") {
      throw new Error(`${check.id}: command must be a non-empty string`);
    }
    if (check.covers !== undefined) {
      validateStringList(check.covers, `${check.id} covers`);
      if (new Set(check.covers).size !== check.covers.length) {
        throw new Error(`${check.id}: covers contains duplicate checks`);
      }
    }
    checkIds.add(check.id);
    coversById.set(check.id, check.covers ?? []);
  }
  for (const [checkId, coveredIds] of coversById) {
    for (const coveredId of coveredIds) {
      if (!checkIds.has(coveredId)) {
        throw new Error(`${checkId}: covers unknown check ${coveredId}`);
      }
      if (coveredId === checkId) {
        throw new Error(`${checkId}: cannot cover itself`);
      }
    }
  }

  validateStringList(config.full_checks, "full_checks");
  if (new Set(config.full_checks).size !== config.full_checks.length) {
    throw new Error("full_checks contains duplicate checks");
  }
  for (const checkId of config.full_checks) {
    if (!checkIds.has(checkId)) {
      throw new Error(`full_checks references unknown check: ${checkId}`);
    }
  }

  const mappingIds = new Set();
  for (const mapping of config.mappings) {
    if (typeof mapping.id !== "string" || mappingIds.has(mapping.id)) {
      throw new Error(
        `invalid or duplicate verification mapping: ${String(mapping.id)}`,
      );
    }
    mappingIds.add(mapping.id);
    validateStringList(mapping.globs, `${mapping.id} globs`);
    validateStringList(mapping.checks, `${mapping.id} checks`, {
      allowEmpty: true,
    });
    if (
      typeof mapping.reason !== "string" ||
      mapping.reason.trim().length < 8
    ) {
      throw new Error(`${mapping.id}: reason must explain the selection`);
    }
    for (const checkId of mapping.checks) {
      if (!checkIds.has(checkId)) {
        throw new Error(`${mapping.id}: unknown check ${checkId}`);
      }
    }
    if (mapping.upgrade !== undefined) {
      if (
        typeof mapping.upgrade.condition !== "string" ||
        mapping.upgrade.condition.trim().length < 8
      ) {
        throw new Error(`${mapping.id}: upgrade needs a concrete condition`);
      }
      validateStringList(
        mapping.upgrade.checks,
        `${mapping.id} upgrade checks`,
      );
      for (const checkId of mapping.upgrade.checks) {
        if (!checkIds.has(checkId)) {
          throw new Error(`${mapping.id}: unknown upgrade check ${checkId}`);
        }
      }
    }
  }
  return config;
}

function selectionOrder(config) {
  const stageOrder = new Map(
    config.stages.map((stage, index) => [stage.id, index]),
  );
  const checkOrder = new Map(
    config.checks.map((check, index) => [check.id, index]),
  );
  return (left, right) => {
    const stageDifference =
      stageOrder.get(left.stage) - stageOrder.get(right.stage);
    return (
      stageDifference || checkOrder.get(left.id) - checkOrder.get(right.id)
    );
  };
}

function addReason(reasonMap, checkId, mapping, repoPath, upgraded) {
  const reasons = reasonMap.get(checkId) ?? [];
  reasons.push({
    mapping: mapping.id,
    path: repoPath,
    reason: upgraded ? mapping.upgrade.condition : mapping.reason,
    upgraded,
  });
  reasonMap.set(checkId, reasons);
}

function coverageCycles(config) {
  const checksById = new Map(config.checks.map((check) => [check.id, check]));
  const cyclic = new Set();
  const visiting = new Set();
  const visited = new Set();
  const visit = (checkId, ancestry) => {
    if (visiting.has(checkId)) {
      for (const ancestor of ancestry.slice(ancestry.indexOf(checkId))) {
        cyclic.add(ancestor);
      }
      return;
    }
    if (visited.has(checkId)) return;
    visiting.add(checkId);
    const nextAncestry = [...ancestry, checkId];
    for (const coveredId of checksById.get(checkId).covers ?? []) {
      visit(coveredId, nextAncestry);
    }
    visiting.delete(checkId);
    visited.add(checkId);
  };
  for (const check of config.checks) visit(check.id, []);
  return cyclic;
}

function applyCoverage(config, selectedIds) {
  const selected = new Set(selectedIds);
  const cyclic = coverageCycles(config);
  const coveredBy = new Map();
  for (const coveringCheck of config.checks) {
    if (!selected.has(coveringCheck.id) || cyclic.has(coveringCheck.id))
      continue;
    for (const coveredId of coveringCheck.covers ?? []) {
      if (!selected.has(coveredId) || cyclic.has(coveredId)) continue;
      const covering = coveredBy.get(coveredId) ?? [];
      covering.push(coveringCheck.id);
      coveredBy.set(coveredId, covering);
    }
  }
  const covers = new Map();
  for (const [coveredId, coveringIds] of coveredBy) {
    for (const coveringId of coveringIds) {
      const covered = covers.get(coveringId) ?? [];
      covered.push(coveredId);
      covers.set(coveringId, covered);
    }
  }
  return {
    selectedIds: [...selected].filter((checkId) => !coveredBy.has(checkId)),
    covers,
  };
}

export function selectChecks(config, changedPaths, { full = false } = {}) {
  validateVerificationMap(config);
  const checksById = new Map(config.checks.map((check) => [check.id, check]));
  if (full) {
    const coverage = applyCoverage(config, config.full_checks);
    return coverage.selectedIds
      .map((id) => ({
        ...checksById.get(id),
        covered: coverage.covers.get(id) ?? [],
        reasons: [
          {
            mapping: "full",
            path: "*",
            reason: "完整验证门禁",
            upgraded: true,
          },
        ],
      }))
      .sort(selectionOrder(config));
  }

  const uniquePaths = [...new Set(changedPaths.map(normalizeRepoPath))];
  const selectedIds = new Set();
  const reasons = new Map();
  const unknown = [];
  for (const repoPath of uniquePaths) {
    const mappings = config.mappings.filter((mapping) =>
      mapping.globs.some((glob) => minimatch(repoPath, glob, { dot: true })),
    );
    if (mappings.length === 0) {
      unknown.push(repoPath);
      continue;
    }
    for (const mapping of mappings) {
      for (const checkId of mapping.checks) {
        selectedIds.add(checkId);
        addReason(reasons, checkId, mapping, repoPath, false);
      }
      for (const checkId of mapping.upgrade?.checks ?? []) {
        selectedIds.add(checkId);
        addReason(reasons, checkId, mapping, repoPath, true);
      }
    }
  }
  if (unknown.length > 0) {
    throw new Error(
      `verification map has no owner for:\n${unknown.map((value) => `- ${value}`).join("\n")}`,
    );
  }
  const coverage = applyCoverage(config, selectedIds);
  return coverage.selectedIds
    .map((id) => ({
      ...checksById.get(id),
      covered: coverage.covers.get(id) ?? [],
      reasons: reasons.get(id),
    }))
    .sort(selectionOrder(config));
}

function gitLines(repositoryRoot, args) {
  const result = spawnSync("git", args, {
    cwd: repositoryRoot,
    encoding: "utf8",
  });
  if (result.status !== 0) {
    throw new Error(
      `git ${args.join(" ")} failed: ${result.stderr.trim() || result.stdout.trim()}`,
    );
  }
  return result.stdout.split(/\r?\n/u).filter(Boolean);
}

function changedFromGit(repositoryRoot, base) {
  if (base) {
    return gitLines(repositoryRoot, ["diff", "--name-only", `${base}...HEAD`]);
  }
  return [
    ...gitLines(repositoryRoot, ["diff", "--name-only", "HEAD"]),
    ...gitLines(repositoryRoot, ["ls-files", "--others", "--exclude-standard"]),
  ];
}

function parseArguments(argv) {
  const options = { changed: [] };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === "--base") options.base = argv[++index];
    else if (argument === "--changed") options.changed.push(argv[++index]);
    else if (argument === "--dry-run") options.dryRun = true;
    else if (argument === "--full") options.full = true;
    else if (argument === "--map") options.map = argv[++index];
    else throw new Error(`unknown argument: ${argument}`);
  }
  if (options.full && (options.base || options.changed.length > 0)) {
    throw new Error("--full cannot be combined with --base or --changed");
  }
  return options;
}

export function formatDuration(milliseconds) {
  return `${(milliseconds / 1_000).toFixed(2)}s`;
}

function printSelection(selection, write) {
  for (const check of selection) {
    const grouped = new Map();
    for (const reason of check.reasons) {
      const key = `${reason.mapping}\u0000${reason.reason}`;
      const value = grouped.get(key) ?? { ...reason, paths: [] };
      value.paths.push(reason.path);
      grouped.set(key, value);
    }
    const reasons = [...grouped.values()].map((reason) => {
      const paths = [...new Set(reason.paths)];
      const scope =
        paths.length <= 3
          ? paths.join(", ")
          : `${paths.slice(0, 3).join(", ")} 等 ${String(paths.length)} 个路径`;
      return `${reason.reason}（${reason.mapping}: ${scope}）`;
    });
    write(`[选择] ${check.id} — ${reasons.join("；")}`);
    if (check.covered.length > 0) {
      write(`[覆盖] ${check.id} — 已覆盖 ${check.covered.join("、")}`);
    }
  }
}

export function executeSelection(
  config,
  selection,
  {
    repositoryRoot = defaultRoot,
    dryRun = false,
    write = console.log,
    execute = (command) =>
      spawnSync(command, {
        cwd: repositoryRoot,
        env: process.env,
        shell: true,
        stdio: "inherit",
      }),
    now = () => performance.now(),
  } = {},
) {
  printSelection(selection, write);
  if (dryRun || selection.length === 0) return { durationMs: 0, checks: [] };

  const results = [];
  const totalStarted = now();
  for (const stage of config.stages) {
    const stageChecks = selection.filter((check) => check.stage === stage.id);
    if (stageChecks.length === 0) continue;
    const stageStarted = now();
    write(`[阶段] ${stage.label}（${stageChecks.length} 项）`);
    for (const check of stageChecks) {
      const started = now();
      const result = execute(check.command);
      const durationMs = now() - started;
      results.push({ id: check.id, durationMs, status: result.status });
      if (result.status !== 0) {
        write(`[失败] ${check.id}，耗时 ${formatDuration(durationMs)}`);
        throw new Error(
          `${check.id} failed with exit code ${String(result.status)}`,
        );
      }
      write(`[通过] ${check.id}，耗时 ${formatDuration(durationMs)}`);
    }
    write(
      `[阶段完成] ${stage.label}，耗时 ${formatDuration(now() - stageStarted)}`,
    );
  }
  const durationMs = now() - totalStarted;
  write(`[验证完成] ${results.length} 项，累计 ${formatDuration(durationMs)}`);
  return { durationMs, checks: results };
}

function main() {
  const options = parseArguments(process.argv.slice(2));
  const repositoryRoot = defaultRoot;
  const mapPath = path.resolve(
    repositoryRoot,
    options.map ?? "docs/_meta/verification-map.yaml",
  );
  const config = validateVerificationMap(parse(readFileSync(mapPath, "utf8")));
  const changed = options.full
    ? []
    : options.changed.length > 0
      ? options.changed
      : changedFromGit(repositoryRoot, options.base);
  const selection = selectChecks(config, changed, { full: options.full });
  if (!options.full && changed.length === 0) {
    console.log("没有待验证的 Git 改动。");
    return;
  }
  executeSelection(config, selection, {
    repositoryRoot,
    dryRun: options.dryRun,
  });
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

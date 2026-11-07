import { execFileSync } from "node:child_process";
import { readFileSync, readdirSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const EXPECTED_PACKAGES = new Set([
  "codex-monitor-core",
  "codex-monitor-storage",
  "codex-monitor-telemetry",
  "codex-monitor-dashboard",
  "codex-monitor-engine",
  "codex-monitor-host",
  "codex-monitor-desktop",
]);

const ALLOWED_WORKSPACE_DEPENDENCIES = new Map([
  ["codex-monitor-core", new Set()],
  ["codex-monitor-storage", new Set(["codex-monitor-core"])],
  ["codex-monitor-telemetry", new Set(["codex-monitor-core"])],
  ["codex-monitor-dashboard", new Set(["codex-monitor-core"])],
  [
    "codex-monitor-engine",
    new Set([
      "codex-monitor-core",
      "codex-monitor-storage",
      "codex-monitor-telemetry",
      "codex-monitor-dashboard",
    ]),
  ],
  ["codex-monitor-host", new Set(["codex-monitor-core"])],
  [
    "codex-monitor-desktop",
    new Set([
      "codex-monitor-core",
      "codex-monitor-storage",
      "codex-monitor-telemetry",
      "codex-monitor-dashboard",
      "codex-monitor-engine",
      "codex-monitor-host",
    ]),
  ],
]);

const FORBIDDEN_DIRECT_DEPENDENCIES = new Map([
  [
    "codex-monitor-core",
    new Set([
      "axum",
      "http",
      "opentelemetry",
      "opentelemetry-proto",
      "rusqlite",
      "serde_json",
      "tauri",
    ]),
  ],
  [
    "codex-monitor-storage",
    new Set(["axum", "opentelemetry", "opentelemetry-proto", "tauri"]),
  ],
  ["codex-monitor-telemetry", new Set(["rusqlite", "tauri"])],
  [
    "codex-monitor-dashboard",
    new Set(["opentelemetry", "opentelemetry-proto", "rusqlite", "tauri"]),
  ],
  [
    "codex-monitor-host",
    new Set([
      "axum",
      "opentelemetry",
      "opentelemetry-proto",
      "rusqlite",
      "tauri",
    ]),
  ],
  [
    "codex-monitor-engine",
    new Set(["axum", "opentelemetry-proto", "rusqlite", "tauri"]),
  ],
]);

function productionDependencies(pkg) {
  return pkg.dependencies.filter(
    (dependency) => dependency.kind === null || dependency.kind === "build",
  );
}

export function validateCrateBoundaries(metadata) {
  const workspaceIds = new Set(metadata.workspace_members);
  const packages = metadata.packages.filter((pkg) => workspaceIds.has(pkg.id));
  const byName = new Map(packages.map((pkg) => [pkg.name, pkg]));
  const errors = [];

  for (const expected of EXPECTED_PACKAGES) {
    if (!byName.has(expected)) {
      errors.push(`缺少 workspace package：${expected}`);
    }
  }
  for (const name of byName.keys()) {
    if (!EXPECTED_PACKAGES.has(name)) {
      errors.push(`未登记的 workspace package：${name}`);
    }
  }

  const graph = new Map();
  for (const [name, pkg] of byName) {
    const production = productionDependencies(pkg);
    const workspaceDependencies = production
      .map((dependency) => dependency.name)
      .filter((dependency) => byName.has(dependency));
    graph.set(name, workspaceDependencies);

    const allowed = ALLOWED_WORKSPACE_DEPENDENCIES.get(name) ?? new Set();
    for (const dependency of workspaceDependencies) {
      if (!allowed.has(dependency)) {
        errors.push(`${name} 不得依赖 workspace package ${dependency}`);
      }
    }

    const forbidden = FORBIDDEN_DIRECT_DEPENDENCIES.get(name) ?? new Set();
    for (const dependency of production) {
      if (forbidden.has(dependency.name)) {
        errors.push(`${name} 不得直接依赖 ${dependency.name}`);
      }
      if (
        name !== "codex-monitor-desktop" &&
        dependency.name.startsWith("tauri")
      ) {
        errors.push(`${name} 不得引入 Tauri package ${dependency.name}`);
      }
    }
  }

  if (byName.has("codex-monitor-desktop")) {
    const dependencies = new Set(
      productionDependencies(byName.get("codex-monitor-desktop")).map(
        (dependency) => dependency.name,
      ),
    );
    for (const required of ["codex-monitor-engine", "codex-monitor-host"]) {
      if (!dependencies.has(required)) {
        errors.push(`codex-monitor-desktop 必须依赖 ${required}`);
      }
    }
  }

  const visiting = new Set();
  const visited = new Set();
  const visit = (name, trail) => {
    if (visiting.has(name)) {
      errors.push(`workspace Cargo 依赖成环：${[...trail, name].join(" -> ")}`);
      return;
    }
    if (visited.has(name)) return;
    visiting.add(name);
    for (const dependency of graph.get(name) ?? []) {
      visit(dependency, [...trail, name]);
    }
    visiting.delete(name);
    visited.add(name);
  };
  for (const name of graph.keys()) visit(name, []);

  return errors;
}

export function validateDesktopOnlyConfiguration({
  manifest,
  repositoryPaths,
  sourceContents = [],
  workflowContents,
}) {
  const errors = [];
  const libSection = /\[lib\]([\s\S]*?)(?=\n\[|$)/u.exec(manifest)?.[1] ?? "";
  if (!/^name\s*=\s*"codex_monitor_desktop"\s*$/mu.test(libSection)) {
    errors.push("codex-monitor-desktop 必须保留默认 Rust library 名称");
  }
  if (/^crate-type\s*=/mu.test(libSection)) {
    errors.push("codex-monitor-desktop 不得声明移动端 crate-type");
  }
  const mobilePath = /(^|\/)(?:android|ios)(?:\/|\.|-|$)/iu;
  for (const repoPath of repositoryPaths) {
    const normalized = repoPath.replaceAll("\\", "/");
    if (
      !normalized.startsWith("src-tauri/icons/") &&
      mobilePath.test(normalized)
    ) {
      errors.push(`codex-monitor-desktop 不得保留移动端路径：${repoPath}`);
    }
  }
  const mobileWorkflow =
    /(?:tauri\s+(?:android|ios)|mobile_entry_point|target_os\s*=\s*"(?:android|ios)"|(?:aarch64|x86_64)-(?:linux-android|apple-ios))/iu;
  for (const [name, contents] of workflowContents) {
    if (mobileWorkflow.test(contents)) {
      errors.push(`桌面发布流水线不得包含移动端入口或 target：${name}`);
    }
  }
  for (const [name, contents] of sourceContents) {
    if (mobileWorkflow.test(contents)) {
      errors.push(`桌面源码不得包含移动端入口或 target：${name}`);
    }
  }
  return errors;
}

function repositoryFiles(directory, prefix = "") {
  const files = [];
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    const relative = prefix ? `${prefix}/${entry.name}` : entry.name;
    if (entry.isDirectory())
      files.push(
        ...repositoryFiles(path.join(directory, entry.name), relative),
      );
    else if (entry.isFile()) files.push(relative);
  }
  return files;
}

function loadDesktopOnlyConfiguration(root) {
  const workflowsRoot = path.join(root, ".github", "workflows");
  const workflowPaths = repositoryFiles(workflowsRoot);
  const desktopRoot = path.join(root, "apps", "desktop");
  const desktopPaths = repositoryFiles(desktopRoot);
  return {
    manifest: readFileSync(
      path.join(root, "apps", "desktop", "src-tauri", "Cargo.toml"),
      "utf8",
    ),
    repositoryPaths: desktopPaths,
    sourceContents: desktopPaths
      .filter(
        (name) =>
          !name.startsWith("src-tauri/icons/") &&
          /\.(?:rs|json|toml)$/iu.test(name),
      )
      .map((name) => [
        name,
        readFileSync(path.join(desktopRoot, name), "utf8"),
      ]),
    workflowContents: workflowPaths.map((name) => [
      name,
      readFileSync(path.join(workflowsRoot, name), "utf8"),
    ]),
  };
}

function loadMetadata(root) {
  return JSON.parse(
    execFileSync("cargo", ["metadata", "--format-version", "1", "--no-deps"], {
      cwd: root,
      encoding: "utf8",
      windowsHide: true,
    }),
  );
}

function main() {
  const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
  const errors = [
    ...validateCrateBoundaries(loadMetadata(root)),
    ...validateDesktopOnlyConfiguration(loadDesktopOnlyConfiguration(root)),
  ];
  if (errors.length > 0) {
    for (const error of errors) console.error(`crate-boundary: ${error}`);
    process.exitCode = 1;
    return;
  }
  console.log("crate boundaries verified");
}

if (
  process.argv[1] &&
  path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
  main();
}

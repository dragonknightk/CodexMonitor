import { appendFileSync } from "node:fs";
import path from "node:path";
import process from "node:process";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

import { minimatch } from "minimatch";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
export const ciCategories = [
  "rust",
  "native_host",
  "web",
  "docs",
  "dependency_security",
  "release",
  "tooling",
];

const rules = [
  {
    globs: ["crates/**"],
    categories: ["rust"],
  },
  {
    globs: ["apps/desktop/**"],
    categories: ["rust", "native_host"],
  },
  {
    globs: ["web/**", "api/**"],
    categories: ["web"],
  },
  {
    globs: [
      "docs/**",
      "README.md",
      "README.en.md",
      "README.zh-CN.md",
      "PRIVACY.md",
      "PRIVACY.zh-CN.md",
      "LICENSE",
      ".github/PULL_REQUEST_TEMPLATE.md",
    ],
    categories: ["docs"],
  },
  {
    globs: ["fixtures/**", "contracts/**"],
    categories: ["rust", "docs", "tooling"],
  },
  {
    globs: ["scripts/**"],
    excludes: ["scripts/release/**", "scripts/installed-artifact-smoke.mjs"],
    categories: ["tooling"],
  },
  {
    globs: [
      "scripts/check-docs.mjs",
      "scripts/generate-*.mjs",
      "scripts/read-docs-impact.mjs",
      "scripts/tests/documentation-*.test.mjs",
    ],
    categories: ["docs"],
  },
  {
    globs: [
      "scripts/check-privacy-surfaces.mjs",
      "scripts/check-frontend-licenses.mjs",
      "scripts/tests/*privacy*.test.mjs",
      ".gitleaks.toml",
      "deny.toml",
    ],
    categories: ["dependency_security"],
  },
  {
    globs: [".github/workflows/**"],
    excludes: [
      ".github/workflows/release.yml",
      ".github/workflows/package-unsigned.yml",
    ],
    categories: ["tooling"],
  },
  {
    globs: [
      ".github/workflows/release.yml",
      ".github/workflows/package-unsigned.yml",
      "scripts/release/**",
      "scripts/installed-artifact-smoke.mjs",
      "assets/**",
      "apps/desktop/src-tauri/tauri.conf.json",
    ],
    categories: ["release", "native_host"],
  },
  {
    globs: ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "rustfmt.toml"],
    categories: ["rust", "dependency_security", "tooling"],
  },
  {
    globs: ["package.json", "pnpm-lock.yaml", "pnpm-workspace.yaml", ".npmrc"],
    categories: ["web", "dependency_security", "tooling"],
  },
  {
    globs: ["assets/**"],
    categories: ["native_host", "release"],
  },
  {
    globs: [".editorconfig", ".gitattributes", ".gitignore", ".prettierignore"],
    categories: ["tooling"],
  },
];

function normalizeRepoPath(value) {
  return value.replaceAll("\\", "/").replace(/^\.\//u, "");
}

export function classifyPaths(paths, { forceFull = false } = {}) {
  const result = Object.fromEntries(ciCategories.map((name) => [name, false]));
  const unknown = [];
  const uniquePaths = [
    ...new Set(paths.map(normalizeRepoPath).filter(Boolean)),
  ];
  for (const repoPath of uniquePaths) {
    const matches = rules.filter(
      (rule) =>
        rule.globs.some((glob) => minimatch(repoPath, glob, { dot: true })) &&
        !(rule.excludes ?? []).some((glob) =>
          minimatch(repoPath, glob, { dot: true }),
        ),
    );
    if (matches.length === 0) {
      unknown.push(repoPath);
      continue;
    }
    for (const rule of matches) {
      for (const category of rule.categories) result[category] = true;
    }
  }
  if (unknown.length > 0) {
    throw new Error(
      `CI classification has no owner for:\n${unknown.map((value) => `- ${value}`).join("\n")}`,
    );
  }
  return {
    ...result,
    full: forceFull,
    has_changes: uniquePaths.length > 0,
    paths: uniquePaths,
  };
}

function parseArguments(argv) {
  const options = { changed: [] };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === "--base") options.base = argv[++index];
    else if (argument === "--head") options.head = argv[++index];
    else if (argument === "--changed") options.changed.push(argv[++index]);
    else if (argument === "--full") options.full = true;
    else throw new Error(`unknown argument: ${argument}`);
  }
  return options;
}

function changedFromGit(base, head) {
  if (!base || !head) throw new Error("classification needs --base and --head");
  const result = spawnSync(
    "git",
    ["diff", "--name-only", `${base}...${head}`],
    {
      cwd: root,
      encoding: "utf8",
    },
  );
  if (result.status !== 0) {
    throw new Error(result.stderr.trim() || "unable to classify Git diff");
  }
  return result.stdout.split(/\r?\n/u).filter(Boolean);
}

function writeOutputs(classification) {
  const lines = [...ciCategories, "full", "has_changes"].map(
    (name) => `${name}=${String(classification[name])}`,
  );
  if (process.env.GITHUB_OUTPUT) {
    appendFileSync(process.env.GITHUB_OUTPUT, `${lines.join("\n")}\n`, "utf8");
  }
  for (const line of lines) console.log(line);
  console.log(`changed_paths=${String(classification.paths.length)}`);
}

function main() {
  const options = parseArguments(process.argv.slice(2));
  const changed =
    options.changed.length > 0
      ? options.changed
      : options.full || process.env.FORCE_FULL === "true"
        ? []
        : changedFromGit(options.base, options.head);
  writeOutputs(
    classifyPaths(changed, {
      forceFull: options.full || process.env.FORCE_FULL === "true",
    }),
  );
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

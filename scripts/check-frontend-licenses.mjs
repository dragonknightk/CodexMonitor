import { spawnSync } from "node:child_process";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const allowed = new Set([
  "(MIT OR CC0-1.0)",
  "Apache-2.0",
  "Apache-2.0 OR MIT",
  "BSD-2-Clause",
  "BSD-3-Clause",
  "CC0-1.0",
  "ISC",
  "MIT",
  "MIT-0",
  "MPL-2.0",
]);
const acceptedExceptions = new Map([
  ["BlueOak-1.0.0", new Set(["lru-cache", "minimatch"])],
  ["CC-BY-4.0", new Set(["caniuse-lite"])],
  ["Python-2.0", new Set(["argparse"])],
]);

function pnpmLicenses() {
  const npmExecPath = process.env.npm_execpath;
  let command;
  let arguments_;
  if (npmExecPath !== undefined) {
    command = process.execPath;
    arguments_ = [npmExecPath, "licenses", "list", "--json"];
  } else if (process.platform === "win32") {
    command = process.env.ComSpec ?? "cmd.exe";
    arguments_ = ["/d", "/s", "/c", "pnpm licenses list --json"];
  } else {
    command = "pnpm";
    arguments_ = ["licenses", "list", "--json"];
  }
  const result = spawnSync(command, arguments_, {
    cwd: root,
    encoding: "utf8",
  });
  if (result.status !== 0) {
    throw new Error(result.stderr || "pnpm licenses list failed");
  }
  return JSON.parse(result.stdout);
}

function main() {
  const licenses = pnpmLicenses();
  const rejected = [];
  let packageCount = 0;
  for (const [license, packages] of Object.entries(licenses)) {
    if (!Array.isArray(packages)) {
      throw new Error(`license group ${license} is not a package list`);
    }
    packageCount += packages.length;
    if (allowed.has(license)) continue;
    const exceptions = acceptedExceptions.get(license);
    for (const package_ of packages) {
      if (exceptions?.has(package_.name) !== true) {
        rejected.push(
          `${package_.name}@${package_.versions.join(",")} (${license})`,
        );
      }
    }
  }
  if (rejected.length > 0) {
    throw new Error(`unreviewed frontend licenses:\n${rejected.join("\n")}`);
  }
  console.log(
    `frontend licenses verified: ${packageCount} package records, ${allowed.size} allowed expressions, ${acceptedExceptions.size} reviewed exception groups`,
  );
}

try {
  main();
} catch (error) {
  console.error(error instanceof Error ? error.message : String(error));
  process.exitCode = 1;
}

#!/usr/bin/env node

import { copyFileSync } from "node:fs";
import path from "node:path";

import {
  ensureEmptyDirectory,
  expectedReleaseFileNames,
  listRegularFiles,
  parseArguments,
  readProductMetadata,
  requireStringArgument,
} from "./release-contract.mjs";

try {
  const args = parseArguments(process.argv.slice(2));
  const input = path.resolve(requireStringArgument(args, "input"));
  const output = path.resolve(requireStringArgument(args, "output"));
  const version = requireStringArgument(args, "version");
  const configuredVersion = readProductMetadata().version;
  if (version !== configuredVersion) {
    throw new Error(
      `Collected release version ${version} does not match configured ${configuredVersion}`,
    );
  }

  const expectedNames = expectedReleaseFileNames(version);
  const expected = new Set(expectedNames);
  const sources = new Map();
  for (const source of listRegularFiles(input)) {
    const name = path.basename(source);
    if (!expected.has(name))
      throw new Error(`Unexpected release input: ${name}`);
    if (sources.has(name)) throw new Error(`Duplicate release input: ${name}`);
    sources.set(name, source);
  }
  const missing = expectedNames.filter((name) => !sources.has(name));
  if (missing.length > 0) {
    throw new Error(`Missing release inputs: ${missing.join(", ")}`);
  }

  ensureEmptyDirectory(output);
  for (const name of expectedNames) {
    copyFileSync(sources.get(name), path.join(output, name));
  }
  process.stdout.write(`${expectedNames.length} release inputs collected\n`);
} catch (error) {
  process.stderr.write(
    `${error instanceof Error ? error.message : String(error)}\n`,
  );
  process.exitCode = 1;
}

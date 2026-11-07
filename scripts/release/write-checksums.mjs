#!/usr/bin/env node

import { existsSync, mkdirSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

import {
  compareReleaseNames,
  listRegularFiles,
  parseArguments,
  relativePosix,
  requireStringArgument,
  sha256File,
} from "./release-contract.mjs";

export function writeChecksums(root, output) {
  const absoluteRoot = path.resolve(root);
  const absoluteOutput = path.resolve(output);
  const outputRelative = path.relative(absoluteRoot, absoluteOutput);
  if (
    !outputRelative ||
    outputRelative.startsWith("..") ||
    path.isAbsolute(outputRelative)
  ) {
    throw new Error("Checksum output must be a file below --root");
  }
  if (existsSync(absoluteOutput)) {
    throw new Error(`Checksum output already exists: ${absoluteOutput}`);
  }
  const files = listRegularFiles(absoluteRoot)
    .filter((filePath) => path.resolve(filePath) !== absoluteOutput)
    .map((filePath) => ({
      filePath,
      relative: relativePosix(absoluteRoot, filePath),
    }))
    .sort((left, right) => compareReleaseNames(left.relative, right.relative));
  if (files.length === 0)
    throw new Error("No release files are available to hash");
  const contents = files
    .map(({ filePath, relative }) => `${sha256File(filePath)}  ${relative}`)
    .join("\n");
  mkdirSync(path.dirname(absoluteOutput), { recursive: true });
  writeFileSync(absoluteOutput, `${contents}\n`, {
    encoding: "utf8",
    flag: "wx",
  });
  return files.map(({ relative }) => relative);
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  try {
    const args = parseArguments(process.argv.slice(2));
    writeChecksums(
      requireStringArgument(args, "root"),
      requireStringArgument(args, "output"),
    );
  } catch (error) {
    process.stderr.write(
      `${error instanceof Error ? error.message : String(error)}\n`,
    );
    process.exitCode = 1;
  }
}

#!/usr/bin/env node

import { readFileSync } from "node:fs";
import path from "node:path";
import { isDeepStrictEqual } from "node:util";

import {
  buildManifest,
  compareReleaseNames,
  expectedReleaseFileNames,
  findSupportedTarget,
  globalChecksumName,
  installerNames,
  inspectWindowsPortableZip,
  listRegularFiles,
  manifestName,
  parseArguments,
  readProductMetadata,
  relativePosix,
  requireStringArgument,
  sbomName,
  sha256File,
  supportedTargets,
  targetChecksumName,
} from "./release-contract.mjs";

function assertExactFiles(root, expectedRelativePaths) {
  const actual = listRegularFiles(root)
    .map((filePath) => relativePosix(root, filePath))
    .sort(compareReleaseNames);
  const expected = [...expectedRelativePaths].sort(compareReleaseNames);
  if (!isDeepStrictEqual(actual, expected)) {
    throw new Error(
      `Release file allowlist mismatch\nexpected: ${expected.join(", ")}\nactual: ${actual.join(", ")}`,
    );
  }
}

function validateManifest(filePath, version, channel, targetDefinition) {
  const actual = JSON.parse(readFileSync(filePath, "utf8"));
  const expected = buildManifest(version, channel, targetDefinition);
  if (!isDeepStrictEqual(actual, expected)) {
    throw new Error(
      `Release manifest does not match its target contract: ${path.basename(filePath)}`,
    );
  }
}

function validateSbom(filePath) {
  const sbom = JSON.parse(readFileSync(filePath, "utf8"));
  if (
    sbom?.bomFormat !== "CycloneDX" ||
    typeof sbom.specVersion !== "string" ||
    !/^\d+\.\d+$/u.test(sbom.specVersion)
  ) {
    throw new Error(`SBOM is not CycloneDX JSON: ${path.basename(filePath)}`);
  }
}

function validatePayloads(root, version, channel, targetDefinition) {
  if (targetDefinition.platform !== "windows") return;
  const [portable] = installerNames(version, channel, targetDefinition);
  if (!portable || portable.kind !== "portable-zip") {
    throw new Error("Windows target must define one portable ZIP payload");
  }
  inspectWindowsPortableZip(path.join(root, "installers", portable.name));
}

function validateChecksums(root, checksumRelativePath) {
  const checksumPath = path.join(root, ...checksumRelativePath.split("/"));
  const expectedFiles = listRegularFiles(root)
    .filter((filePath) => path.resolve(filePath) !== path.resolve(checksumPath))
    .map((filePath) => ({
      relative: relativePosix(root, filePath),
      digest: sha256File(filePath),
    }))
    .sort((left, right) => compareReleaseNames(left.relative, right.relative));
  const expected = `${expectedFiles
    .map(({ digest, relative }) => `${digest}  ${relative}`)
    .join("\n")}\n`;
  const actual = readFileSync(checksumPath, "utf8");
  if (actual !== expected) {
    throw new Error(
      `Checksum file is incomplete, stale, or not stably sorted: ${checksumRelativePath}`,
    );
  }
}

function verifyTarget(root, args, version) {
  const channel = requireStringArgument(args, "channel");
  const platform = requireStringArgument(args, "platform");
  const arch = requireStringArgument(args, "arch");
  const targetDefinition = findSupportedTarget(platform, arch);
  const manifest = manifestName(version, channel, targetDefinition);
  const sbom = sbomName(version, channel, targetDefinition);
  const expected = [
    ...installerNames(version, channel, targetDefinition).map(
      ({ name }) => `installers/${name}`,
    ),
    `metadata/${manifest}`,
    `metadata/${sbom}`,
  ];
  const includeChecksums = args.checksums === true;
  const checksum = targetChecksumName(version, channel, targetDefinition);
  if (includeChecksums) expected.push(checksum);
  assertExactFiles(root, expected);
  validateManifest(
    path.join(root, "metadata", manifest),
    version,
    channel,
    targetDefinition,
  );
  validateSbom(path.join(root, "metadata", sbom));
  validatePayloads(root, version, channel, targetDefinition);
  if (includeChecksums) validateChecksums(root, checksum);
}

function verifyRelease(root, version) {
  const checksum = globalChecksumName(version);
  const expected = [...expectedReleaseFileNames(version), checksum];
  assertExactFiles(root, expected);
  for (const targetDefinition of supportedTargets) {
    validateManifest(
      path.join(root, manifestName(version, "release", targetDefinition)),
      version,
      "release",
      targetDefinition,
    );
    validateSbom(
      path.join(root, sbomName(version, "release", targetDefinition)),
    );
    if (targetDefinition.platform === "windows") {
      const [portable] = installerNames(version, "release", targetDefinition);
      inspectWindowsPortableZip(path.join(root, portable.name));
    }
  }
  validateChecksums(root, checksum);
}

try {
  const args = parseArguments(process.argv.slice(2));
  const scope = requireStringArgument(args, "scope");
  const root = path.resolve(requireStringArgument(args, "root"));
  const version = readProductMetadata().version;
  if (scope === "target") verifyTarget(root, args, version);
  else if (scope === "release") verifyRelease(root, version);
  else throw new Error(`Unsupported verification scope: ${scope}`);
  process.stdout.write(`Verified ${scope} artifact contract for v${version}\n`);
} catch (error) {
  process.stderr.write(
    `${error instanceof Error ? error.message : String(error)}\n`,
  );
  process.exitCode = 1;
}

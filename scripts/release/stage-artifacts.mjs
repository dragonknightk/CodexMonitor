#!/usr/bin/env node

import { spawnSync } from "node:child_process";
import {
  copyFileSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  statSync,
  writeFileSync,
} from "node:fs";
import path from "node:path";

import {
  appendGithubOutputs,
  artifactPrefix,
  buildManifest,
  createWindowsPortableZip,
  ensureEmptyDirectory,
  findSupportedTarget,
  installerNames,
  listRegularFiles,
  manifestName,
  parseArguments,
  readProductMetadata,
  requireStringArgument,
  sbomName,
} from "./release-contract.mjs";

function exactlyOne(values, description) {
  if (values.length !== 1) {
    throw new Error(
      `Expected exactly one ${description}, found ${values.length}`,
    );
  }
  return values[0];
}

function filesWithExtension(directory, extension) {
  return listRegularFiles(directory).filter((filePath) =>
    filePath.toLowerCase().endsWith(extension.toLowerCase()),
  );
}

function findAppBundles(directory) {
  const apps = [];
  const visit = (current) => {
    for (const entry of readdirSync(current, { withFileTypes: true })) {
      const absolute = path.join(current, entry.name);
      if (entry.isSymbolicLink()) {
        throw new Error(
          `Bundle output may not contain a symlink at this level: ${absolute}`,
        );
      }
      if (!entry.isDirectory()) continue;
      if (entry.name.endsWith(".app")) apps.push(absolute);
      else visit(absolute);
    }
  };
  visit(directory);
  return apps;
}

function resolveBuiltExecutable(targetDirectory, targetDefinition, appBundle) {
  const product = readProductMetadata();
  if (targetDefinition.platform === "macos") {
    const macOsDirectory = path.join(appBundle, "Contents", "MacOS");
    const files = readdirSync(macOsDirectory, { withFileTypes: true })
      .filter((entry) => entry.isFile())
      .map((entry) => path.join(macOsDirectory, entry.name));
    const preferredNames = new Set([
      product.mainBinaryName,
      product.cargoBinaryName,
      product.productName,
    ]);
    const preferred = files.filter((filePath) =>
      preferredNames.has(path.basename(filePath)),
    );
    return preferred.length === 1
      ? preferred[0]
      : exactlyOne(files, "macOS application executable");
  }

  const extension = targetDefinition.platform === "windows" ? ".exe" : "";
  const candidates = [product.mainBinaryName, product.cargoBinaryName]
    .map((name) => path.join(targetDirectory, `${name}${extension}`))
    .filter((filePath, index, all) => all.indexOf(filePath) === index)
    .filter((filePath) => {
      try {
        return statSync(filePath).isFile();
      } catch {
        return false;
      }
    });
  return exactlyOne(candidates, "built application executable");
}

try {
  const args = parseArguments(process.argv.slice(2));
  const channel = requireStringArgument(args, "channel");
  const platform = requireStringArgument(args, "platform");
  const arch = requireStringArgument(args, "arch");
  const target = requireStringArgument(args, "target");
  const targetDirectory = path.resolve(
    requireStringArgument(args, "target-dir"),
  );
  const outputDirectory = path.resolve(requireStringArgument(args, "output"));
  const targetDefinition = findSupportedTarget(platform, arch, target);
  const product = readProductMetadata();
  const bundleDirectory = path.join(targetDirectory, "bundle");
  const expectedInstallers = installerNames(
    product.version,
    channel,
    targetDefinition,
  );
  const sources = new Map();
  const stagedInstallers = new Map();
  let appBundle;

  for (const format of targetDefinition.formats) {
    if (format.kind === "portable-zip") continue;
    const formatDirectory = path.join(bundleDirectory, format.directory);
    if (format.kind === "app") {
      appBundle = exactlyOne(
        findAppBundles(formatDirectory),
        "macOS .app bundle",
      );
      sources.set(format.kind, appBundle);
      continue;
    }
    const extension =
      {
        dmg: ".dmg",
        appimage: ".AppImage",
        deb: ".deb",
        rpm: ".rpm",
      }[format.kind] ?? `.${format.kind}`;
    sources.set(
      format.kind,
      exactlyOne(
        filesWithExtension(formatDirectory, extension),
        `${format.kind} bundle`,
      ),
    );
  }

  ensureEmptyDirectory(outputDirectory);
  const installersDirectory = path.join(outputDirectory, "installers");
  const metadataDirectory = path.join(outputDirectory, "metadata");
  mkdirSync(installersDirectory);
  mkdirSync(metadataDirectory);

  const executablePath = resolveBuiltExecutable(
    targetDirectory,
    targetDefinition,
    appBundle,
  );

  for (const installer of expectedInstallers) {
    const source = sources.get(installer.kind);
    const destination = path.join(installersDirectory, installer.name);
    if (installer.kind === "portable-zip") {
      createWindowsPortableZip(executablePath, destination);
    } else if (installer.kind === "app") {
      const result = spawnSync(
        "ditto",
        ["-c", "-k", "--sequesterRsrc", "--keepParent", source, destination],
        { encoding: "utf8", stdio: "pipe" },
      );
      if (result.status !== 0) {
        throw new Error(
          `ditto failed to archive the macOS app: ${result.stderr.trim()}`,
        );
      }
    } else copyFileSync(source, destination);
    stagedInstallers.set(installer.kind, destination);
  }

  const manifestFileName = manifestName(
    product.version,
    channel,
    targetDefinition,
  );
  const manifestPath = path.join(metadataDirectory, manifestFileName);
  writeFileSync(
    manifestPath,
    `${JSON.stringify(buildManifest(product.version, channel, targetDefinition), null, 2)}\n`,
    { encoding: "utf8", flag: "wx" },
  );
  JSON.parse(readFileSync(manifestPath, "utf8"));

  const prefix = artifactPrefix(product.version, channel, platform, arch);
  const outputs = {
    version: product.version,
    artifact_prefix: prefix,
    artifact_name: prefix.toLowerCase(),
    executable_path: path.resolve(executablePath),
    installers_glob: path.join(installersDirectory, "*"),
    manifest_path: manifestPath,
    sbom_path: path.join(
      metadataDirectory,
      sbomName(product.version, channel, targetDefinition),
    ),
  };
  if (stagedInstallers.has("portable-zip"))
    outputs.portable_zip_path = path.resolve(
      stagedInstallers.get("portable-zip"),
    );
  if (appBundle) outputs.app_bundle_path = path.resolve(appBundle);
  if (stagedInstallers.has("appimage"))
    outputs.appimage_path = path.resolve(stagedInstallers.get("appimage"));
  if (stagedInstallers.has("deb"))
    outputs.deb_path = path.resolve(stagedInstallers.get("deb"));
  if (stagedInstallers.has("rpm"))
    outputs.rpm_path = path.resolve(stagedInstallers.get("rpm"));
  if (stagedInstallers.has("dmg"))
    outputs.dmg_path = path.resolve(stagedInstallers.get("dmg"));
  appendGithubOutputs(outputs);
  process.stdout.write(`${prefix}\n`);
} catch (error) {
  process.stderr.write(
    `${error instanceof Error ? error.message : String(error)}\n`,
  );
  process.exitCode = 1;
}

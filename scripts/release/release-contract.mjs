import { createHash } from "node:crypto";
import {
  appendFileSync,
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  statSync,
  writeFileSync,
} from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { deflateRawSync, inflateRawSync } from "node:zlib";

const scriptDirectory = path.dirname(fileURLToPath(import.meta.url));

export const repositoryRoot = path.resolve(scriptDirectory, "..", "..");
export const tauriConfigPath = path.join(
  repositoryRoot,
  "apps",
  "desktop",
  "src-tauri",
  "tauri.conf.json",
);

export const windowsPortableEntryName = "codex-monitor.exe";

export const supportedTargets = Object.freeze([
  Object.freeze({
    platform: "windows",
    arch: "x86_64",
    target: "x86_64-pc-windows-msvc",
    runner: "windows-2025",
    bundles: null,
    formats: Object.freeze([
      Object.freeze({
        kind: "portable-zip",
        directory: null,
        suffix: "zip",
      }),
    ]),
  }),
  Object.freeze({
    platform: "macos",
    arch: "x86_64",
    target: "x86_64-apple-darwin",
    runner: "macos-15-intel",
    bundles: "app,dmg",
    formats: Object.freeze([
      Object.freeze({ kind: "app", directory: "macos", suffix: "app.zip" }),
      Object.freeze({ kind: "dmg", directory: "dmg", suffix: "dmg.dmg" }),
    ]),
  }),
  Object.freeze({
    platform: "macos",
    arch: "aarch64",
    target: "aarch64-apple-darwin",
    runner: "macos-15",
    bundles: "app,dmg",
    formats: Object.freeze([
      Object.freeze({ kind: "app", directory: "macos", suffix: "app.zip" }),
      Object.freeze({ kind: "dmg", directory: "dmg", suffix: "dmg.dmg" }),
    ]),
  }),
  Object.freeze({
    platform: "linux",
    arch: "x86_64",
    target: "x86_64-unknown-linux-gnu",
    runner: "ubuntu-22.04",
    bundles: "appimage,deb,rpm",
    formats: Object.freeze([
      Object.freeze({
        kind: "appimage",
        directory: "appimage",
        suffix: "appimage.AppImage",
      }),
      Object.freeze({ kind: "deb", directory: "deb", suffix: "deb.deb" }),
      Object.freeze({ kind: "rpm", directory: "rpm", suffix: "rpm.rpm" }),
    ]),
  }),
]);

const allowedChannels = new Set(["release", "unsigned-development"]);

function readJson(filePath) {
  return JSON.parse(readFileSync(filePath, "utf8"));
}

function tomlSection(document, name) {
  const marker = `[${name}]`;
  const start = document.indexOf(marker);
  if (start < 0) throw new Error(`TOML section ${marker} is missing`);
  const remainder = document.slice(start + marker.length);
  const nextSection = /^\[/mu.exec(remainder);
  return nextSection ? remainder.slice(0, nextSection.index) : remainder;
}

function cargoWorkspaceVersion() {
  const cargo = readFileSync(path.join(repositoryRoot, "Cargo.toml"), "utf8");
  const workspacePackage = tomlSection(cargo, "workspace.package");
  const version = /^version\s*=\s*"([^"]+)"\s*$/mu.exec(
    workspacePackage ?? "",
  )?.[1];
  if (!version) throw new Error("Cargo workspace package version is missing");
  return version;
}

function desktopPackageName() {
  const cargo = readFileSync(
    path.join(repositoryRoot, "apps", "desktop", "src-tauri", "Cargo.toml"),
    "utf8",
  );
  const packageSection = tomlSection(cargo, "package");
  const name = /^name\s*=\s*"([^"]+)"\s*$/mu.exec(packageSection ?? "")?.[1];
  if (!name) throw new Error("Desktop Cargo package name is missing");
  return name;
}

export function readProductMetadata() {
  const tauri = readJson(tauriConfigPath);
  const packageJson = readJson(path.join(repositoryRoot, "package.json"));
  const version = tauri.version;
  if (
    typeof version !== "string" ||
    !/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/u.test(version)
  ) {
    throw new Error("Tauri version must be an explicit SemVer string");
  }
  const versions = new Map([
    ["Tauri", version],
    ["package.json", packageJson.version],
    ["Cargo workspace", cargoWorkspaceVersion()],
  ]);
  for (const [source, candidate] of versions) {
    if (candidate !== version) {
      throw new Error(
        `${source} version ${String(candidate)} does not match ${version}`,
      );
    }
  }
  if (typeof tauri.productName !== "string" || !tauri.productName) {
    throw new Error("Tauri productName is missing");
  }
  if (
    typeof tauri.identifier !== "string" ||
    !tauri.identifier ||
    tauri.identifier.toLowerCase().endsWith(".app")
  ) {
    throw new Error(
      "Tauri identifier is missing or uses a reserved .app suffix",
    );
  }
  if (typeof tauri.mainBinaryName !== "string" || !tauri.mainBinaryName) {
    throw new Error("Tauri mainBinaryName is missing");
  }
  return Object.freeze({
    productName: tauri.productName,
    version,
    identifier: tauri.identifier,
    mainBinaryName: tauri.mainBinaryName,
    cargoBinaryName: desktopPackageName(),
  });
}

export function parseArguments(argv) {
  const result = Object.create(null);
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (!argument.startsWith("--")) {
      throw new Error(`Unexpected positional argument: ${argument}`);
    }
    const key = argument.slice(2);
    if (!key || Object.hasOwn(result, key)) {
      throw new Error(`Invalid or duplicate argument: ${argument}`);
    }
    const next = argv[index + 1];
    if (next === undefined || next.startsWith("--")) result[key] = true;
    else {
      result[key] = next;
      index += 1;
    }
  }
  return result;
}

export function requireStringArgument(args, name) {
  const value = args[name];
  if (typeof value !== "string" || !value) {
    throw new Error(`--${name} is required`);
  }
  return value;
}

export function assertChannel(channel) {
  if (!allowedChannels.has(channel)) {
    throw new Error(`Unsupported release channel: ${channel}`);
  }
}

export function findSupportedTarget(platform, arch, target) {
  const matches = supportedTargets.filter(
    (candidate) =>
      candidate.platform === platform &&
      candidate.arch === arch &&
      (target === undefined || candidate.target === target),
  );
  if (matches.length !== 1) {
    throw new Error(
      `Unsupported or ambiguous release target: ${platform}/${arch}/${String(target)}`,
    );
  }
  return matches[0];
}

export function artifactPrefix(version, channel, platform, arch) {
  assertChannel(channel);
  const channelSuffix = channel === "unsigned-development" ? `-${channel}` : "";
  return `CodexMonitor-v${version}-${platform}-${arch}${channelSuffix}`;
}

export function installerNames(version, channel, targetDefinition) {
  const prefix = artifactPrefix(
    version,
    channel,
    targetDefinition.platform,
    targetDefinition.arch,
  );
  return targetDefinition.formats.map((format) => ({
    kind: format.kind,
    name:
      format.kind === "portable-zip"
        ? `${prefix}.zip`
        : `${prefix}-${format.suffix}`,
  }));
}

export function manifestName(version, channel, targetDefinition) {
  return `${artifactPrefix(
    version,
    channel,
    targetDefinition.platform,
    targetDefinition.arch,
  )}-manifest.json`;
}

export function sbomName(version, channel, targetDefinition) {
  return `${artifactPrefix(
    version,
    channel,
    targetDefinition.platform,
    targetDefinition.arch,
  )}-sbom.cdx.json`;
}

export function targetChecksumName(version, channel, targetDefinition) {
  return `${artifactPrefix(
    version,
    channel,
    targetDefinition.platform,
    targetDefinition.arch,
  )}-SHA256SUMS`;
}

export function globalChecksumName(version) {
  return `CodexMonitor-v${version}-SHA256SUMS`;
}

export function signingDeclaration(channel, platform) {
  assertChannel(channel);
  if (channel === "release" && platform === "windows") {
    return Object.freeze({ codeSigning: "authenticode", notarized: null });
  }
  if (channel === "release" && platform === "macos") {
    return Object.freeze({ codeSigning: "developer-id", notarized: true });
  }
  if (channel === "unsigned-development" && platform === "macos") {
    return Object.freeze({ codeSigning: "ad-hoc", notarized: false });
  }
  return Object.freeze({ codeSigning: "none", notarized: null });
}

export function buildManifest(version, channel, targetDefinition) {
  const product = readProductMetadata();
  return {
    schemaVersion: 1,
    product: product.productName,
    version,
    channel,
    platform: targetDefinition.platform,
    arch: targetDefinition.arch,
    target: targetDefinition.target,
    identifier: product.identifier,
    mainBinaryName: product.mainBinaryName,
    authentication: signingDeclaration(channel, targetDefinition.platform),
    artifacts: installerNames(version, channel, targetDefinition),
  };
}

export function ensureEmptyDirectory(directory) {
  if (existsSync(directory)) {
    if (!statSync(directory).isDirectory()) {
      throw new Error(`Output path is not a directory: ${directory}`);
    }
    if (readdirSync(directory).length !== 0) {
      throw new Error(`Output directory must be empty: ${directory}`);
    }
  } else mkdirSync(directory, { recursive: true });
}

export function listRegularFiles(directory) {
  if (!existsSync(directory) || !statSync(directory).isDirectory()) {
    throw new Error(`Directory does not exist: ${directory}`);
  }
  const files = [];
  const visit = (current) => {
    for (const entry of readdirSync(current, { withFileTypes: true })) {
      const absolute = path.join(current, entry.name);
      if (entry.isSymbolicLink()) {
        throw new Error(
          `Release staging may not contain symlinks: ${absolute}`,
        );
      }
      if (entry.isDirectory()) visit(absolute);
      else if (entry.isFile()) files.push(absolute);
      else throw new Error(`Unsupported release staging entry: ${absolute}`);
    }
  };
  visit(directory);
  return files;
}

export function relativePosix(root, filePath) {
  const relative = path.relative(root, filePath);
  if (!relative || relative.startsWith("..") || path.isAbsolute(relative)) {
    throw new Error(`Path is outside the expected root: ${filePath}`);
  }
  return relative.split(path.sep).join("/");
}

export function sha256File(filePath) {
  return createHash("sha256").update(readFileSync(filePath)).digest("hex");
}

function crc32(buffer) {
  let value = 0xffffffff;
  for (const byte of buffer) {
    value ^= byte;
    for (let bit = 0; bit < 8; bit += 1) {
      value = (value >>> 1) ^ (0xedb88320 & -(value & 1));
    }
  }
  return (value ^ 0xffffffff) >>> 0;
}

export function createWindowsPortableZip(binaryPath, outputPath) {
  const payload = readFileSync(binaryPath);
  if (payload.length === 0) {
    throw new Error("Windows portable executable must not be empty");
  }
  const name = Buffer.from(windowsPortableEntryName, "utf8");
  const compressed = deflateRawSync(payload, { level: 9 });
  const checksum = crc32(payload);
  const flags = 0x0800;
  const method = 8;
  const dosTime = 0;
  const dosDate = 0x0021;

  const local = Buffer.alloc(30);
  local.writeUInt32LE(0x04034b50, 0);
  local.writeUInt16LE(20, 4);
  local.writeUInt16LE(flags, 6);
  local.writeUInt16LE(method, 8);
  local.writeUInt16LE(dosTime, 10);
  local.writeUInt16LE(dosDate, 12);
  local.writeUInt32LE(checksum, 14);
  local.writeUInt32LE(compressed.length, 18);
  local.writeUInt32LE(payload.length, 22);
  local.writeUInt16LE(name.length, 26);

  const central = Buffer.alloc(46);
  central.writeUInt32LE(0x02014b50, 0);
  central.writeUInt16LE(20, 4);
  central.writeUInt16LE(20, 6);
  central.writeUInt16LE(flags, 8);
  central.writeUInt16LE(method, 10);
  central.writeUInt16LE(dosTime, 12);
  central.writeUInt16LE(dosDate, 14);
  central.writeUInt32LE(checksum, 16);
  central.writeUInt32LE(compressed.length, 20);
  central.writeUInt32LE(payload.length, 24);
  central.writeUInt16LE(name.length, 28);

  const centralOffset = local.length + name.length + compressed.length;
  const centralSize = central.length + name.length;
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0);
  end.writeUInt16LE(1, 8);
  end.writeUInt16LE(1, 10);
  end.writeUInt32LE(centralSize, 12);
  end.writeUInt32LE(centralOffset, 16);

  writeFileSync(
    outputPath,
    Buffer.concat([local, name, compressed, central, name, end]),
    { flag: "wx" },
  );
}

export function inspectWindowsPortableZip(filePath) {
  const archive = readFileSync(filePath);
  if (archive.length < 22) throw new Error("Windows portable ZIP is truncated");
  const endOffset = archive.length - 22;
  if (archive.readUInt32LE(endOffset) !== 0x06054b50) {
    throw new Error("Windows portable ZIP must not contain a trailing comment");
  }
  if (
    archive.readUInt16LE(endOffset + 8) !== 1 ||
    archive.readUInt16LE(endOffset + 10) !== 1
  ) {
    throw new Error("Windows portable ZIP must contain exactly one entry");
  }
  const centralSize = archive.readUInt32LE(endOffset + 12);
  const centralOffset = archive.readUInt32LE(endOffset + 16);
  if (centralOffset + centralSize !== endOffset) {
    throw new Error("Windows portable ZIP central directory is malformed");
  }
  if (archive.readUInt32LE(centralOffset) !== 0x02014b50) {
    throw new Error("Windows portable ZIP central directory is missing");
  }
  const nameLength = archive.readUInt16LE(centralOffset + 28);
  const extraLength = archive.readUInt16LE(centralOffset + 30);
  const commentLength = archive.readUInt16LE(centralOffset + 32);
  if (46 + nameLength + extraLength + commentLength !== centralSize) {
    throw new Error("Windows portable ZIP contains unexpected entry metadata");
  }
  const name = archive
    .subarray(centralOffset + 46, centralOffset + 46 + nameLength)
    .toString("utf8");
  if (name !== windowsPortableEntryName) {
    throw new Error(
      `Windows portable ZIP root entry must be ${windowsPortableEntryName}`,
    );
  }
  const localOffset = archive.readUInt32LE(centralOffset + 42);
  if (localOffset !== 0 || archive.readUInt32LE(localOffset) !== 0x04034b50) {
    throw new Error("Windows portable ZIP local entry is malformed");
  }
  const localNameLength = archive.readUInt16LE(localOffset + 26);
  const localExtraLength = archive.readUInt16LE(localOffset + 28);
  const localName = archive
    .subarray(localOffset + 30, localOffset + 30 + localNameLength)
    .toString("utf8");
  if (localName !== windowsPortableEntryName || localExtraLength !== 0) {
    throw new Error("Windows portable ZIP contains an unexpected root layout");
  }
  const method = archive.readUInt16LE(localOffset + 8);
  const compressedSize = archive.readUInt32LE(localOffset + 18);
  const uncompressedSize = archive.readUInt32LE(localOffset + 22);
  const dataOffset = localOffset + 30 + localNameLength;
  if (dataOffset + compressedSize !== centralOffset) {
    throw new Error("Windows portable ZIP contains unexpected extra payloads");
  }
  const compressed = archive.subarray(dataOffset, centralOffset);
  const payload = method === 8 ? inflateRawSync(compressed) : compressed;
  if (
    (method !== 0 && method !== 8) ||
    payload.length !== uncompressedSize ||
    crc32(payload) !== archive.readUInt32LE(localOffset + 14)
  ) {
    throw new Error("Windows portable ZIP executable payload is invalid");
  }
  return Object.freeze({ name, payload });
}

export function compareReleaseNames(left, right) {
  if (left === right) return 0;
  return left < right ? -1 : 1;
}

export function appendGithubOutputs(outputs) {
  const outputFile = process.env.GITHUB_OUTPUT;
  if (!outputFile) return;
  const lines = [];
  for (const [key, value] of Object.entries(outputs)) {
    const text = String(value);
    if (!/^[A-Za-z_][A-Za-z0-9_]*$/u.test(key) || /[\r\n]/u.test(text)) {
      throw new Error(`Unsafe GitHub Actions output: ${key}`);
    }
    lines.push(`${key}=${text}`);
  }
  appendFileSync(outputFile, `${lines.join("\n")}\n`, "utf8");
}

export function expectedReleaseFileNames(version) {
  const names = [];
  for (const target of supportedTargets) {
    names.push(
      ...installerNames(version, "release", target).map(({ name }) => name),
    );
    names.push(manifestName(version, "release", target));
    names.push(sbomName(version, "release", target));
  }
  return names.sort(compareReleaseNames);
}

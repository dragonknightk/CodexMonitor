import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import {
  mkdtempSync,
  mkdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import YAML from "yaml";

import {
  buildManifest,
  createWindowsPortableZip,
  expectedReleaseFileNames,
  inspectWindowsPortableZip,
  installerNames,
  manifestName,
  readProductMetadata,
  repositoryRoot,
  sbomName,
  signingDeclaration,
  supportedTargets,
} from "./release-contract.mjs";
import { writeChecksums } from "./write-checksums.mjs";

const unsignedWorkflowPath = path.join(
  repositoryRoot,
  ".github",
  "workflows",
  "package-unsigned.yml",
);
const releaseWorkflowPath = path.join(
  repositoryRoot,
  ".github",
  "workflows",
  "release.yml",
);
const installedSmokePath = path.join(
  repositoryRoot,
  "scripts",
  "installed-artifact-smoke.mjs",
);

const actionPins = new Map([
  ["actions/checkout", "df4cb1c069e1874edd31b4311f1884172cec0e10"],
  ["actions/setup-node", "249970729cb0ef3589644e2896645e5dc5ba9c38"],
  ["actions/upload-artifact", "b7c566a772e6b6bfb58ed0dc250532a479d7789f"],
  ["actions/download-artifact", "37930b1c2abaa49bbe596cd826c3c89aef350131"],
  ["actions/attest", "f6bf1532d7d6793fce74eac584813a8eee607999"],
  ["Swatinem/rust-cache", "42dc69e1aa15d09112580998cf2ef0119e2e91ae"],
  ["anchore/sbom-action", "e22c389904149dbc22b58101806040fa8d37a610"],
]);

function readWorkflow(filePath) {
  const text = readFileSync(filePath, "utf8");
  return { text, document: YAML.parse(text) };
}

function assertPinnedActions(text) {
  const uses = [...text.matchAll(/^\s*uses:\s*([^\s@]+)@([^\s]+)\s*$/gmu)];
  assert.ok(uses.length > 0);
  for (const [, action, revision] of uses) {
    assert.equal(
      revision,
      actionPins.get(action),
      `${action} must use its approved SHA`,
    );
    assert.match(revision, /^[0-9a-f]{40}$/u);
  }
}

function assertInstalledSmokeContract(text, shortcutName) {
  assert.doesNotMatch(
    text,
    /--executable "\$\{\{ steps\.stage\.outputs\.executable_path \}\}"/u,
  );
  assert.match(text, /id: installed-windows/u);
  assert.match(text, /steps\.stage\.outputs\.portable_zip_path/u);
  assert.match(text, /Expand-Archive/u);
  assert.match(text, /codex-monitor\.exe at its root/u);
  assert.match(
    text,
    /steps\.installed-windows\.outputs\.primary_executable_path/u,
  );
  assert.match(
    text,
    /steps\.installed-windows\.outputs\.secondary_executable_path/u,
  );
  assert.match(text, /--require-notifications/u);
  assert.match(text, /--cleanup-platform-registration/u);
  assert.match(text, /New-Object -ComObject WScript\.Shell/u);
  assert.match(text, /\.CreateShortcut\(\$shortcutPath\)/u);
  assert.match(text, /\$shortcut\.TargetPath/u);
  assert.match(text, new RegExp(shortcutName.replace(/[()]/gu, "\\$&"), "u"));
  assert.match(text, /id: installed-macos/u);
  assert.match(text, /steps\.installed-macos\.outputs\.executable_path/u);
  assert.match(text, /id: installed-linux/u);
  assert.match(text, /steps\.installed-linux\.outputs\.executable_path/u);
  assert.equal(
    (text.match(/node scripts\/installed-artifact-smoke\.mjs/gu) ?? []).length,
    5,
  );
  assert.match(text, /dpkg-deb --field/u);
  assert.match(text, /rpm -qp --requires/u);
  assert.match(text, /libwebkit2gtk-4\\\.1\\\.so/u);
  assert.match(text, /Remove isolated Windows portable extractions/u);
  assert.match(
    text,
    /Remove Windows platform registration after portable smoke/u,
  );
  assert.match(text, /Remove isolated macOS smoke installation/u);
  assert.match(text, /Remove isolated Linux smoke installation/u);
}

function matrixContract(document) {
  return document.jobs.package.strategy.matrix.include.map((entry) => ({
    runner: entry.runner,
    platform: entry.platform,
    arch: entry.arch,
    target: entry.target,
    bundles: entry.bundles,
  }));
}

test("installed smoke follows bootstrap into the packaged Dashboard document", () => {
  const text = readFileSync(installedSmokePath, "utf8");
  assert.match(text, /await verifyDashboardDocument\(origin, cookie\)/u);
  assert.match(text, /documentText\.includes\('<div id="root"><\/div>'\)/u);
  assert.match(text, /reference\.startsWith\("\/assets\/"\)/u);
  assert.match(text, /dashboard_document: true/u);
  assert.match(text, /dashboard_assets: true/u);
});

test("product versions and native release matrix are coherent", () => {
  const product = readProductMetadata();
  assert.match(product.version, /^\d+\.\d+\.\d+/u);
  assert.equal(product.identifier, "dev.codexmonitor.desktop");
  assert.doesNotMatch(product.identifier, /\.app$/iu);
  assert.equal(supportedTargets.length, 4);
  assert.equal(
    supportedTargets.flatMap((target) =>
      installerNames(product.version, "release", target),
    ).length,
    8,
  );
  assert.equal(expectedReleaseFileNames(product.version).length, 16);
  assert.deepEqual(
    installerNames(product.version, "release", supportedTargets[0]),
    [
      {
        kind: "portable-zip",
        name: `CodexMonitor-v${product.version}-windows-x86_64.zip`,
      },
    ],
  );
  assert.deepEqual(signingDeclaration("release", "windows"), {
    codeSigning: "authenticode",
    notarized: null,
  });
  assert.deepEqual(signingDeclaration("release", "macos"), {
    codeSigning: "developer-id",
    notarized: true,
  });
  assert.deepEqual(signingDeclaration("unsigned-development", "macos"), {
    codeSigning: "ad-hoc",
    notarized: false,
  });
});

test("unsigned workflow is isolated from release authority", () => {
  const { text, document } = readWorkflow(unsignedWorkflowPath);
  assert.deepEqual(document.permissions, { contents: "read" });
  assert.deepEqual(
    matrixContract(document),
    supportedTargets.map(({ runner, platform, arch, target, bundles }) => ({
      runner,
      platform,
      arch,
      target,
      bundles,
    })),
  );
  assert.doesNotMatch(text, /\bsecrets\./u);
  assert.doesNotMatch(text, /\bid-token\s*:/u);
  assert.doesNotMatch(text, /actions\/attest@/u);
  assert.doesNotMatch(text, /gh release/u);
  assert.match(text, /unsigned-development/u);
  assert.match(text, /APPLE_SIGNING_IDENTITY:\s*"-"/u);
  assert.match(text, /--no-sign/u);
  assert.match(text, /pnpm exec tauri build/u);
  assert.doesNotMatch(text, /\b(?:nsis|msi)\b/iu);
  assertInstalledSmokeContract(text, "CodexMonitor (Unsigned Development).lnk");
  assert.match(text, /CODEX_MONITOR_BUILD_CHANNEL:\s*unsigned-development/u);
  assertPinnedActions(text);
});

test("formal release fails closed and uses minimal job permissions", () => {
  const { text, document } = readWorkflow(releaseWorkflowPath);
  assert.deepEqual(document.permissions, { contents: "read" });
  assert.deepEqual(document.jobs.package.permissions, {
    contents: "read",
    "id-token": "write",
    attestations: "write",
  });
  assert.deepEqual(document.jobs.publish.permissions, { contents: "write" });
  assert.deepEqual(
    matrixContract(document),
    supportedTargets.map(({ runner, platform, arch, target, bundles }) => ({
      runner,
      platform,
      arch,
      target,
      bundles,
    })),
  );
  assert.match(text, /Required Windows release credential/u);
  assert.match(text, /Required macOS release credential/u);
  assert.match(text, /if: always\(\) && matrix\.platform == 'windows'/u);
  assert.match(text, /if: always\(\) && matrix\.platform == 'macos'/u);
  assert.match(text, /RUNNER_TEMP\/codexmonitor-notary-key\.p8/u);
  assert.match(text, /scripts\/release\/sign-windows\.ps1/u);
  assert.match(text, /Authenticode-sign the Windows portable executable/u);
  assert.match(text, /xcrun notarytool submit/u);
  assert.match(text, /xcrun stapler staple/u);
  assert.equal((text.match(/actions\/attest@/gu) ?? []).length, 2);
  assert.match(text, /format: cyclonedx-json/u);
  assert.match(text, /syft-version: v1\.46\.0/u);
  assert.match(text, /pnpm exec tauri build/u);
  assert.doesNotMatch(text, /\b(?:nsis|msi)\b/iu);
  assert.match(text, /scripts\/release\/collect-release\.mjs/u);
  assert.match(text, /scripts\/release\/verify-artifacts\.mjs/u);
  assertInstalledSmokeContract(text, "CodexMonitor.lnk");
  assert.match(text, /CODEX_MONITOR_BUILD_CHANNEL:\s*production/u);
  assertPinnedActions(text);
});

test("checksum writer is stable and covers every other staged file", () => {
  const directory = mkdtempSync(
    path.join(os.tmpdir(), "codexmonitor-release-checksum-"),
  );
  try {
    mkdirSync(path.join(directory, "metadata"));
    mkdirSync(path.join(directory, "installers"));
    writeFileSync(path.join(directory, "metadata", "z.json"), "{}\n");
    writeFileSync(path.join(directory, "installers", "a.bin"), "synthetic\n");
    const checksum = path.join(directory, "SHA256SUMS");
    const covered = writeChecksums(directory, checksum);
    assert.deepEqual(covered, ["installers/a.bin", "metadata/z.json"]);
    const lines = readFileSync(checksum, "utf8").trimEnd().split("\n");
    assert.equal(lines.length, 2);
    assert.match(lines[0], /^[0-9a-f]{64}  installers\/a\.bin$/u);
    assert.match(lines[1], /^[0-9a-f]{64}  metadata\/z\.json$/u);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

test("synthetic Windows bundle stages and verifies the unsigned contract", () => {
  const directory = mkdtempSync(
    path.join(os.tmpdir(), "codexmonitor-release-stage-"),
  );
  try {
    const targetDirectory = path.join(directory, "target", "release");
    const stagingDirectory = path.join(directory, "staging");
    mkdirSync(targetDirectory, { recursive: true });
    writeFileSync(
      path.join(targetDirectory, "codex-monitor-desktop.exe"),
      "binary",
    );

    execFileSync(
      process.execPath,
      [
        path.join(repositoryRoot, "scripts", "release", "stage-artifacts.mjs"),
        "--channel",
        "unsigned-development",
        "--platform",
        "windows",
        "--arch",
        "x86_64",
        "--target",
        "x86_64-pc-windows-msvc",
        "--target-dir",
        targetDirectory,
        "--output",
        stagingDirectory,
      ],
      { cwd: repositoryRoot },
    );
    const target = supportedTargets[0];
    const product = readProductMetadata();
    const [portable] = installerNames(
      product.version,
      "unsigned-development",
      target,
    );
    const archive = inspectWindowsPortableZip(
      path.join(stagingDirectory, "installers", portable.name),
    );
    assert.equal(archive.name, "codex-monitor.exe");
    assert.equal(archive.payload.toString("utf8"), "binary");
    writeFileSync(
      path.join(
        stagingDirectory,
        "metadata",
        `CodexMonitor-v${product.version}-windows-x86_64-unsigned-development-sbom.cdx.json`,
      ),
      '{"bomFormat":"CycloneDX","specVersion":"1.6"}\n',
    );
    execFileSync(
      process.execPath,
      [
        path.join(repositoryRoot, "scripts", "release", "verify-artifacts.mjs"),
        "--scope",
        "target",
        "--root",
        stagingDirectory,
        "--channel",
        "unsigned-development",
        "--platform",
        target.platform,
        "--arch",
        target.arch,
      ],
      { cwd: repositoryRoot },
    );
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

test("synthetic native inputs assemble into the exact formal release set", () => {
  const directory = mkdtempSync(
    path.join(os.tmpdir(), "codexmonitor-release-assemble-"),
  );
  try {
    const input = path.join(directory, "input");
    const output = path.join(directory, "output");
    const product = readProductMetadata();
    for (const target of supportedTargets) {
      const targetInput = path.join(input, `${target.platform}-${target.arch}`);
      mkdirSync(targetInput, { recursive: true });
      for (const { name } of installerNames(
        product.version,
        "release",
        target,
      )) {
        const destination = path.join(targetInput, name);
        if (target.platform === "windows") {
          const binary = path.join(targetInput, "synthetic-windows.exe");
          writeFileSync(binary, "synthetic windows executable\n");
          createWindowsPortableZip(binary, destination);
          rmSync(binary);
        } else {
          writeFileSync(destination, `synthetic ${name}\n`);
        }
      }
      writeFileSync(
        path.join(
          targetInput,
          manifestName(product.version, "release", target),
        ),
        `${JSON.stringify(buildManifest(product.version, "release", target), null, 2)}\n`,
      );
      writeFileSync(
        path.join(targetInput, sbomName(product.version, "release", target)),
        '{"bomFormat":"CycloneDX","specVersion":"1.6"}\n',
      );
    }

    execFileSync(
      process.execPath,
      [
        path.join(repositoryRoot, "scripts", "release", "collect-release.mjs"),
        "--input",
        input,
        "--output",
        output,
        "--version",
        product.version,
      ],
      { cwd: repositoryRoot },
    );
    writeChecksums(
      output,
      path.join(output, `CodexMonitor-v${product.version}-SHA256SUMS`),
    );
    execFileSync(
      process.execPath,
      [
        path.join(repositoryRoot, "scripts", "release", "verify-artifacts.mjs"),
        "--scope",
        "release",
        "--root",
        output,
      ],
      { cwd: repositoryRoot },
    );
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

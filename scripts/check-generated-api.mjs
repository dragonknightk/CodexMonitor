import { spawnSync } from "node:child_process";
import { readFile, rm, mkdtemp } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const scriptDirectory = dirname(fileURLToPath(import.meta.url));
const repositoryRoot = resolve(scriptDirectory, "..");

export const openApiPath = join(repositoryRoot, "api", "openapi.yaml");
export const committedSchemaPath = join(
  repositoryRoot,
  "web",
  "src",
  "api",
  "schema.d.ts",
);

function normalizeLineEndings(value) {
  return value.replaceAll("\r\n", "\n");
}

export function generatedSchemasMatch(committed, generated) {
  return normalizeLineEndings(committed) === normalizeLineEndings(generated);
}

export async function generateSchema(outputPath) {
  const result = spawnSync(
    process.execPath,
    [
      join(
        repositoryRoot,
        "web",
        "node_modules",
        "openapi-typescript",
        "bin",
        "cli.js",
      ),
      openApiPath,
      "-o",
      outputPath,
    ],
    {
      cwd: repositoryRoot,
      encoding: "utf8",
      windowsHide: true,
    },
  );

  if (result.error) {
    throw result.error;
  }
  if (result.status !== 0) {
    throw new Error(
      `OpenAPI generation failed (${String(result.status)}):\n${result.stderr || result.stdout}`,
    );
  }
}

export async function checkGeneratedApi(
  expectedSchemaPath = committedSchemaPath,
) {
  const temporaryDirectory = await mkdtemp(
    join(tmpdir(), "codex-monitor-api-"),
  );
  const generatedPath = join(temporaryDirectory, "schema.d.ts");

  try {
    await generateSchema(generatedPath);
    const [committed, generated] = await Promise.all([
      readFile(expectedSchemaPath, "utf8"),
      readFile(generatedPath, "utf8"),
    ]);
    return generatedSchemasMatch(committed, generated);
  } finally {
    await rm(temporaryDirectory, { force: true, recursive: true });
  }
}

async function main() {
  if (!(await checkGeneratedApi())) {
    console.error(
      "Generated API types are stale. Run `pnpm --dir web generate:api` and commit web/src/api/schema.d.ts.",
    );
    process.exitCode = 1;
    return;
  }

  console.log("Generated API types are current.");
}

const entryPoint = process.argv[1];
if (
  entryPoint !== undefined &&
  import.meta.url === pathToFileURL(resolve(entryPoint)).href
) {
  await main();
}

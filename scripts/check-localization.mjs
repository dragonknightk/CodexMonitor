import { createRequire } from "node:module";
import { readFile, readdir } from "node:fs/promises";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const scriptDirectory = dirname(fileURLToPath(import.meta.url));
export const repositoryRoot = resolve(scriptDirectory, "..");
export const localesRoot = join(repositoryRoot, "web", "src", "locales");
export const dashboardSourceRoot = join(repositoryRoot, "web", "src");
export const nativeHostSourceRoot = join(
  repositoryRoot,
  "apps",
  "desktop",
  "src-tauri",
  "src",
);

export const supportedLocales = ["en-US", "zh-CN"];
export const requiredNamespaces = [
  "common",
  "dashboard",
  "settings",
  "alerts",
  "errors",
  "host",
];

const require = createRequire(import.meta.url);
const ts = require(join(repositoryRoot, "web", "node_modules", "typescript"));

const visibleAttributes = new Set([
  "alt",
  "aria-description",
  "aria-label",
  "aria-placeholder",
  "aria-valuetext",
  "placeholder",
  "title",
]);
const visibleLetters = /\p{Letter}/u;

function displayPath(path) {
  return relative(repositoryRoot, path).replaceAll("\\", "/");
}

function normalizeVisibleText(value) {
  return value.replaceAll(/\s+/gu, " ").trim();
}

function flattenCatalog(value, prefix, output, violations, catalogLabel) {
  if (typeof value === "string") {
    if (prefix === "") {
      violations.push(`${catalogLabel}: catalog root must be an object`);
      return;
    }
    if (value.length === 0) {
      violations.push(`${catalogLabel}: empty translation at ${prefix}`);
    }
    output.set(prefix, value);
    return;
  }
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    violations.push(
      `${catalogLabel}: ${prefix || "<root>"} must be an object or string`,
    );
    return;
  }
  const entries = Object.entries(value);
  if (entries.length === 0) {
    violations.push(`${catalogLabel}: empty object at ${prefix || "<root>"}`);
  }
  for (const [key, child] of entries) {
    const childPrefix = prefix === "" ? key : `${prefix}.${key}`;
    flattenCatalog(child, childPrefix, output, violations, catalogLabel);
  }
}

function interpolationParameters(value, label, violations) {
  const parameters = new Set();
  for (const match of value.matchAll(/\{\{([\s\S]*?)\}\}/gu)) {
    const expression = match[1];
    const parsed =
      /^\s*-?\s*([A-Za-z_][A-Za-z0-9_]*)(?:\s*,[\s\S]*)?\s*$/u.exec(expression);
    if (parsed === null) {
      violations.push(`${label}: invalid interpolation {{${expression}}}`);
      continue;
    }
    parameters.add(parsed[1]);
  }
  return [...parameters].sort();
}

async function loadLocale(locale, root, violations) {
  const directory = join(root, locale);
  let entries;
  try {
    entries = await readdir(directory, { withFileTypes: true });
  } catch (error) {
    violations.push(
      `${displayPath(directory)}: locale directory is unavailable (${error instanceof Error ? error.message : String(error)})`,
    );
    return new Map();
  }

  const catalogs = new Map();
  const filenames = entries
    .filter((entry) => entry.isFile() && entry.name.endsWith(".json"))
    .map((entry) => entry.name)
    .sort();
  for (const filename of filenames) {
    const path = join(directory, filename);
    const label = `${locale}/${filename}`;
    try {
      const parsed = JSON.parse(await readFile(path, "utf8"));
      const flattened = new Map();
      flattenCatalog(parsed, "", flattened, violations, label);
      catalogs.set(filename.slice(0, -".json".length), flattened);
    } catch (error) {
      violations.push(
        `${label}: invalid JSON (${error instanceof Error ? error.message : String(error)})`,
      );
    }
  }
  return catalogs;
}

export async function findCatalogViolations(root = localesRoot) {
  const violations = [];
  const catalogsByLocale = new Map();
  for (const locale of supportedLocales) {
    catalogsByLocale.set(locale, await loadLocale(locale, root, violations));
  }

  const baseline = catalogsByLocale.get("en-US") ?? new Map();
  for (const namespace of requiredNamespaces) {
    if (!baseline.has(namespace)) {
      violations.push(`en-US: missing required namespace ${namespace}.json`);
    }
  }

  const baselineNamespaces = [...baseline.keys()].sort();
  for (const locale of supportedLocales.filter((value) => value !== "en-US")) {
    const catalogs = catalogsByLocale.get(locale) ?? new Map();
    const namespaces = [...catalogs.keys()].sort();
    for (const namespace of baselineNamespaces) {
      if (!catalogs.has(namespace)) {
        violations.push(`${locale}: missing namespace ${namespace}.json`);
      }
    }
    for (const namespace of namespaces) {
      if (!baseline.has(namespace)) {
        violations.push(
          `${locale}: namespace ${namespace}.json has no en-US counterpart`,
        );
      }
    }

    for (const namespace of baselineNamespaces) {
      const baselineCatalog = baseline.get(namespace);
      const translatedCatalog = catalogs.get(namespace);
      if (baselineCatalog === undefined || translatedCatalog === undefined) {
        continue;
      }
      const baselineKeys = [...baselineCatalog.keys()].sort();
      const translatedKeys = [...translatedCatalog.keys()].sort();
      for (const key of baselineKeys) {
        if (!translatedCatalog.has(key)) {
          violations.push(`${locale}/${namespace}.json: missing key ${key}`);
        }
      }
      for (const key of translatedKeys) {
        if (!baselineCatalog.has(key)) {
          violations.push(
            `${locale}/${namespace}.json: extra key ${key} has no en-US counterpart`,
          );
        }
      }
      for (const key of baselineKeys) {
        const baselineValue = baselineCatalog.get(key);
        const translatedValue = translatedCatalog.get(key);
        if (baselineValue === undefined || translatedValue === undefined) {
          continue;
        }
        const baselineParameters = interpolationParameters(
          baselineValue,
          `en-US/${namespace}.json:${key}`,
          violations,
        );
        const translatedParameters = interpolationParameters(
          translatedValue,
          `${locale}/${namespace}.json:${key}`,
          violations,
        );
        if (
          JSON.stringify(baselineParameters) !==
          JSON.stringify(translatedParameters)
        ) {
          violations.push(
            `${locale}/${namespace}.json:${key} interpolation mismatch; expected [${baselineParameters.join(", ")}], found [${translatedParameters.join(", ")}]`,
          );
        }
      }
    }
  }
  return violations;
}

function jsxStaticString(initializer) {
  if (initializer === undefined) {
    return null;
  }
  if (ts.isStringLiteral(initializer)) {
    return initializer.text;
  }
  if (
    ts.isJsxExpression(initializer) &&
    initializer.expression !== undefined &&
    (ts.isStringLiteral(initializer.expression) ||
      ts.isNoSubstitutionTemplateLiteral(initializer.expression))
  ) {
    return initializer.expression.text;
  }
  return null;
}

function openingElementIgnoresTranslation(openingElement) {
  return openingElement.attributes.properties.some(
    (attribute) =>
      ts.isJsxAttribute(attribute) &&
      attribute.name.getText() === "translate" &&
      jsxStaticString(attribute.initializer) === "no",
  );
}

function locationOf(sourceFile, node) {
  const location = sourceFile.getLineAndCharacterOfPosition(
    node.getStart(sourceFile),
  );
  return { line: location.line + 1, column: location.character + 1 };
}

export function findDashboardVisibleTextViolations(source, filename) {
  const sourceFile = ts.createSourceFile(
    filename,
    source,
    ts.ScriptTarget.Latest,
    true,
    ts.ScriptKind.TSX,
  );
  const violations = [];

  function report(node, kind, rawText) {
    const text = normalizeVisibleText(rawText);
    if (text === "" || !visibleLetters.test(text)) {
      return;
    }
    const { line, column } = locationOf(sourceFile, node);
    violations.push({ filename, line, column, kind, text });
  }

  function checkAttributes(openingElement, ignored) {
    if (ignored) {
      return;
    }
    for (const attribute of openingElement.attributes.properties) {
      if (!ts.isJsxAttribute(attribute)) {
        continue;
      }
      const name = attribute.name.getText();
      if (!visibleAttributes.has(name)) {
        continue;
      }
      const text = jsxStaticString(attribute.initializer);
      if (text !== null) {
        report(attribute, `visible JSX attribute ${name}`, text);
      }
    }
  }

  function visit(node, ignored) {
    if (ts.isJsxElement(node)) {
      const nextIgnored =
        ignored || openingElementIgnoresTranslation(node.openingElement);
      checkAttributes(node.openingElement, nextIgnored);
      for (const child of node.children) {
        visit(child, nextIgnored);
      }
      return;
    }
    if (ts.isJsxSelfClosingElement(node)) {
      const nextIgnored = ignored || openingElementIgnoresTranslation(node);
      checkAttributes(node, nextIgnored);
      return;
    }
    if (ts.isJsxFragment(node)) {
      for (const child of node.children) {
        visit(child, ignored);
      }
      return;
    }
    if (ts.isJsxText(node)) {
      if (!ignored) {
        report(node, "visible JSX text", node.getText(sourceFile));
      }
      return;
    }
    if (ts.isJsxExpression(node)) {
      if (
        !ignored &&
        node.expression !== undefined &&
        (ts.isStringLiteral(node.expression) ||
          ts.isNoSubstitutionTemplateLiteral(node.expression))
      ) {
        report(node, "visible JSX expression", node.expression.text);
      }
      if (node.expression !== undefined) {
        visit(node.expression, ignored);
      }
      return;
    }
    ts.forEachChild(node, (child) => {
      visit(child, ignored);
    });
  }

  visit(sourceFile, false);
  return violations;
}

const rustVisibleLiteralPatterns = [
  /\.(?:body|title|tooltip)\s*\(\s*"((?:\\.|[^"\\])*)"/gu,
  /(?:CheckMenuItem|IconMenuItem|MenuItem)::new\s*\(\s*[^,\n]+,\s*"((?:\\.|[^"\\])*)"/gu,
  /(?:CheckMenuItem|IconMenuItem|MenuItem)::with_id\s*\(\s*[^,\n]+,\s*[^,\n]+,\s*"((?:\\.|[^"\\])*)"/gu,
  /PredefinedMenuItem::(?:about|quit)\s*\(\s*[^,\n]+,\s*Some\s*\(\s*"((?:\\.|[^"\\])*)"/gu,
];

export function findNativeVisibleTextViolations(source, filename) {
  const violations = [];
  for (const pattern of rustVisibleLiteralPatterns) {
    for (const match of source.matchAll(pattern)) {
      const text = normalizeVisibleText(match[1]);
      if (text === "" || !visibleLetters.test(text)) {
        continue;
      }
      const index = match.index ?? 0;
      const lineStart = source.lastIndexOf("\n", index) + 1;
      const lineEnd = source.indexOf("\n", index);
      const sourceLine = source.slice(
        lineStart,
        lineEnd === -1 ? source.length : lineEnd,
      );
      if (sourceLine.includes("i18n-visible-ok:")) {
        continue;
      }
      violations.push({
        filename,
        line: source.slice(0, index).split(/\r?\n/u).length,
        column: index - lineStart + 1,
        kind: "native menu or notification literal",
        text,
      });
    }
  }
  return violations.sort(
    (left, right) => left.line - right.line || left.column - right.column,
  );
}

async function filesUnder(root, predicate) {
  const output = [];
  async function visit(directory) {
    const entries = await readdir(directory, { withFileTypes: true });
    for (const entry of entries) {
      const path = join(directory, entry.name);
      if (entry.isDirectory()) {
        await visit(path);
      } else if (entry.isFile() && predicate(path)) {
        output.push(path);
      }
    }
  }
  await visit(root);
  return output.sort();
}

export async function findProductionVisibleTextViolations({
  dashboardRoot = dashboardSourceRoot,
  nativeRoot = nativeHostSourceRoot,
} = {}) {
  const violations = [];
  const dashboardFiles = await filesUnder(
    dashboardRoot,
    (path) => path.endsWith(".tsx") && !path.endsWith(".test.tsx"),
  );
  for (const path of dashboardFiles) {
    violations.push(
      ...findDashboardVisibleTextViolations(
        await readFile(path, "utf8"),
        displayPath(path),
      ),
    );
  }

  const nativeFiles = await filesUnder(nativeRoot, (path) =>
    path.endsWith(".rs"),
  );
  for (const path of nativeFiles) {
    violations.push(
      ...findNativeVisibleTextViolations(
        await readFile(path, "utf8"),
        displayPath(path),
      ),
    );
  }
  return violations;
}

export async function findLocalizationViolations(options = {}) {
  const [catalogs, visibleText] = await Promise.all([
    findCatalogViolations(options.localesRoot),
    findProductionVisibleTextViolations({
      dashboardRoot: options.dashboardRoot,
      nativeRoot: options.nativeRoot,
    }),
  ]);
  return [
    ...catalogs.map((message) => ({ kind: "catalog", message })),
    ...visibleText.map((violation) => ({
      kind: "visible-text",
      message: `${violation.filename}:${violation.line}:${violation.column}: ${violation.kind}: ${JSON.stringify(violation.text)}`,
    })),
  ];
}

async function main() {
  const violations = await findLocalizationViolations();
  if (violations.length > 0) {
    for (const violation of violations) {
      console.error(violation.message);
    }
    process.exitCode = 1;
    return;
  }
  console.log(
    `localization validation passed (${supportedLocales.join(", ")}; ${requiredNamespaces.length} required namespaces)`,
  );
}

const entryPoint = process.argv[1];
if (
  entryPoint !== undefined &&
  import.meta.url === pathToFileURL(resolve(entryPoint)).href
) {
  await main();
}

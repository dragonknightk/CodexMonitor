#!/usr/bin/env node

import {
  appendGithubOutputs,
  parseArguments,
  readProductMetadata,
  requireStringArgument,
} from "./release-contract.mjs";

try {
  const args = parseArguments(process.argv.slice(2));
  const tag = requireStringArgument(args, "tag");
  const product = readProductMetadata();
  if (process.env.GITHUB_REF_TYPE && process.env.GITHUB_REF_TYPE !== "tag") {
    throw new Error("Formal releases must run from a Git tag");
  }
  if (tag !== `v${product.version}`) {
    throw new Error(
      `Release tag ${tag} does not match configured version v${product.version}`,
    );
  }
  appendGithubOutputs({ version: product.version });
  process.stdout.write(`${product.version}\n`);
} catch (error) {
  process.stderr.write(
    `${error instanceof Error ? error.message : String(error)}\n`,
  );
  process.exitCode = 1;
}

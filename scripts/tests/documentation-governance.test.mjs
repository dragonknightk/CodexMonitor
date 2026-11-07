import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";

import { validateDocumentation } from "../check-docs.mjs";

function write(repositoryRoot, repoPath, contents) {
  const fullPath = path.join(repositoryRoot, ...repoPath.split("/"));
  mkdirSync(path.dirname(fullPath), { recursive: true });
  writeFileSync(fullPath, contents, "utf8");
}

function document(
  id,
  repoPath,
  category,
  {
    language = "zh-CN",
    authorityLanguage = language,
    historical = "mutable",
    navigation = "docs-index",
    translationOf,
  } = {},
) {
  return {
    id,
    path: repoPath,
    category,
    language,
    authority_language: authorityLanguage,
    historical,
    navigation,
    ...(translationOf === undefined ? {} : { translation_of: translationOf }),
  };
}

function fixture() {
  const repositoryRoot = mkdtempSync(
    path.join(tmpdir(), "codex-monitor-docs-"),
  );
  write(
    repositoryRoot,
    "docs/index.md",
    "# 文档索引\n\n- [开发指南](development/guide.md)\n",
  );
  write(
    repositoryRoot,
    "docs/development/guide.md",
    "# 开发指南\n\n这里是中文说明。\n",
  );
  const config = {
    version: 2,
    generated: [],
    documents: [
      document("index", "docs/index.md", "navigation"),
      document("guide", "docs/development/guide.md", "development"),
    ],
    mappings: [],
  };
  return { repositoryRoot, config };
}

function withFixture(callback) {
  const value = fixture();
  try {
    callback(value);
  } finally {
    rmSync(value.repositoryRoot, { recursive: true, force: true });
  }
}

test("accepts a registered Chinese inventory with unified navigation", () => {
  withFixture(({ repositoryRoot, config }) => {
    const inventory = validateDocumentation(config, repositoryRoot);
    assert.equal(inventory.size, 2);
  });
});

test("rejects unregistered and unreachable Markdown", async (t) => {
  await t.test("unregistered", () => {
    withFixture(({ repositoryRoot, config }) => {
      write(repositoryRoot, "docs/orphan.md", "# 遗漏文档\n");
      assert.throws(
        () => validateDocumentation(config, repositoryRoot),
        /unregistered Markdown document/u,
      );
    });
  });

  await t.test("unreachable", () => {
    withFixture(({ repositoryRoot, config }) => {
      write(repositoryRoot, "docs/index.md", "# 文档索引\n\n暂无链接。\n");
      assert.throws(
        () => validateDocumentation(config, repositoryRoot),
        /not reachable from docs\/index\.md/u,
      );
    });
  });
});

test("rejects any AGENTS.md file even when it is registered", () => {
  withFixture(({ repositoryRoot, config }) => {
    write(repositoryRoot, "docs/AGENTS.md", "# 局部规则\n");
    config.documents.push(
      document("nested-agent", "docs/AGENTS.md", "fixed-entry"),
    );
    assert.throws(
      () => validateDocumentation(config, repositoryRoot),
      /AGENTS\.md files are not allowed/u,
    );
  });
});

test("rejects a declared Chinese authority without Chinese narrative", () => {
  withFixture(({ repositoryRoot, config }) => {
    write(
      repositoryRoot,
      "docs/development/guide.md",
      "# Development guide\n\nEnglish-only authority.\n",
    );
    assert.throws(
      () => validateDocumentation(config, repositoryRoot),
      /zh-CN document has no Chinese narrative/u,
    );
  });
});

test("validates translation links and heading structure from the registry", () => {
  withFixture(({ repositoryRoot, config }) => {
    write(
      repositoryRoot,
      "docs/index.md",
      "# 文档索引\n\n- [开发指南](development/guide.md)\n- [中文用户文档](user/example.zh-CN.md)\n- [English user guide](user/example.md)\n",
    );
    write(
      repositoryRoot,
      "docs/user/example.zh-CN.md",
      "# 用户指南\n\n[English](example.md)\n\n## 安装\n\n中文说明。\n",
    );
    write(
      repositoryRoot,
      "docs/user/example.md",
      "# User guide\n\n[中文](example.zh-CN.md)\n\n## Install\n\nEnglish translation.\n",
    );
    config.documents.push(
      document("user-zh", "docs/user/example.zh-CN.md", "user"),
      document("user-en", "docs/user/example.md", "user", {
        language: "en-US",
        authorityLanguage: "zh-CN",
        translationOf: "docs/user/example.zh-CN.md",
      }),
    );
    assert.doesNotThrow(() => validateDocumentation(config, repositoryRoot));

    write(
      repositoryRoot,
      "docs/user/example.md",
      "# User guide\n\n## Install\n\nEnglish translation.\n",
    );
    assert.throws(
      () => validateDocumentation(config, repositoryRoot),
      /missing link to authority/u,
    );
  });
});

test("rejects modifications to registered immutable history", () => {
  withFixture(({ repositoryRoot, config }) => {
    config.documents.find((entry) => entry.id === "guide").historical =
      "immutable";
    assert.throws(
      () =>
        validateDocumentation(config, repositoryRoot, {
          changedEntries: [{ status: "M", path: "docs/development/guide.md" }],
        }),
      /accepted historical document is immutable/u,
    );
  });
});

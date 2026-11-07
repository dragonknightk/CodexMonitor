import { describe, expect, it } from "vitest";

import i18n, { supportedLocales } from "./i18n";

const requiredNamespaces = [
  "common",
  "dashboard",
  "settings",
  "alerts",
  "errors",
  "host",
] as const;

describe("i18n catalogs", () => {
  it("registers every v1 namespace for both supported locales", () => {
    for (const locale of supportedLocales) {
      for (const namespace of requiredNamespaces) {
        expect(
          i18n.hasResourceBundle(locale, namespace),
          `${locale}/${namespace}`,
        ).toBe(true);
      }
    }
  });

  it("renders privacy-safe host notifications without translating identifiers", () => {
    const parameters = {
      model: "gpt-5.5",
      reasoningTokens: 516,
      patternN: 1,
    };

    expect(
      i18n.t("notification.reasoningPattern.body", {
        ...parameters,
        lng: "en-US",
        ns: "host",
      }),
    ).toBe(
      "gpt-5.5 produced 516 reasoning tokens (n = 1). Review or regenerate the response.",
    );
    expect(
      i18n.t("notification.reasoningPattern.body", {
        ...parameters,
        lng: "zh-CN",
        ns: "host",
      }),
    ).toBe(
      "gpt-5.5 产生了 516 个推理 token（n = 1）。建议检查或重新生成回答。",
    );
  });

  it("keeps the document language synchronized with the active catalog", async () => {
    await i18n.changeLanguage("zh-CN");
    expect(document.documentElement.lang).toBe("zh-CN");

    await i18n.changeLanguage("en-US");
    expect(document.documentElement.lang).toBe("en-US");
  });
});

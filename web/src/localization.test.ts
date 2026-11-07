import { describe, expect, it } from "vitest";

import {
  detectBrowserLocale,
  localeFromLanguage,
  resolveSupportedLocale,
} from "./localization";

describe("locale resolution", () => {
  it("selects the first supported browser preference", () => {
    expect(resolveSupportedLocale(["ja-JP", "zh-Hans-CN", "en-US"])).toBe(
      "zh-CN",
    );
    expect(
      detectBrowserLocale({
        languages: ["ja-JP", "en-GB"],
        language: "zh-CN",
      }),
    ).toBe("en-US");
  });

  it("does not map Traditional Chinese to the Simplified Chinese catalog", () => {
    expect(resolveSupportedLocale(["zh-TW", "zh-Hant-HK"])).toBe("en-US");
    expect(localeFromLanguage("zh-CN")).toBe("zh-CN");
    expect(localeFromLanguage(undefined)).toBe("en-US");
  });
});

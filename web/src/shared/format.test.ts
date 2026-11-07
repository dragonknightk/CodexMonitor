import { describe, expect, it } from "vitest";

import { formatDateTime, formatNumber } from "./format";

describe("locale-aware presentation formatting", () => {
  it("uses the active locale for raw numeric values", () => {
    for (const locale of ["en-US", "zh-CN"] as const) {
      expect(formatNumber(1_234_567, locale)).toBe(
        new Intl.NumberFormat(locale, {
          maximumFractionDigits: 0,
          useGrouping: true,
        }).format(1_234_567),
      );
    }
  });

  it("formats UTC timestamps per locale and rejects invalid timestamps", () => {
    const instant = "2026-07-15T12:34:56Z";
    expect(formatDateTime(instant, "en-US")).not.toBe(
      formatDateTime(instant, "zh-CN"),
    );
    expect(formatDateTime("invalid", "en-US")).toBe("—");
  });
});

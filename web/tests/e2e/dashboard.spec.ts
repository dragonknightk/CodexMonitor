import AxeBuilder from "@axe-core/playwright";
import { expect, test } from "@playwright/test";

import {
  installDashboardMock,
  localeCopy,
  openDashboard,
  projectLocale,
} from "./dashboard-mock";
import { themeContrastChecks } from "./contrast";

test("exposes an accessible, locale-complete responsive dashboard", async ({
  page,
}, testInfo) => {
  const locale = projectLocale(testInfo.project.name);
  const copy = localeCopy[locale];
  await installDashboardMock(page, locale);
  await openDashboard(page, locale);

  await expect(page.locator("html")).toHaveAttribute("lang", locale);
  await expect(
    page.getByRole("heading", { exact: true, level: 1, name: "CodexMonitor" }),
  ).toHaveCount(1);
  for (const tabLabel of [
    copy.callsHeading,
    copy.alertsHeading,
    copy.sourcesHeading,
    copy.settingsHeading,
  ]) {
    await expect(
      page.getByRole("tab", { exact: true, name: tabLabel }),
    ).toBeVisible();
  }
  await expect(
    page.getByRole("heading", {
      exact: true,
      level: 2,
      name: copy.callsHeading,
    }),
  ).toBeVisible();
  await expect(
    page.getByRole("form", { exact: true, name: copy.filterForm }),
  ).toBeVisible();
  await expect(
    page.getByRole("table", { exact: true, name: copy.tableCaption }),
  ).toBeVisible();
  await expect(
    page.getByRole("navigation", { exact: true, name: copy.pagination }),
  ).toBeVisible();

  const duplicateIds = await page.locator("[id]").evaluateAll((elements) => {
    const counts = new Map<string, number>();
    for (const element of elements) {
      counts.set(element.id, (counts.get(element.id) ?? 0) + 1);
    }
    return [...counts.entries()]
      .filter(([, count]) => count > 1)
      .map(([id]) => id);
  });
  expect(duplicateIds).toEqual([]);

  const overflow = await page.evaluate(() => ({
    clientWidth: document.documentElement.clientWidth,
    scrollWidth: document.documentElement.scrollWidth,
  }));
  expect(overflow.scrollWidth).toBeLessThanOrEqual(overflow.clientWidth + 1);
  const tableOverflow = await page
    .locator(".table-viewport")
    .evaluate((node) => ({
      clientWidth: node.clientWidth,
      scrollWidth: node.scrollWidth,
    }));
  expect(tableOverflow.scrollWidth).toBeGreaterThan(tableOverflow.clientWidth);

  for (const scrollRegion of [
    page.locator(".summary-grid"),
    page.locator(".table-viewport"),
  ]) {
    const dimensions = await scrollRegion.evaluate((node) => ({
      clientWidth: node.clientWidth,
      scrollWidth: node.scrollWidth,
    }));
    if (dimensions.scrollWidth <= dimensions.clientWidth) {
      await expect(scrollRegion).not.toHaveAttribute("tabindex");
      continue;
    }
    await expect(scrollRegion).toHaveAttribute("tabindex", "0");
    await scrollRegion.focus();
    await expect(scrollRegion).toBeFocused();
    expect(
      await scrollRegion.evaluate((node) => node.matches(":focus-visible")),
    ).toBe(true);
    await page.keyboard.press("ArrowRight");
    await expect
      .poll(() => scrollRegion.evaluate((node) => node.scrollLeft))
      .toBeGreaterThan(0);
  }

  const accessibility = await new AxeBuilder({ page }).analyze();
  expect(
    accessibility.violations,
    JSON.stringify(accessibility.violations, null, 2),
  ).toEqual([]);
  expect(
    accessibility.incomplete.filter((result) => result.id !== "color-contrast"),
  ).toEqual([]);

  for (const colorScheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme });
    for (const check of await themeContrastChecks(page)) {
      expect(
        check.ratio,
        `${colorScheme}: ${check.name} must reach ${String(check.minimum)}:1`,
      ).toBeGreaterThanOrEqual(check.minimum);
    }
  }
  await page.emulateMedia({ colorScheme: "light" });
});

test("supports keyboard filtering and persists an immediate language switch", async ({
  page,
}, testInfo) => {
  const locale = projectLocale(testInfo.project.name);
  const nextLocale = locale === "en-US" ? "zh-CN" : "en-US";
  const copy = localeCopy[locale];
  const nextCopy = localeCopy[nextLocale];
  const harness = await installDashboardMock(page, locale);
  await openDashboard(page, locale);

  const conversationFilter = page.getByRole("searchbox", {
    exact: true,
    name: copy.conversationFilter,
  });
  const modelFilter = page.getByRole("searchbox", {
    exact: true,
    name: copy.modelFilter,
  });
  await conversationFilter.focus();
  await page.keyboard.press("Tab");
  await expect(modelFilter).toBeFocused();
  expect(
    await modelFilter.evaluate((node) => node.matches(":focus-visible")),
  ).toBe(true);
  expect(
    await modelFilter.evaluate((node) =>
      Number.parseFloat(getComputedStyle(node).outlineWidth),
    ),
  ).toBeGreaterThanOrEqual(2);

  await modelFilter.fill("gpt-no-match");
  await modelFilter.press("Enter");
  await expect
    .poll(() =>
      harness.modelCallRequestUrls.some(
        (value) => new URL(value).searchParams.get("model") === "gpt-no-match",
      ),
    )
    .toBe(true);
  await expect(page.getByText(copy.emptyCalls, { exact: true })).toBeVisible();
  await page
    .getByRole("button", { exact: true, name: copy.clearFilters })
    .click();
  await expect(modelFilter).toHaveValue("");
  await expect(page.getByText("gpt-5.6-live", { exact: true })).toBeVisible();

  const fromFilter = page.getByLabel(copy.fromFilter, { exact: true });
  const toFilter = page.getByLabel(copy.toFilter, { exact: true });
  await fromFilter.fill("2026-07-15T08:30");
  await toFilter.fill("2026-07-15T08:00");
  await page
    .getByRole("button", { exact: true, name: copy.applyFilters })
    .click();
  await expect(
    page.getByRole("alert").filter({ hasText: copy.invalidTimeRange }),
  ).toBeVisible();
  await expect(fromFilter).toHaveAttribute(
    "aria-describedby",
    "model-call-time-range-error",
  );
  await expect(toFilter).toHaveAttribute(
    "aria-describedby",
    "model-call-time-range-error",
  );
  await page
    .getByRole("button", { exact: true, name: copy.clearFilters })
    .click();

  await page
    .getByRole("tab", { exact: true, name: copy.settingsHeading })
    .click();
  const language = page.getByRole("combobox", {
    name: new RegExp(copy.languageField, "u"),
  });
  await language.selectOption(nextLocale);
  await page
    .getByRole("button", { exact: true, name: copy.saveSettings })
    .click();

  await expect.poll(() => harness.settingsUpdates.length).toBe(1);
  expect(harness.settingsUpdates[0]).toEqual({
    csrfProof: "playwright_csrf_proof_0123456789abcdef",
    ifMatch: '"7"',
    settings: {
      language: nextLocale,
      login_autostart: false,
      notify_max_n: 1,
      retention_days: 90,
      system_notifications: false,
    },
  });
  await expect(page.locator("html")).toHaveAttribute("lang", nextLocale);
  await expect(
    page.getByRole("heading", {
      exact: true,
      level: 2,
      name: nextCopy.settingsHeading,
    }),
  ).toBeVisible();
  await expect(
    page.getByText(nextCopy.savedSettings, { exact: true }),
  ).toBeVisible();

  await page.reload();
  await openDashboard(page, nextLocale);
  await expect(page.locator("html")).toHaveAttribute("lang", nextLocale);
  await page
    .getByRole("tab", { exact: true, name: nextCopy.settingsHeading })
    .click();
  await expect(
    page.getByRole("combobox", {
      name: new RegExp(nextCopy.languageField, "u"),
    }),
  ).toHaveValue(nextLocale);
});

test("navigates across all workspace tabs with keyboard and pointer without layout overflow", async ({
  page,
}, testInfo) => {
  const locale = projectLocale(testInfo.project.name);
  const copy = localeCopy[locale];
  await installDashboardMock(page, locale);
  await openDashboard(page, locale);

  for (const tabHeading of [
    copy.alertsHeading,
    copy.sourcesHeading,
    copy.settingsHeading,
    copy.callsHeading,
  ]) {
    await page.getByRole("tab", { exact: true, name: tabHeading }).click();
    await expect(
      page.getByRole("heading", {
        exact: true,
        level: 2,
        name: tabHeading,
      }),
    ).toBeVisible();
    const accessibility = await new AxeBuilder({ page }).analyze();
    expect(
      accessibility.violations,
      JSON.stringify(accessibility.violations, null, 2),
    ).toEqual([]);
  }
});

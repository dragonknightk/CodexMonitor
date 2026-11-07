import type { Locale } from "./api/types";

export const supportedLocales = ["en-US", "zh-CN"] as const;

export interface BrowserLocalePreferences {
  readonly languages?: readonly string[];
  readonly language?: string;
}

function supportedLocale(languageTag: string): Locale | null {
  const normalized = languageTag.trim().replaceAll("_", "-").toLowerCase();
  if (normalized === "en" || normalized.startsWith("en-")) {
    return "en-US";
  }
  if (
    normalized === "zh" ||
    normalized === "zh-cn" ||
    normalized.startsWith("zh-cn-") ||
    normalized === "zh-sg" ||
    normalized.startsWith("zh-sg-") ||
    normalized === "zh-hans" ||
    normalized.startsWith("zh-hans-")
  ) {
    return "zh-CN";
  }
  return null;
}

export function resolveSupportedLocale(
  languageTags: readonly string[],
): Locale {
  for (const languageTag of languageTags) {
    const locale = supportedLocale(languageTag);
    if (locale !== null) {
      return locale;
    }
  }
  return "en-US";
}

export function localeFromLanguage(language: string | undefined): Locale {
  return resolveSupportedLocale(language === undefined ? [] : [language]);
}

export function detectBrowserLocale(
  preferences: BrowserLocalePreferences | undefined = typeof navigator ===
  "undefined"
    ? undefined
    : navigator,
): Locale {
  if (preferences === undefined) {
    return "en-US";
  }
  const languages = [...(preferences.languages ?? [])];
  if (
    preferences.language !== undefined &&
    !languages.includes(preferences.language)
  ) {
    languages.push(preferences.language);
  }
  return resolveSupportedLocale(languages);
}

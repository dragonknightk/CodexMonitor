import i18n from "i18next";
import { initReactI18next } from "react-i18next";

import enCommon from "./locales/en-US/common.json";
import enAlerts from "./locales/en-US/alerts.json";
import enDashboard from "./locales/en-US/dashboard.json";
import enErrors from "./locales/en-US/errors.json";
import enHost from "./locales/en-US/host.json";
import enSources from "./locales/en-US/sources.json";
import enSettings from "./locales/en-US/settings.json";
import zhCommon from "./locales/zh-CN/common.json";
import zhAlerts from "./locales/zh-CN/alerts.json";
import zhDashboard from "./locales/zh-CN/dashboard.json";
import zhErrors from "./locales/zh-CN/errors.json";
import zhHost from "./locales/zh-CN/host.json";
import zhSources from "./locales/zh-CN/sources.json";
import zhSettings from "./locales/zh-CN/settings.json";
import {
  detectBrowserLocale,
  localeFromLanguage,
  supportedLocales,
} from "./localization";

export { supportedLocales };

void i18n.use(initReactI18next).init({
  resources: {
    "en-US": {
      alerts: enAlerts,
      common: enCommon,
      dashboard: enDashboard,
      errors: enErrors,
      host: enHost,
      settings: enSettings,
      sources: enSources,
    },
    "zh-CN": {
      alerts: zhAlerts,
      common: zhCommon,
      dashboard: zhDashboard,
      errors: zhErrors,
      host: zhHost,
      settings: zhSettings,
      sources: zhSources,
    },
  },
  lng: detectBrowserLocale(),
  fallbackLng: "en-US",
  defaultNS: "common",
  supportedLngs: supportedLocales,
  initAsync: false,
  interpolation: { escapeValue: false },
});

function synchronizeDocumentLanguage(language: string | undefined) {
  if (typeof document !== "undefined") {
    document.documentElement.lang = localeFromLanguage(language);
  }
}

i18n.on("languageChanged", synchronizeDocumentLanguage);
synchronizeDocumentLanguage(i18n.resolvedLanguage);

export default i18n;

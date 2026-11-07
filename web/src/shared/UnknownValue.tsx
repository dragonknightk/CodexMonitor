import { useTranslation } from "react-i18next";

export function UnknownValue() {
  const { t } = useTranslation("common");
  return <span className="unknown-value">{t("unknown")}</span>;
}

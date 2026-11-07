import { useTranslation } from "react-i18next";

import type { Locale } from "../../api/types";
import { formatNumber } from "../../shared/format";

export interface CallsPaginationProps {
  pageNumber: number;
  locale: Locale;
  hasPrevious: boolean;
  hasNext: boolean;
  busy: boolean;
  onPrevious: () => void;
  onNext: () => void;
}

export function CallsPagination({
  pageNumber,
  locale,
  hasPrevious,
  hasNext,
  busy,
  onPrevious,
  onNext,
}: CallsPaginationProps) {
  const { t } = useTranslation("dashboard");
  return (
    <nav className="table-pagination" aria-label={t("calls.pagination.label")}>
      <span>
        {t("calls.pagination.page", {
          page: formatNumber(pageNumber, locale),
        })}
      </span>
      <div>
        <button
          type="button"
          className="secondary-button"
          disabled={busy || !hasPrevious}
          onClick={onPrevious}
        >
          {t("calls.pagination.previous")}
        </button>
        <button
          type="button"
          className="secondary-button"
          disabled={busy || !hasNext}
          onClick={onNext}
        >
          {t("calls.pagination.next")}
        </button>
      </div>
    </nav>
  );
}

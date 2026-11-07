import { useTranslation } from "react-i18next";

import type { Locale, ModelCallSummary } from "../../api/types";
import type { DisplayError } from "../../shared/errors";
import { errorTranslationKey } from "../../shared/errors";
import { formatNumber } from "../../shared/format";
import { useHorizontalOverflow } from "../../shared/useHorizontalOverflow";

export type CallsSummaryState =
  | { kind: "loading" }
  | { kind: "ready"; summary: ModelCallSummary }
  | { kind: "error"; error: DisplayError };

export interface CallsSummaryProps {
  state: CallsSummaryState;
  locale: Locale;
  onRetry: () => void;
}

interface SummaryMetricProps {
  label: string;
  value: string;
  detail?: string | undefined;
}

function SummaryMetric({ label, value, detail }: SummaryMetricProps) {
  return (
    <div className="summary-card">
      <span className="summary-label">{label}</span>
      <strong>{value}</strong>
      {detail === undefined ? null : (
        <span className="summary-detail">{detail}</span>
      )}
    </div>
  );
}

function AggregateMetric({
  label,
  aggregate,
  locale,
}: {
  label: string;
  aggregate: ModelCallSummary["input"];
  locale: Locale;
}) {
  const { t } = useTranslation("dashboard");
  const detail = aggregate.complete
    ? undefined
    : t("calls.summary.known", {
        known: formatNumber(aggregate.known_call_count, locale),
        total: formatNumber(aggregate.total_call_count, locale),
      });
  return (
    <SummaryMetric
      label={label}
      value={formatNumber(aggregate.value, locale)}
      detail={detail}
    />
  );
}

function LoadingSummary() {
  const { t } = useTranslation("dashboard");
  return (
    <>
      <span className="sr-only">{t("calls.summary.loading")}</span>
      {Array.from({ length: 7 }, (_, index) => (
        <div
          key={index}
          className="summary-card summary-card--loading"
          aria-hidden="true"
        >
          <span className="skeleton" />
          <span className="skeleton skeleton--value" />
        </div>
      ))}
    </>
  );
}

function ReadySummary({
  summary,
  locale,
}: {
  summary: ModelCallSummary;
  locale: Locale;
}) {
  const { t } = useTranslation("dashboard");
  return (
    <>
      <SummaryMetric
        label={t("calls.summary.calls")}
        value={formatNumber(summary.call_count, locale)}
      />
      <SummaryMetric
        label={t("calls.summary.alerts")}
        value={formatNumber(summary.alert_count, locale)}
      />
      <AggregateMetric
        label={t("calls.summary.input")}
        aggregate={summary.input}
        locale={locale}
      />
      <AggregateMetric
        label={t("calls.summary.cachedInput")}
        aggregate={summary.cached_input}
        locale={locale}
      />
      <AggregateMetric
        label={t("calls.summary.output")}
        aggregate={summary.output}
        locale={locale}
      />
      <AggregateMetric
        label={t("calls.summary.reasoning")}
        aggregate={summary.reasoning}
        locale={locale}
      />
      <AggregateMetric
        label={t("calls.summary.total")}
        aggregate={summary.total}
        locale={locale}
      />
    </>
  );
}

function ErrorSummary({
  error,
  onRetry,
}: {
  error: DisplayError;
  onRetry: () => void;
}) {
  const { t: tDashboard } = useTranslation("dashboard");
  const { t: tErrors } = useTranslation("errors");
  const { t: tCommon } = useTranslation("common");
  return (
    <div className="summary-error" role="alert">
      <strong>{tDashboard("calls.summary.errorTitle")}</strong>
      <span>{tErrors(errorTranslationKey(error.code))}</span>
      <button type="button" className="secondary-button" onClick={onRetry}>
        {tCommon("retry")}
      </button>
    </div>
  );
}

export function CallsSummary({ state, locale, onRetry }: CallsSummaryProps) {
  const { t } = useTranslation("dashboard");
  const { elementRef, overflows } = useHorizontalOverflow<HTMLElement>();
  return (
    <section
      ref={elementRef}
      className="summary-grid"
      aria-label={t("calls.summary.label")}
      aria-busy={state.kind === "loading"}
      tabIndex={overflows ? 0 : undefined}
    >
      {state.kind === "loading" ? <LoadingSummary /> : null}
      {state.kind === "ready" ? (
        <ReadySummary summary={state.summary} locale={locale} />
      ) : null}
      {state.kind === "error" ? (
        <ErrorSummary error={state.error} onRetry={onRetry} />
      ) : null}
    </section>
  );
}

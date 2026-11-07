import { useTranslation } from "react-i18next";

import type { DisplayError } from "../../shared/errors";
import { errorTranslationKey } from "../../shared/errors";
import {
  formatDateTime,
  formatNumber,
  shortenIdentifier,
} from "../../shared/format";
import { UnknownValue } from "../../shared/UnknownValue";
import { useHorizontalOverflow } from "../../shared/useHorizontalOverflow";
import type { Locale, ModelCall } from "../../api/types";

const columnCount = 12;
const loadingRowCount = 5;

export type ModelCallTableState =
  | { kind: "loading" }
  | { kind: "ready"; items: ModelCall[] }
  | { kind: "error"; error: DisplayError };

export interface ModelCallTableProps {
  state: ModelCallTableState;
  locale: Locale;
  onRetry: () => void;
}

function OptionalValue({ value }: { value: string | null }) {
  return value === null ? <UnknownValue /> : <span>{value}</span>;
}

function TokenValue({
  value,
  locale,
}: {
  value: number | null;
  locale: Locale;
}) {
  return value === null ? (
    <UnknownValue />
  ) : (
    <span>{formatNumber(value, locale)}</span>
  );
}

function ConversationValue({ call }: { call: ModelCall }) {
  const conversation = call.conversation;
  if (conversation === null) {
    return <UnknownValue />;
  }

  const fallbackId = shortenIdentifier(conversation.id);
  const primary =
    conversation.resolved_display_name ??
    conversation.thread_id ??
    conversation.source_id ??
    fallbackId;
  const secondary =
    conversation.resolved_display_name === null
      ? null
      : (conversation.thread_id ?? conversation.source_id ?? fallbackId);

  return (
    <span className="cell-stack">
      <span className="cell-primary">{primary}</span>
      {secondary === null ? null : (
        <span className="cell-secondary">{shortenIdentifier(secondary)}</span>
      )}
    </span>
  );
}

function SourceValue({
  sourceIds,
  locale,
}: {
  sourceIds: string[];
  locale: Locale;
}) {
  const first = sourceIds[0];
  if (first === undefined) {
    return <UnknownValue />;
  }
  return (
    <span className="source-value" title={sourceIds.join(", ")}>
      {first}
      {sourceIds.length > 1
        ? ` +${formatNumber(sourceIds.length - 1, locale)}`
        : null}
    </span>
  );
}

function AlertValue({ call, locale }: { call: ModelCall; locale: Locale }) {
  const { t } = useTranslation("dashboard");
  if (call.alert_state === "none") {
    return (
      <span className="alert-state alert-state--none">
        {t("calls.alert.none")}
      </span>
    );
  }
  return (
    <span className="alert-state alert-state--matched">
      {t("calls.alert.matched", {
        count: formatNumber(call.alert_count, locale),
      })}
    </span>
  );
}

function ModelCallRow({ call, locale }: { call: ModelCall; locale: Locale }) {
  return (
    <tr>
      <td className="cell-time">
        <time dateTime={call.event_at}>
          {formatDateTime(call.event_at, locale)}
        </time>
      </td>
      <td>
        <ConversationValue call={call} />
      </td>
      <td>
        <OptionalValue value={call.model} />
      </td>
      <td>
        <OptionalValue value={call.reasoning_effort} />
      </td>
      <td className="cell-number">
        <TokenValue value={call.tokens.input} locale={locale} />
      </td>
      <td className="cell-number">
        <TokenValue value={call.tokens.cached_input} locale={locale} />
      </td>
      <td className="cell-number">
        <TokenValue value={call.tokens.output} locale={locale} />
      </td>
      <td className="cell-number">
        <TokenValue value={call.tokens.reasoning} locale={locale} />
      </td>
      <td className="cell-number cell-number--total">
        <TokenValue value={call.tokens.total} locale={locale} />
      </td>
      <td>
        <SourceValue sourceIds={call.source_ids} locale={locale} />
      </td>
      <td>
        <OptionalValue value={call.client} />
      </td>
      <td>
        <AlertValue call={call} locale={locale} />
      </td>
    </tr>
  );
}

function LoadingRows() {
  const { t } = useTranslation("dashboard");
  return (
    <>
      <tr className="sr-only">
        <td colSpan={columnCount}>{t("calls.state.loading")}</td>
      </tr>
      {Array.from({ length: loadingRowCount }, (_, rowIndex) => (
        <tr key={rowIndex} className="loading-row" aria-hidden="true">
          {Array.from({ length: columnCount }, (_, columnIndex) => (
            <td key={columnIndex}>
              <span className="skeleton" />
            </td>
          ))}
        </tr>
      ))}
    </>
  );
}

function EmptyRow() {
  const { t } = useTranslation("dashboard");
  return (
    <tr>
      <td colSpan={columnCount}>
        <div className="table-state" role="status">
          <strong>{t("calls.state.emptyTitle")}</strong>
          <span>{t("calls.state.emptyBody")}</span>
        </div>
      </td>
    </tr>
  );
}

function ErrorRow({
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
    <tr>
      <td colSpan={columnCount}>
        <div className="table-state table-state--error" role="alert">
          <strong>{tDashboard("calls.state.errorTitle")}</strong>
          <span>{tErrors(errorTranslationKey(error.code))}</span>
          <button type="button" className="secondary-button" onClick={onRetry}>
            {tCommon("retry")}
          </button>
        </div>
      </td>
    </tr>
  );
}

export function ModelCallTable({
  state,
  locale,
  onRetry,
}: ModelCallTableProps) {
  const { t } = useTranslation("dashboard");
  const { elementRef, overflows } = useHorizontalOverflow<HTMLDivElement>();
  return (
    <div
      ref={elementRef}
      className="table-viewport"
      role="region"
      aria-label={t("calls.table.scrollRegion")}
      tabIndex={overflows ? 0 : undefined}
    >
      <table className="model-call-table" aria-busy={state.kind === "loading"}>
        <caption className="sr-only">{t("calls.table.caption")}</caption>
        <colgroup>
          <col className="column-time" />
          <col className="column-conversation" />
          <col className="column-model" />
          <col className="column-effort" />
          <col className="column-token" span={5} />
          <col className="column-source" />
          <col className="column-client" />
          <col className="column-alert" />
        </colgroup>
        <thead>
          <tr>
            <th scope="col">{t("calls.table.columns.time")}</th>
            <th scope="col">{t("calls.table.columns.conversation")}</th>
            <th scope="col">{t("calls.table.columns.model")}</th>
            <th scope="col">{t("calls.table.columns.effort")}</th>
            <th scope="col" className="cell-number">
              {t("calls.table.columns.input")}
            </th>
            <th scope="col" className="cell-number">
              {t("calls.table.columns.cachedInput")}
            </th>
            <th scope="col" className="cell-number">
              {t("calls.table.columns.output")}
            </th>
            <th scope="col" className="cell-number">
              {t("calls.table.columns.reasoning")}
            </th>
            <th scope="col" className="cell-number">
              {t("calls.table.columns.total")}
            </th>
            <th scope="col">{t("calls.table.columns.source")}</th>
            <th scope="col">{t("calls.table.columns.client")}</th>
            <th scope="col">{t("calls.table.columns.alert")}</th>
          </tr>
        </thead>
        <tbody>
          {state.kind === "loading" ? <LoadingRows /> : null}
          {state.kind === "ready" && state.items.length === 0 ? (
            <EmptyRow />
          ) : null}
          {state.kind === "ready"
            ? state.items.map((call) => (
                <ModelCallRow key={call.id} call={call} locale={locale} />
              ))
            : null}
          {state.kind === "error" ? (
            <ErrorRow error={state.error} onRetry={onRetry} />
          ) : null}
        </tbody>
      </table>
    </div>
  );
}

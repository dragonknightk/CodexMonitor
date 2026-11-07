import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import type { Alert, AlertEvent, Locale, PageMetadata } from "../../api/types";
import type { ResourceSynchronization } from "../events";
import type { DisplayError } from "../../shared/errors";
import { errorTranslationKey, toDisplayError } from "../../shared/errors";
import {
  formatDateTime,
  formatNumber,
  shortenIdentifier,
} from "../../shared/format";
import { alertsApi } from "./api";
import type { AlertsApi } from "./api";

const initialQuery = { limit: 10 } as const;

function compareAlerts(left: Alert, right: Alert): number {
  return (
    (right.created_at < left.created_at
      ? -1
      : right.created_at > left.created_at
        ? 1
        : 0) || (right.id < left.id ? -1 : right.id > left.id ? 1 : 0)
  );
}

function mergeAlerts(current: Alert[], incoming: Alert[]): Alert[] {
  const byId = new Map(current.map((alert) => [alert.id, alert]));
  for (const alert of incoming) {
    byId.set(alert.id, alert);
  }
  return [...byId.values()].sort(compareAlerts);
}

type AlertsState =
  | { kind: "loading" }
  | { kind: "ready"; items: Alert[] }
  | { kind: "error"; error: DisplayError };

type LoadMoreState =
  | { kind: "idle" }
  | { kind: "loading" }
  | { kind: "error"; error: DisplayError };

export interface AlertsViewProps {
  api?: AlertsApi | undefined;
  locale: Locale;
  synchronization?: ResourceSynchronization<AlertEvent> | undefined;
}

function ActionState({
  action,
  locale,
}: {
  action: Alert["action"];
  locale: Locale;
}) {
  const { t } = useTranslation("alerts");
  if (action === null) {
    return (
      <span className="action-summary">
        <span className="diagnostic-badge diagnostic-badge--none">
          {t("action.notRequested")}
        </span>
      </span>
    );
  }

  const eventTime =
    action.completed_at ?? action.started_at ?? action.not_before;
  return (
    <span className="action-summary">
      <span className={`diagnostic-badge diagnostic-badge--${action.state}`}>
        {t(`action.states.${action.state}`)}
      </span>
      {action.suppression_reason === null ? null : (
        <span>{t(`action.suppression.${action.suppression_reason}`)}</span>
      )}
      {action.error_code === null ? null : (
        <span>
          <code>{action.error_code}</code>{" "}
          {t(`action.errors.${action.error_code}`)}
        </span>
      )}
      {action.suppression_reason === null && action.error_code === null ? (
        <time dateTime={eventTime}>{formatDateTime(eventTime, locale)}</time>
      ) : null}
    </span>
  );
}

function AlertCard({ alert, locale }: { alert: Alert; locale: Locale }) {
  const { t } = useTranslation("alerts");
  return (
    <article className="alert-card">
      <header className="alert-card__header">
        <div>
          <p>{t("suspectedPattern")}</p>
          <h3>
            {t("matchTitle", {
              tokens: formatNumber(
                alert.evidence.reasoning_output_tokens,
                locale,
              ),
              pattern: formatNumber(alert.evidence.pattern_n, locale),
            })}
          </h3>
        </div>
        <time dateTime={alert.evidence.event_at}>
          {formatDateTime(alert.evidence.event_at, locale)}
        </time>
      </header>

      <dl className="alert-evidence">
        <div>
          <dt>{t("fields.modelCall")}</dt>
          <dd>
            <code title={alert.model_call_id}>
              {shortenIdentifier(alert.model_call_id)}
            </code>
          </dd>
        </div>
        <div>
          <dt>{t("fields.ruleVersion")}</dt>
          <dd>
            <code>{`${alert.rule_id}@${String(alert.rule_version)}`}</code>
          </dd>
        </div>
        <div>
          <dt>{t("fields.source")}</dt>
          <dd>
            <code>{`${alert.evidence.source_id}@${String(alert.evidence.source_contract_version)}`}</code>
            <span className="evidence-quality">
              {t(`quality.${alert.evidence.quality}`)}
            </span>
          </dd>
        </div>
      </dl>

      <div className="alert-action">
        <span>{t("fields.action")}</span>
        <ActionState action={alert.action} locale={locale} />
      </div>
    </article>
  );
}

export function AlertsView({
  api = alertsApi,
  locale,
  synchronization,
}: AlertsViewProps) {
  const { t } = useTranslation("alerts");
  const { t: tErrors } = useTranslation("errors");
  const { t: tCommon } = useTranslation("common");
  const [attempt, setAttempt] = useState(0);
  const [state, setState] = useState<AlertsState>({ kind: "loading" });
  const [page, setPage] = useState<PageMetadata | null>(null);
  const [requestedCursor, setRequestedCursor] = useState<string | null>(null);
  const [loadMoreState, setLoadMoreState] = useState<LoadMoreState>({
    kind: "idle",
  });
  const processedDeliveryRef = useRef(0);
  const startedSyncEpochRef = useRef<number | null>(null);
  const syncEpoch = synchronization?.epoch;
  const synchronized = synchronization !== undefined;
  const onSnapshotComplete = synchronization?.onSnapshotComplete;
  const deliveredEvents = synchronization?.events;

  useEffect(() => {
    if (synchronized && syncEpoch === null) {
      return;
    }
    if (
      typeof syncEpoch === "number" &&
      startedSyncEpochRef.current !== syncEpoch
    ) {
      startedSyncEpochRef.current = syncEpoch;
      setRequestedCursor(null);
      setPage(null);
      setLoadMoreState({ kind: "idle" });
      setState({ kind: "loading" });
    }
    let active = true;
    void api.list(initialQuery).then(
      (result) => {
        if (active) {
          setPage(result.page);
          setState({ kind: "ready", items: result.items });
          if (typeof syncEpoch === "number") {
            onSnapshotComplete?.("alerts", syncEpoch);
          }
        }
      },
      (error: unknown) => {
        if (active) {
          setState({ kind: "error", error: toDisplayError(error) });
        }
      },
    );
    return () => {
      active = false;
    };
  }, [api, attempt, onSnapshotComplete, syncEpoch, synchronized]);

  useEffect(() => {
    const pending = (deliveredEvents ?? []).filter(
      (delivery) => delivery.deliveryId > processedDeliveryRef.current,
    );
    if (pending.length === 0) {
      return;
    }
    processedDeliveryRef.current = pending.at(-1)?.deliveryId ?? 0;
    setState((current) => {
      if (current.kind !== "ready") {
        return current;
      }
      const items = mergeAlerts(
        current.items,
        pending.map((delivery) => delivery.event.alert),
      );
      return {
        kind: "ready",
        items: items.slice(
          0,
          Math.max(current.items.length, initialQuery.limit),
        ),
      };
    });
  }, [deliveredEvents]);

  useEffect(() => {
    if (requestedCursor === null) {
      return;
    }
    let active = true;
    void api.list({ ...initialQuery, cursor: requestedCursor }).then(
      (result) => {
        if (active) {
          setState((current) =>
            current.kind === "ready"
              ? {
                  kind: "ready",
                  items: mergeAlerts(current.items, result.items),
                }
              : current,
          );
          setPage(result.page);
          setRequestedCursor(null);
          setLoadMoreState({ kind: "idle" });
        }
      },
      (error: unknown) => {
        if (active) {
          setRequestedCursor(null);
          setLoadMoreState({
            kind: "error",
            error: toDisplayError(error),
          });
        }
      },
    );
    return () => {
      active = false;
    };
  }, [api, requestedCursor]);

  const retryLoad = () => {
    setRequestedCursor(null);
    setPage(null);
    setLoadMoreState({ kind: "idle" });
    setState({ kind: "loading" });
    setAttempt((value) => value + 1);
  };

  const loadMore = () => {
    if (page?.has_more !== true || loadMoreState.kind === "loading") {
      return;
    }
    setLoadMoreState({ kind: "loading" });
    setRequestedCursor(page.next_cursor);
  };

  return (
    <section className="feature-panel" aria-labelledby="alerts-heading">
      <header className="feature-heading">
        <h2 id="alerts-heading">{t("heading")}</h2>
      </header>

      {state.kind === "loading" ? (
        <div className="feature-state" role="status">
          {t("state.loading")}
        </div>
      ) : null}
      {state.kind === "error" ? (
        <div className="feature-state feature-state--error" role="alert">
          <strong>{t("state.errorTitle")}</strong>
          <span>{tErrors(errorTranslationKey(state.error.code))}</span>
          <button
            type="button"
            className="secondary-button"
            onClick={retryLoad}
          >
            {tCommon("retry")}
          </button>
        </div>
      ) : null}
      {state.kind === "ready" && state.items.length === 0 ? (
        <div className="feature-state" role="status">
          {t("state.empty")}
        </div>
      ) : null}
      {state.kind === "ready" && state.items.length > 0 ? (
        <div className="alert-list">
          {state.items.map((alert) => (
            <AlertCard key={alert.id} alert={alert} locale={locale} />
          ))}
        </div>
      ) : null}

      {state.kind === "ready" && page?.has_more === true ? (
        <div className="load-more">
          <button
            type="button"
            className="secondary-button"
            disabled={loadMoreState.kind === "loading"}
            onClick={loadMore}
          >
            {loadMoreState.kind === "loading"
              ? t("pagination.loading")
              : t("pagination.loadMore")}
          </button>
        </div>
      ) : null}
      {loadMoreState.kind === "error" ? (
        <div className="inline-error load-more-error" role="alert">
          <span>{tErrors(errorTranslationKey(loadMoreState.error.code))}</span>
          <button type="button" className="secondary-button" onClick={loadMore}>
            {tCommon("retry")}
          </button>
        </div>
      ) : null}
    </section>
  );
}

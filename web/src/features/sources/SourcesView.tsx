import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import type { Locale, SourceHealth, SourceHealthEvent } from "../../api/types";
import type { ResourceSynchronization } from "../events";
import type { DisplayError } from "../../shared/errors";
import { errorTranslationKey, toDisplayError } from "../../shared/errors";
import { formatDateTime } from "../../shared/format";
import { UnknownValue } from "../../shared/UnknownValue";
import { sourcesApi } from "./api";
import type { SourcesApi } from "./api";
import {
  canRetryReceiver,
  mergeSourceHealth,
  replaceSourceHealth,
} from "./model";

type SourcesState =
  | { kind: "loading" }
  | { kind: "ready"; items: SourceHealth[] }
  | { kind: "error"; error: DisplayError };

interface SourceMutationState {
  sourceId: string | null;
  error: DisplayError | null;
  status: "idle" | "pending" | "success" | "error";
}

export interface SourcesViewProps {
  api?: SourcesApi | undefined;
  locale: Locale;
  synchronization?: ResourceSynchronization<SourceHealthEvent> | undefined;
}

function OptionalDateTime({
  value,
  locale,
}: {
  value: string | null;
  locale: Locale;
}) {
  return value === null ? (
    <UnknownValue />
  ) : (
    <time dateTime={value}>{formatDateTime(value, locale)}</time>
  );
}

function StatusBadge({ status }: { status: SourceHealth["status"] }) {
  const { t } = useTranslation("sources");
  return (
    <span className={`diagnostic-badge diagnostic-badge--${status}`}>
      {t(`status.${status}`)}
    </span>
  );
}

function ReceiverDetails({ source }: { source: SourceHealth }) {
  const { t } = useTranslation("sources");
  if (source.receiver === null) {
    return <UnknownValue />;
  }
  return (
    <span className="receiver-details">
      <code>{`${source.receiver.host}:${String(source.receiver.port)}`}</code>
      <span
        className={`diagnostic-badge diagnostic-badge--${source.receiver.bind_state}`}
      >
        {t(`bindState.${source.receiver.bind_state}`)}
      </span>
    </span>
  );
}

function SafeErrorDetails({ source }: { source: SourceHealth }) {
  const { t } = useTranslation("sources");
  if (source.last_error_code === null) {
    return (
      <div className="safe-diagnostic">
        <p>{t("remediation.bindConflict")}</p>
      </div>
    );
  }
  return (
    <div className="safe-diagnostic">
      <code>{source.last_error_code}</code>
      <span>{t(`safeError.codes.${source.last_error_code}.message`)}</span>
      <p>{t(`safeError.codes.${source.last_error_code}.remediation`)}</p>
    </div>
  );
}

function SourceCard({
  source,
  locale,
  mutation,
  onRetry,
}: {
  source: SourceHealth;
  locale: Locale;
  mutation: SourceMutationState;
  onRetry: (sourceId: SourceHealth["source_id"]) => void;
}) {
  const { t } = useTranslation("sources");
  const { t: tErrors } = useTranslation("errors");
  const canRetry = canRetryReceiver(source);
  const hasDiagnosticError =
    source.last_error_code !== null ||
    source.receiver?.bind_state === "conflict";
  const retrying =
    mutation.sourceId === source.source_id && mutation.status === "pending";
  return (
    <article className="diagnostic-card">
      <header className="diagnostic-card__header">
        <div>
          <h3>{source.source_id}</h3>
          <span>
            {t("contractVersion", { version: source.source_contract_version })}
          </span>
        </div>
        <StatusBadge status={source.status} />
      </header>

      <dl className="diagnostic-list">
        <div>
          <dt>{t("fields.receiver")}</dt>
          <dd>
            <ReceiverDetails source={source} />
          </dd>
        </div>
        <div>
          <dt>{t("fields.lastAccepted")}</dt>
          <dd>
            <OptionalDateTime value={source.last_accepted_at} locale={locale} />
          </dd>
        </div>
        {hasDiagnosticError ? (
          <>
            <div>
              <dt>{t("fields.lastSafeError")}</dt>
              <dd>
                <SafeErrorDetails source={source} />
              </dd>
            </div>
            <div>
              <dt>{t("fields.errorOccurred")}</dt>
              <dd>
                <OptionalDateTime
                  value={source.last_error_occurred_at}
                  locale={locale}
                />
              </dd>
            </div>
          </>
        ) : null}
        <div>
          <dt>{t("fields.updated")}</dt>
          <dd>
            <time dateTime={source.updated_at}>
              {formatDateTime(source.updated_at, locale)}
            </time>
          </dd>
        </div>
      </dl>

      <div className="supported-fields">
        <span>{t("fields.supportedFields")}</span>
        <div>
          {source.supported_fields.length === 0 ? (
            <span className="unknown-value">{t("supportedFieldsEmpty")}</span>
          ) : (
            source.supported_fields.map((field) => (
              <code key={field}>{field}</code>
            ))
          )}
        </div>
      </div>

      {canRetry ? (
        <button
          type="button"
          className="secondary-button"
          disabled={retrying}
          onClick={() => {
            onRetry(source.source_id);
          }}
        >
          {retrying ? t("retry.retrying") : t("retry.action")}
        </button>
      ) : null}

      {mutation.status !== "error" ||
      mutation.error === null ||
      mutation.sourceId !== source.source_id ? null : (
        <p className="inline-error" role="alert">
          {tErrors(errorTranslationKey(mutation.error.code))}
        </p>
      )}
      {mutation.status === "success" &&
      mutation.sourceId === source.source_id ? (
        <p className="inline-success" role="status">
          {t("retry.accepted")}
        </p>
      ) : null}
    </article>
  );
}

export function SourcesView({
  api = sourcesApi,
  locale,
  synchronization,
}: SourcesViewProps) {
  const { t } = useTranslation("sources");
  const { t: tErrors } = useTranslation("errors");
  const { t: tCommon } = useTranslation("common");
  const [attempt, setAttempt] = useState(0);
  const [state, setState] = useState<SourcesState>({ kind: "loading" });
  const [mutation, setMutation] = useState<SourceMutationState>({
    sourceId: null,
    error: null,
    status: "idle",
  });
  const processedDeliveryRef = useRef(0);
  const syncEpoch = synchronization?.epoch;
  const synchronized = synchronization !== undefined;
  const onSnapshotComplete = synchronization?.onSnapshotComplete;
  const deliveredEvents = synchronization?.events;

  useEffect(() => {
    if (synchronized && syncEpoch === null) {
      return;
    }
    let active = true;
    void api.list().then(
      (list) => {
        if (active) {
          setState({ kind: "ready", items: list.items });
          if (typeof syncEpoch === "number") {
            onSnapshotComplete?.("sources", syncEpoch);
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
      return {
        kind: "ready",
        items: mergeSourceHealth(
          current.items,
          pending.map((delivery) => delivery.event.source),
        ),
      };
    });
  }, [deliveredEvents]);

  const retryLoad = () => {
    setState({ kind: "loading" });
    setAttempt((value) => value + 1);
  };

  const retrySource = (sourceId: SourceHealth["source_id"]) => {
    setMutation({ sourceId, error: null, status: "pending" });
    void api.retry(sourceId).then(
      (updated) => {
        setState((current) =>
          current.kind === "ready"
            ? {
                kind: "ready",
                items: replaceSourceHealth(current.items, updated),
              }
            : current,
        );
        setMutation({ sourceId, error: null, status: "success" });
      },
      (error: unknown) => {
        setMutation({
          sourceId,
          error: toDisplayError(error),
          status: "error",
        });
      },
    );
  };

  return (
    <section className="feature-panel" aria-labelledby="sources-heading">
      <header className="feature-heading">
        <h2 id="sources-heading">{t("heading")}</h2>
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
        <div className="diagnostic-cards">
          {state.items.map((source) => (
            <SourceCard
              key={`${source.source_id}:${String(source.source_contract_version)}`}
              source={source}
              locale={locale}
              mutation={mutation}
              onRetry={retrySource}
            />
          ))}
        </div>
      ) : null}
    </section>
  );
}

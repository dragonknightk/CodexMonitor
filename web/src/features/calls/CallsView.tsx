import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import type { PageMetadata } from "../../api/types";
import type { ModelCallEvent } from "../../api/types";
import type { ResourceSynchronization } from "../events";
import { toDisplayError } from "../../shared/errors";
import { localeFromLanguage } from "../../localization";
import { callsApi } from "./api";
import type { CallsApi } from "./api";
import { CallsPagination } from "./CallsPagination";
import { CallsSummary } from "./CallsSummary";
import type { CallsSummaryState } from "./CallsSummary";
import { ModelCallFilters } from "./ModelCallFilters";
import { ModelCallTable } from "./ModelCallTable";
import type { ModelCallTableState } from "./ModelCallTable";
import {
  buildModelCallQuery,
  defaultModelCallFilterDraft,
  defaultModelCallQuery,
  toSummaryQuery,
  withPageCursor,
} from "./query";
import type { ModelCallFilterDraft } from "./query";
import { mergeModelCallEvent } from "./live";

export interface CallsViewProps {
  api?: CallsApi | undefined;
  synchronization?: ResourceSynchronization<ModelCallEvent> | undefined;
}

export function CallsView({ api = callsApi, synchronization }: CallsViewProps) {
  const { t, i18n } = useTranslation("dashboard");
  const [draft, setDraft] = useState<ModelCallFilterDraft>(() => ({
    ...defaultModelCallFilterDraft,
  }));
  const [validationError, setValidationError] = useState<string | null>(null);
  const [appliedQuery, setAppliedQuery] = useState(() => ({
    ...defaultModelCallQuery,
  }));
  const [pageCursors, setPageCursors] = useState<(string | null)[]>([null]);
  const [pageIndex, setPageIndex] = useState(0);
  const [pageMetadata, setPageMetadata] = useState<PageMetadata | null>(null);
  const [listAttempt, setListAttempt] = useState(0);
  const [summaryAttempt, setSummaryAttempt] = useState(0);
  const [tableState, setTableState] = useState<ModelCallTableState>({
    kind: "loading",
  });
  const [summaryState, setSummaryState] = useState<CallsSummaryState>({
    kind: "loading",
  });
  const listSnapshotEpochRef = useRef<number | null>(null);
  const summarySnapshotEpochRef = useRef<number | null>(null);
  const startedSyncEpochRef = useRef<number | null>(null);
  const processedDeliveryRef = useRef(0);

  const syncEpoch = synchronization?.epoch;
  const synchronized = synchronization !== undefined;
  const onSnapshotComplete = synchronization?.onSnapshotComplete;
  const deliveredEvents = synchronization?.events;

  const currentCursor = pageCursors[pageIndex] ?? null;
  const pageQuery = useMemo(
    () => withPageCursor(appliedQuery, currentCursor),
    [appliedQuery, currentCursor],
  );
  const summaryQuery = useMemo(
    () => toSummaryQuery(appliedQuery),
    [appliedQuery],
  );

  const reportCompletedSnapshot = useCallback(
    (kind: "list" | "summary", completedEpoch: number) => {
      if (kind === "list") {
        listSnapshotEpochRef.current = completedEpoch;
      } else {
        summarySnapshotEpochRef.current = completedEpoch;
      }
      if (
        listSnapshotEpochRef.current === completedEpoch &&
        summarySnapshotEpochRef.current === completedEpoch
      ) {
        onSnapshotComplete?.("calls", completedEpoch);
      }
    },
    [onSnapshotComplete],
  );

  useEffect(() => {
    if (synchronized && syncEpoch === null) {
      return;
    }
    const startsNewSync =
      typeof syncEpoch === "number" &&
      startedSyncEpochRef.current !== syncEpoch;
    const requestQuery = startsNewSync
      ? withPageCursor(appliedQuery, null)
      : pageQuery;
    if (startsNewSync) {
      startedSyncEpochRef.current = syncEpoch;
      listSnapshotEpochRef.current = null;
      summarySnapshotEpochRef.current = null;
      setPageCursors([null]);
      setPageIndex(0);
      setPageMetadata(null);
      setTableState({ kind: "loading" });
      setSummaryState({ kind: "loading" });
    }
    if (typeof syncEpoch === "number") {
      listSnapshotEpochRef.current = null;
    }
    let active = true;
    void api.list(requestQuery).then(
      (page) => {
        if (active) {
          setPageMetadata(page.page);
          setTableState({ kind: "ready", items: page.items });
          if (typeof syncEpoch === "number") {
            reportCompletedSnapshot("list", syncEpoch);
          }
        }
      },
      (error: unknown) => {
        if (active) {
          setTableState({ kind: "error", error: toDisplayError(error) });
        }
      },
    );
    return () => {
      active = false;
    };
  }, [
    api,
    appliedQuery,
    listAttempt,
    pageQuery,
    reportCompletedSnapshot,
    syncEpoch,
    synchronized,
  ]);

  useEffect(() => {
    if (synchronized && syncEpoch === null) {
      return;
    }
    if (typeof syncEpoch === "number") {
      summarySnapshotEpochRef.current = null;
    }
    let active = true;
    void api.summarize(summaryQuery).then(
      (summary) => {
        if (active) {
          setSummaryState({ kind: "ready", summary });
          if (typeof syncEpoch === "number") {
            reportCompletedSnapshot("summary", syncEpoch);
          }
        }
      },
      (error: unknown) => {
        if (active) {
          setSummaryState({ kind: "error", error: toDisplayError(error) });
        }
      },
    );
    return () => {
      active = false;
    };
  }, [
    api,
    reportCompletedSnapshot,
    summaryAttempt,
    summaryQuery,
    syncEpoch,
    synchronized,
  ]);

  useEffect(() => {
    const pending = (deliveredEvents ?? []).filter(
      (delivery) => delivery.deliveryId > processedDeliveryRef.current,
    );
    if (pending.length === 0) {
      return;
    }
    processedDeliveryRef.current = pending.at(-1)?.deliveryId ?? 0;
    setTableState((current) =>
      current.kind === "ready"
        ? {
            kind: "ready",
            items: pending.reduce(
              (items, delivery) =>
                mergeModelCallEvent(
                  items,
                  delivery.event.model_call,
                  appliedQuery,
                  pageIndex === 0,
                ),
              current.items,
            ),
          }
        : current,
    );
    setListAttempt((value) => value + 1);
    setSummaryAttempt((value) => value + 1);
  }, [appliedQuery, deliveredEvents, pageIndex]);

  const resetPage = () => {
    setPageCursors([null]);
    setPageIndex(0);
    setPageMetadata(null);
    setTableState({ kind: "loading" });
  };

  const applyFilters = () => {
    const result = buildModelCallQuery(draft);
    if (!result.ok) {
      setValidationError(t("calls.filters.errors.invalidTimeRange"));
      return;
    }
    setValidationError(null);
    setAppliedQuery(result.query);
    resetPage();
    setSummaryState({ kind: "loading" });
  };

  const resetFilters = () => {
    setDraft({ ...defaultModelCallFilterDraft });
    setValidationError(null);
    setAppliedQuery({ ...defaultModelCallQuery });
    resetPage();
    setSummaryState({ kind: "loading" });
  };

  const retryList = () => {
    setTableState({ kind: "loading" });
    setListAttempt((value) => value + 1);
  };

  const retrySummary = () => {
    setSummaryState({ kind: "loading" });
    setSummaryAttempt((value) => value + 1);
  };

  const showNextPage = () => {
    if (tableState.kind === "loading" || pageMetadata?.has_more !== true) {
      return;
    }
    setPageCursors((cursors) => [
      ...cursors.slice(0, pageIndex + 1),
      pageMetadata.next_cursor,
    ]);
    setPageIndex(pageIndex + 1);
    setTableState({ kind: "loading" });
  };

  const showPreviousPage = () => {
    if (tableState.kind === "loading" || pageIndex === 0) {
      return;
    }
    setPageIndex(pageIndex - 1);
    setTableState({ kind: "loading" });
  };

  const locale = localeFromLanguage(i18n.resolvedLanguage);
  const busy = tableState.kind === "loading";

  return (
    <section className="calls-panel" aria-labelledby="calls-heading">
      <div className="section-heading">
        <h2 id="calls-heading">{t("calls.heading")}</h2>
      </div>
      <CallsSummary
        state={summaryState}
        locale={locale}
        onRetry={retrySummary}
      />
      <ModelCallFilters
        draft={draft}
        locale={locale}
        validationError={validationError}
        busy={busy}
        onChange={setDraft}
        onApply={applyFilters}
        onReset={resetFilters}
      />
      <ModelCallTable state={tableState} locale={locale} onRetry={retryList} />
      <CallsPagination
        pageNumber={pageIndex + 1}
        locale={locale}
        hasPrevious={pageIndex > 0}
        hasNext={pageMetadata?.has_more === true}
        busy={busy}
        onPrevious={showPreviousPage}
        onNext={showNextPage}
      />
    </section>
  );
}

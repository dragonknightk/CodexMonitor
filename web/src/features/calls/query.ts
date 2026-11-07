import type { ModelCallQuery, SummaryQuery } from "../../api/types";

export const recentCountOptions = [25, 50, 100] as const;
export type RecentCount = (typeof recentCountOptions)[number];

export const modelCallSortOptions = [
  "event_at_desc",
  "event_at_asc",
  "input_tokens_desc",
  "output_tokens_desc",
  "reasoning_tokens_desc",
  "total_tokens_desc",
] as const satisfies readonly NonNullable<ModelCallQuery["sort"]>[];
export type ModelCallSort = (typeof modelCallSortOptions)[number];

export const alertFilterOptions = ["any", "matched", "none"] as const;
export type AlertFilter = (typeof alertFilterOptions)[number];

export interface ModelCallFilterDraft {
  recentCount: RecentCount;
  conversation: string;
  model: string;
  sourceId: string;
  from: string;
  to: string;
  alert: AlertFilter;
  sort: ModelCallSort;
}

export const defaultModelCallFilterDraft: ModelCallFilterDraft = {
  recentCount: 25,
  conversation: "",
  model: "",
  sourceId: "",
  from: "",
  to: "",
  alert: "any",
  sort: "event_at_desc",
};

export const defaultModelCallQuery: ModelCallQuery = {
  limit: defaultModelCallFilterDraft.recentCount,
  alert: defaultModelCallFilterDraft.alert,
  sort: defaultModelCallFilterDraft.sort,
};

export type ModelCallQueryBuildResult =
  | { ok: true; query: ModelCallQuery }
  | { ok: false; reason: "invalid_time_range" };

function localDateTimeToUtc(value: string): string | null {
  const instant = new Date(value);
  if (Number.isNaN(instant.getTime())) {
    return null;
  }
  return instant.toISOString();
}

export function buildModelCallQuery(
  draft: ModelCallFilterDraft,
): ModelCallQueryBuildResult {
  const conversation = draft.conversation.trim();
  const model = draft.model.trim();
  const sourceId = draft.sourceId.trim();
  const from = draft.from === "" ? undefined : localDateTimeToUtc(draft.from);
  const to = draft.to === "" ? undefined : localDateTimeToUtc(draft.to);

  if (
    from === null ||
    to === null ||
    (from !== undefined && to !== undefined && from >= to)
  ) {
    return { ok: false, reason: "invalid_time_range" };
  }

  return {
    ok: true,
    query: {
      limit: draft.recentCount,
      alert: draft.alert,
      sort: draft.sort,
      ...(conversation === "" ? {} : { conversation }),
      ...(model === "" ? {} : { model }),
      ...(sourceId === "" ? {} : { source_id: sourceId }),
      ...(from === undefined ? {} : { from }),
      ...(to === undefined ? {} : { to }),
    },
  };
}

export function withPageCursor(
  query: ModelCallQuery,
  cursor: string | null,
): ModelCallQuery {
  return cursor === null ? query : { ...query, cursor };
}

export function toSummaryQuery(query: ModelCallQuery): SummaryQuery {
  return {
    ...(query.conversation === undefined
      ? {}
      : { conversation: query.conversation }),
    ...(query.model === undefined ? {} : { model: query.model }),
    ...(query.source_id === undefined ? {} : { source_id: query.source_id }),
    ...(query.client === undefined ? {} : { client: query.client }),
    ...(query.from === undefined ? {} : { from: query.from }),
    ...(query.to === undefined ? {} : { to: query.to }),
    ...(query.alert === undefined ? {} : { alert: query.alert }),
    ...(query.temporal_class === undefined
      ? {}
      : { temporal_class: query.temporal_class }),
  };
}

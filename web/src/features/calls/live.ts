import type { ModelCall, ModelCallQuery } from "../../api/types";

function compareText(left: string, right: string): number {
  return left < right ? -1 : left > right ? 1 : 0;
}

function compareDescendingNumber(
  left: number | null,
  right: number | null,
): number {
  if (left === null) {
    return right === null ? 0 : 1;
  }
  if (right === null) {
    return -1;
  }
  return right - left;
}

function compareNewest(left: ModelCall, right: ModelCall): number {
  return (
    compareText(right.event_at, left.event_at) || compareText(right.id, left.id)
  );
}

function compareOldest(left: ModelCall, right: ModelCall): number {
  return (
    compareText(left.event_at, right.event_at) || compareText(left.id, right.id)
  );
}

export function compareModelCalls(
  left: ModelCall,
  right: ModelCall,
  sort: NonNullable<ModelCallQuery["sort"]> = "event_at_desc",
): number {
  switch (sort) {
    case "event_at_desc":
      return compareNewest(left, right);
    case "event_at_asc":
      return compareOldest(left, right);
    case "input_tokens_desc":
      return (
        compareDescendingNumber(left.tokens.input, right.tokens.input) ||
        compareNewest(left, right)
      );
    case "output_tokens_desc":
      return (
        compareDescendingNumber(left.tokens.output, right.tokens.output) ||
        compareNewest(left, right)
      );
    case "reasoning_tokens_desc":
      return (
        compareDescendingNumber(
          left.tokens.reasoning,
          right.tokens.reasoning,
        ) || compareNewest(left, right)
      );
    case "total_tokens_desc":
      return (
        compareDescendingNumber(left.tokens.total, right.tokens.total) ||
        compareNewest(left, right)
      );
  }
}

function matchesConversation(call: ModelCall, selector: string): boolean {
  if (call.conversation === null) {
    return false;
  }
  return [
    call.conversation.id,
    call.conversation.source_id,
    call.conversation.thread_id,
    call.conversation.resolved_display_name,
  ].some((value) => value === selector);
}

export function modelCallMatchesQuery(
  call: ModelCall,
  query: ModelCallQuery,
): boolean {
  return (
    (query.conversation === undefined ||
      matchesConversation(call, query.conversation)) &&
    (query.model === undefined || call.model === query.model) &&
    (query.source_id === undefined ||
      call.source_ids.includes(query.source_id)) &&
    (query.client === undefined || call.client === query.client) &&
    (query.from === undefined || call.event_at >= query.from) &&
    (query.to === undefined || call.event_at < query.to) &&
    (query.alert === undefined ||
      query.alert === "any" ||
      call.alert_state === query.alert) &&
    (query.temporal_class === undefined ||
      call.temporal_class === query.temporal_class)
  );
}

export function mergeModelCallEvent(
  current: ModelCall[],
  incoming: ModelCall,
  query: ModelCallQuery,
  firstPage: boolean,
): ModelCall[] {
  const existing = current.some((call) => call.id === incoming.id);
  const withoutIncoming = current.filter((call) => call.id !== incoming.id);
  if (modelCallMatchesQuery(incoming, query) && (firstPage || existing)) {
    withoutIncoming.push(incoming);
  }
  withoutIncoming.sort((left, right) =>
    compareModelCalls(left, right, query.sort),
  );
  return withoutIncoming.slice(0, query.limit ?? 25);
}

import type { SourceHealth } from "../../api/types";

function sourceIdentity(source: SourceHealth): string {
  return `${source.source_id}\u0000${String(source.source_contract_version)}`;
}

export function canRetryReceiver(source: SourceHealth): boolean {
  return (
    source.receiver?.retry_supported === true &&
    (source.receiver.bind_state === "conflict" ||
      source.receiver.bind_state === "retrying")
  );
}

export function mergeSourceHealth(
  current: readonly SourceHealth[],
  incoming: readonly SourceHealth[],
): SourceHealth[] {
  const byIdentity = new Map(
    current.map((source) => [sourceIdentity(source), source]),
  );
  for (const source of incoming) {
    byIdentity.set(sourceIdentity(source), source);
  }
  return [...byIdentity.values()].sort(
    (left, right) =>
      (left.source_id < right.source_id
        ? -1
        : left.source_id > right.source_id
          ? 1
          : 0) || left.source_contract_version - right.source_contract_version,
  );
}

export function replaceSourceHealth(
  current: readonly SourceHealth[],
  updated: SourceHealth,
): SourceHealth[] {
  return current.map((source) =>
    sourceIdentity(source) === sourceIdentity(updated) ? updated : source,
  );
}

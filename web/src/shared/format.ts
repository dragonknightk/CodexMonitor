import type { Locale } from "../api/types";

export function formatNumber(value: number, locale: Locale): string {
  return new Intl.NumberFormat(locale, {
    maximumFractionDigits: 0,
    useGrouping: true,
  }).format(value);
}

export function formatDateTime(value: string, locale: Locale): string {
  const instant = new Date(value);
  if (Number.isNaN(instant.getTime())) {
    return "—";
  }

  return new Intl.DateTimeFormat(locale, {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hourCycle: "h23",
    timeZone: "UTC",
    timeZoneName: "short",
  }).format(instant);
}

export function shortenIdentifier(value: string): string {
  if (value.length <= 16) {
    return value;
  }
  return `${value.slice(0, 8)}…${value.slice(-5)}`;
}

export function deriveTotal(
  input: number | null,
  output: number | null,
): number | null {
  if (input === null || output === null) {
    return null;
  }
  const total = input + output;
  return Number.isSafeInteger(total) && total <= 1_000_000_000_000_000
    ? total
    : null;
}

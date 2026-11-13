// Display helpers. Times stay in UTC everywhere on the page, the same as
// the filters, so what the operator types and what they read agree.

/** `2026-11-12T18:00:01.123Z` → `2026-11-12 18:00:01`. */
export function formatTime(utc: string): string {
  const match = /^(\d{4}-\d{2}-\d{2})T(\d{2}:\d{2}:\d{2})/.exec(utc);
  return match ? `${match[1]} ${match[2]}` : utc;
}

/** A `datetime-local` value, read as UTC, to RFC 3339; `""` → undefined. */
export function utcInputToRfc3339(value: string): string | undefined {
  if (!value) return undefined;
  const withSeconds = value.length === 16 ? `${value}:00` : value;
  return `${withSeconds}Z`;
}

/** Exact decimal text, never parsed into a float: `0.0000375` → `$0.0000375`. */
export function formatCost(usd: string | null | undefined): string {
  return usd == null ? "—" : `$${usd}`;
}

export function formatTokens(input: number | null, output: number | null): string {
  if (input == null && output == null) return "—";
  return `${input ?? "—"} / ${output ?? "—"}`;
}

export function formatLatency(ms: number): string {
  return ms < 1000 ? `${ms} ms` : `${(ms / 1000).toFixed(2)} s`;
}

export const STATUS_LABELS = {
  succeeded: "Succeeded",
  failed: "Failed",
  timed_out: "Timed out",
} as const;

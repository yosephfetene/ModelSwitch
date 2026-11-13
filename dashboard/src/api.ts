// The admin API (docs/API.md, ADR-036). The session lives in an HttpOnly
// cookie the browser sends by itself; page scripts never see it, and the
// admin token is sent once, at login, and never stored.

export type Status = "succeeded" | "failed" | "timed_out";

export interface RequestSummary {
  id: string;
  client_request_id: string | null;
  /** UTC, such as 2026-11-12T18:00:01.123Z. */
  created_at: string;
  key: { id: string; name: string; prefix: string };
  requested_model: string;
  provider: string;
  model: string;
  status: Status;
  error: { kind: string; code: string | null; provider_status: number | null } | null;
  latency_ms: number;
  usage: {
    input_tokens: number | null;
    output_tokens: number | null;
    total_tokens: number | null;
    source: "provider_reported" | "unavailable";
  };
  /** Exact decimal USD as text: a JSON number would round it. */
  cost: { usd: string; pricing_version: string | null } | null;
  attempt_count: number;
}

export interface HistoryPage {
  requests: RequestSummary[];
  next_before: string | null;
}

export interface Filters {
  /** RFC 3339, UTC. */
  from?: string;
  to?: string;
  provider?: string;
  status?: Status;
}

/** A refused or failed API call, with the gateway's error code. */
export class ApiError extends Error {
  readonly status: number;
  readonly code: string;
  readonly retryAfterSecs: number | null;

  constructor(status: number, code: string, message: string, retryAfterSecs: number | null) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.code = code;
    this.retryAfterSecs = retryAfterSecs;
  }
}

async function call(path: string, init: RequestInit = {}): Promise<Response> {
  let response: Response;
  try {
    response = await fetch(path, { credentials: "same-origin", ...init });
  } catch (error) {
    if (error instanceof DOMException && error.name === "AbortError") throw error;
    throw new ApiError(0, "network", "The gateway could not be reached.", null);
  }
  if (response.ok) return response;

  let code = "unknown";
  let message = `The gateway answered ${response.status}.`;
  try {
    const body = (await response.json()) as { error?: { code?: string; message?: string } };
    if (body.error?.code) code = body.error.code;
    if (body.error?.message) message = body.error.message;
  } catch {
    // Not a gateway error body, such as a proxy's page: keep the defaults.
  }
  const retryAfter = Number.parseInt(response.headers.get("retry-after") ?? "", 10);
  throw new ApiError(
    response.status,
    code,
    message,
    Number.isFinite(retryAfter) ? retryAfter : null,
  );
}

/** Seconds left in the current session; throws 401 when logged out. */
export async function session(): Promise<{ expires_in_secs: number }> {
  return (await call("/admin/api/session")).json();
}

export async function login(token: string): Promise<void> {
  await call("/admin/api/login", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ token }),
  });
}

export async function logout(): Promise<void> {
  await call("/admin/api/logout", { method: "POST" });
}

export function requestsQuery(filters: Filters, before: string | null): string {
  const params = new URLSearchParams();
  for (const name of ["from", "to", "provider", "status"] as const) {
    const value = filters[name];
    if (value) params.set(name, value);
  }
  if (before) params.set("before", before);
  const query = params.toString();
  return query ? `/admin/api/requests?${query}` : "/admin/api/requests";
}

export async function requests(
  filters: Filters,
  before: string | null,
  signal?: AbortSignal,
): Promise<HistoryPage> {
  return (await call(requestsQuery(filters, before), { signal })).json();
}

// A fake admin API behind `fetch`, so tests run without a gateway.

import { vi } from "vitest";

import type { HistoryPage, RequestSummary } from "../api";

export interface Call {
  method: string;
  path: string;
  params: URLSearchParams;
  headers: Headers;
  body: string | null;
}

type Handler = (call: Call) => Response | Promise<Response>;

export function json(status: number, body: unknown, headers: Record<string, string> = {}): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json", ...headers },
  });
}

export function apiError(status: number, code: string, message = code, headers = {}): Response {
  return json(status, { error: { code, message }, request_id: "req_test" }, headers);
}

export const noContent = () => new Response(null, { status: 204 });

/**
 * Routes `"METHOD /path"` to handlers and records every call. Unrouted
 * calls answer 404 like the gateway does.
 */
export function fakeGateway(routes: Record<string, Handler>) {
  const calls: Call[] = [];
  const fetch = vi.fn(async (input: RequestInfo | URL, init: RequestInit = {}) => {
    const url = new URL(String(input), "http://127.0.0.1:9091");
    const call: Call = {
      method: init.method ?? "GET",
      path: url.pathname,
      params: url.searchParams,
      headers: new Headers(init.headers),
      body: typeof init.body === "string" ? init.body : null,
    };
    calls.push(call);
    const handler = routes[`${call.method} ${call.path}`];
    return handler ? handler(call) : apiError(404, "not_found");
  });
  vi.stubGlobal("fetch", fetch);
  return { calls, fetch, requestCalls: () => calls.filter((c) => c.path === "/admin/api/requests") };
}

export function summary(overrides: Partial<RequestSummary> = {}): RequestSummary {
  return {
    id: "req_0190f5a2-0000-7000-8000-000000000001",
    client_request_id: null,
    created_at: "2026-11-12T18:00:01.123Z",
    key: { id: "6f9619ff-8b86-4011-b42d-00c04fc964ff", name: "dev", prefix: "ms_dev_7Hq2" },
    requested_model: "fast",
    provider: "groq",
    model: "llama-3.1-8b-instant",
    status: "succeeded",
    error: null,
    latency_ms: 412,
    usage: { input_tokens: 12, output_tokens: 34, total_tokens: 46, source: "provider_reported" },
    cost: { usd: "0.0000375", pricing_version: "0190f5a2-0000-7000-8000-0000000000aa" },
    attempt_count: 1,
    ...overrides,
  };
}

export function page(requests: RequestSummary[], next_before: string | null = null): HistoryPage {
  return { requests, next_before };
}

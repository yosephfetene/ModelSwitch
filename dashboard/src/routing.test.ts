import { describe, expect, it } from "vitest";

import type { Attempt } from "./api";
import { describeFailure, routingReason } from "./routing";
import { attempt, detail } from "./test/gateway";

const failed = (overrides: Partial<Attempt>): Attempt =>
  attempt({
    status: "failed",
    error: { kind: "status", code: null, provider_status: 500, class: "transient" },
    ...overrides,
  });
const status = (provider_status: number | null, code: string | null = null, kind = "status") =>
  failed({ error: { kind, code, provider_status, class: null } });

describe("routingReason", () => {
  it("names the alias and who answered", () => {
    expect(routingReason(detail({ requested_model: "fast" }))).toBe(
      "Alias fast → answered by groq / llama-3.1-8b-instant",
    );
  });

  it("says when the model was pinned", () => {
    expect(routingReason(detail({ requested_model: "groq/llama-3.1-8b-instant" }))).toBe(
      "Pinned to groq/llama-3.1-8b-instant → answered by groq / llama-3.1-8b-instant",
    );
  });

  it("tells a retry and a fallback apart", () => {
    const reason = routingReason(
      detail({
        requested_model: "fast",
        attempts: [
          failed({ number: 1, provider: "groq", retry_decision: "retry" }),
          { ...failed({ number: 2, provider: "groq", retry_decision: "fallback" }), status: "timed_out", error: null },
          attempt({ number: 3, provider: "gemini", model: "gemini-2.5-flash" }),
        ],
      }),
    );
    expect(reason).toBe(
      "Alias fast → groq had a server error (500), retried → groq timed out, fell back → answered by gemini / gemini-2.5-flash",
    );
  });

  it("explains a rejected Gemini key that fell back", () => {
    const reason = routingReason(
      detail({
        requested_model: "reverse",
        attempts: [
          failed({
            provider: "gemini",
            error: { kind: "status", code: "API_KEY_INVALID", provider_status: 400, class: "target_problem" },
            retry_decision: "fallback",
          }),
          attempt({ number: 2, provider: "groq", model: "openai/gpt-oss-20b" }),
        ],
      }),
    );
    expect(reason).toBe("Alias reverse → gemini rejected the key, fell back → answered by groq / openai/gpt-oss-20b");
  });

  it("ends with the failure when nothing answered", () => {
    const reason = routingReason(
      detail({
        status: "failed",
        requested_model: "allfail",
        attempts: [
          failed({ provider: "groq", retry_decision: "fallback", error: { kind: "status", code: null, provider_status: 429, class: "transient" } }),
          failed({ number: 2, provider: "gemini", retry_decision: "exhausted", error: { kind: "status", code: "API_KEY_INVALID", provider_status: 400, class: "target_problem" } }),
        ],
      }),
    );
    expect(reason).toBe(
      "Alias allfail → groq was rate limited, fell back → gemini rejected the key, nothing left to try → the request failed",
    );
  });

  it("says when the request itself was refused", () => {
    const reason = routingReason(
      detail({
        status: "failed",
        attempts: [
          failed({
            retry_decision: "not_retried",
            error: { kind: "status", code: "context_length_exceeded", provider_status: 400, class: "request_problem" },
          }),
        ],
      }),
    );
    expect(reason).toBe(
      "Alias fast → groq refused the request (400 context_length_exceeded), stopped: the request itself was refused → the request failed",
    );
  });

  it("says when no provider was called", () => {
    const reason = routingReason(
      detail({
        status: "failed",
        requested_model: "failover-demo",
        attempts: [],
        error: { kind: "status", code: "model_not_found", provider_status: null },
      }),
    );
    expect(reason).toBe("Alias failover-demo → no provider was called (model_not_found)");
  });

  it("says when the whole request timed out", () => {
    const reason = routingReason(
      detail({
        status: "timed_out",
        attempts: [{ ...attempt(), status: "timed_out", retry_decision: "exhausted" }],
      }),
    );
    expect(reason).toBe("Alias fast → groq timed out, nothing left to try → the request timed out");
  });
});

describe("describeFailure", () => {
  it.each([
    [status(401), "rejected the key"],
    [status(403), "rejected the key"],
    [status(400, "API_KEY_INVALID"), "rejected the key"],
    [status(429), "was rate limited"],
    [status(404), "does not have the model"],
    [status(400, "model_not_found"), "does not have the model"],
    [status(503), "had a server error (503)"],
    [status(400), "refused the request (400)"],
    [status(null, null, "request"), "could not be reached"],
    [status(null, null, "invalid_response"), "sent an unreadable answer"],
    [status(null, "SAFETY", "blocked"), "blocked the request (SAFETY)"],
    [status(null, null, "blocked"), "blocked the request"],
    [status(null, "weird"), "failed (weird)"],
    [failed({ error: null }), "failed"],
  ])("%#: %s", (input, expected) => {
    expect(describeFailure(input)).toBe(expected);
  });
});

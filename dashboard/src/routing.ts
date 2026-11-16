// The routing reason (ADR-036): built from what is stored, the requested
// model and the attempt chain, never from a stored explanation. Each step
// is short enough to read at a glance:
//
//   Alias `fast` → gemini rejected the key, fell back → answered by groq / llama
//
// Stage 5 may store reasons that attempts cannot explain; until then this
// is the whole story.

import type { Attempt, RequestDetail } from "./api";

/** What went wrong in one attempt, in a few words. */
export function describeFailure(attempt: Attempt): string {
  if (attempt.status === "timed_out") return "timed out";
  const error = attempt.error;
  if (!error) return "failed";
  const status = error.provider_status;
  const code = error.code;
  switch (error.kind) {
    case "request":
      return "could not be reached";
    case "invalid_response":
      return "sent an unreadable answer";
    case "blocked":
      return code ? `blocked the request (${code})` : "blocked the request";
  }
  if (code === "API_KEY_INVALID" || status === 401 || status === 403) return "rejected the key";
  if (status === 429) return "was rate limited";
  if (status === 404 || code === "model_not_found") return "does not have the model";
  if (status !== null && status >= 500) return `had a server error (${status})`;
  if (status !== null) return code ? `refused the request (${status} ${code})` : `refused the request (${status})`;
  return code ? `failed (${code})` : "failed";
}

function after(attempt: Attempt): string | null {
  switch (attempt.retry_decision) {
    case "retry":
      return "retried";
    case "fallback":
      return "fell back";
    case "exhausted":
      return "nothing left to try";
    case "not_retried":
      return attempt.error?.class === "request_problem" ? "stopped: the request itself was refused" : null;
    default:
      return null;
  }
}

/** The routing reason as steps, to be read left to right. */
export function routingSteps(detail: RequestDetail): string[] {
  const requested = detail.requested_model;
  const steps = [requested.includes("/") ? `Pinned to ${requested}` : `Alias ${requested}`];

  if (detail.attempts.length === 0) {
    const why = detail.error?.code ?? detail.error?.kind;
    steps.push(why ? `no provider was called (${why})` : "no provider was called");
    return steps;
  }

  for (const attempt of detail.attempts) {
    if (attempt.status === "succeeded") {
      steps.push(`answered by ${attempt.provider} / ${attempt.model}`);
      continue;
    }
    const next = after(attempt);
    const failure = `${attempt.provider} ${describeFailure(attempt)}`;
    steps.push(next ? `${failure}, ${next}` : failure);
  }

  if (detail.status !== "succeeded" && detail.attempts.every((a) => a.status !== "succeeded")) {
    steps.push(detail.status === "timed_out" ? "the request timed out" : "the request failed");
  }
  return steps;
}

export function routingReason(detail: RequestDetail): string {
  return routingSteps(detail).join(" → ");
}

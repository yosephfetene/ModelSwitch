import { useEffect, useRef, useState } from "react";

import { ApiError, type Attempt, type RequestDetail, requestDetail } from "./api";
import { STATUS_LABELS, formatCost, formatLatency, formatTime } from "./format";
import { routingSteps } from "./routing";

interface Props {
  id: string;
  onClose: () => void;
  onSessionEnded: () => void;
}

const CLASS_LABELS = {
  transient: "transient",
  target_problem: "target problem",
  request_problem: "request problem",
} as const;

const DECISION_LABELS = {
  not_retried: "not retried",
  retry: "retried",
  fallback: "fell back",
  exhausted: "nothing left to try",
} as const;

/** One request in full: why it went where it did, and what it cost. */
export function RequestPanel({ id, onClose, onSessionEnded }: Props) {
  const [detail, setDetail] = useState<RequestDetail | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const [reloads, setReloads] = useState(0);
  const heading = useRef<HTMLHeadingElement>(null);

  useEffect(() => {
    const controller = new AbortController();
    setDetail(null);
    setProblem(null);
    requestDetail(id, controller.signal)
      .then((next) => {
        if (!controller.signal.aborted) setDetail(next);
      })
      .catch((error: unknown) => {
        if (controller.signal.aborted) return;
        if (error instanceof ApiError && error.status === 401) {
          onSessionEnded();
        } else if (error instanceof ApiError && error.code === "request_not_found") {
          setProblem("This request is not in the records.");
        } else {
          setProblem(error instanceof Error ? error.message : "The request could not be loaded.");
        }
      });
    return () => controller.abort();
  }, [id, reloads, onSessionEnded]);

  // Keyboard users land in the panel, and Escape leaves it.
  useEffect(() => heading.current?.focus(), [id]);
  useEffect(() => {
    const close = (event: KeyboardEvent) => event.key === "Escape" && onClose();
    window.addEventListener("keydown", close);
    return () => window.removeEventListener("keydown", close);
  }, [onClose]);

  return (
    <aside className="panel" aria-labelledby="panel-heading">
      <div className="panel-head">
        <h2 id="panel-heading" ref={heading} tabIndex={-1}>
          Request
        </h2>
        <button type="button" className="secondary" aria-label="Close details" onClick={onClose}>
          ✕
        </button>
      </div>

      {problem ? (
        <div role="alert" className="problem-box">
          <p>{problem}</p>
          <button type="button" onClick={() => setReloads((count) => count + 1)}>
            Try again
          </button>
        </div>
      ) : !detail ? (
        <p role="status">Loading request…</p>
      ) : (
        <Detail detail={detail} />
      )}
    </aside>
  );
}

function Detail({ detail }: { detail: RequestDetail }) {
  const steps = routingSteps(detail);
  return (
    <>
      <p className="panel-status">
        <span className={`status status-${detail.status}`}>
          {STATUS_LABELS[detail.status] ?? detail.status}
        </span>
        <span className="muted">{formatTime(detail.created_at)} UTC</span>
        <span className="muted">{formatLatency(detail.latency_ms)}</span>
      </p>

      <section aria-labelledby="reason-heading">
        <h3 id="reason-heading">Routing</h3>
        {detail.routing_reason && (
          <p className="adaptive-reason">
            <span className="muted">Order chosen: </span>
            {detail.routing_reason}
          </p>
        )}
        <ol className="reason" aria-label="Routing reason">
          {steps.map((step, index) => (
            <li key={index}>{step}</li>
          ))}
        </ol>
      </section>

      <section aria-labelledby="attempts-heading">
        <h3 id="attempts-heading">Attempts ({detail.attempts.length})</h3>
        {detail.attempts.length === 0 ? (
          <p className="muted">No provider was called.</p>
        ) : (
          <ol className="attempts">
            {detail.attempts.map((attempt) => (
              <AttemptRow key={attempt.number} attempt={attempt} />
            ))}
          </ol>
        )}
      </section>

      <section aria-labelledby="usage-heading">
        <h3 id="usage-heading">Usage and cost</h3>
        <dl className="facts">
          <dt>Tokens in</dt>
          <dd>{detail.usage.input_tokens ?? "—"}</dd>
          <dt>Tokens out</dt>
          <dd>{detail.usage.output_tokens ?? "—"}</dd>
          <dt>Total</dt>
          <dd>{detail.usage.total_tokens ?? "—"}</dd>
          <dt>Usage</dt>
          <dd>{detail.usage.source === "provider_reported" ? "reported by the provider" : "not reported"}</dd>
          <dt>Estimated cost</dt>
          <dd>{formatCost(detail.cost?.usd)}</dd>
          {detail.price && (
            <>
              <dt>Price per million</dt>
              <dd>
                ${detail.price.input_usd_per_million} in / ${detail.price.output_usd_per_million} out
              </dd>
              <dt>Price source</dt>
              <dd className="wrap">
                <PriceSource source={detail.price.source} />
              </dd>
            </>
          )}
        </dl>
      </section>

      <section aria-labelledby="ids-heading">
        <h3 id="ids-heading">Identifiers</h3>
        <dl className="facts">
          <dt>Request ID</dt>
          <dd>
            <Copyable value={detail.id} />
          </dd>
          <dt>Client request ID</dt>
          <dd>{detail.client_request_id ? <Copyable value={detail.client_request_id} /> : "—"}</dd>
          <dt>Key</dt>
          <dd>
            {detail.key.name} <code className="muted">{detail.key.prefix}</code>
          </dd>
          <dt>Requested</dt>
          <dd>
            <code>{detail.requested_model}</code>
          </dd>
        </dl>
      </section>
    </>
  );
}

function AttemptRow({ attempt }: { attempt: Attempt }) {
  const error = attempt.error;
  return (
    <li className={`attempt attempt-${attempt.status}`}>
      <div className="attempt-line">
        <span className="attempt-number">#{attempt.number}</span>
        <span>
          {attempt.provider} / <code>{attempt.model}</code>
        </span>
        <span className={`status status-${attempt.status}`}>
          {STATUS_LABELS[attempt.status] ?? attempt.status}
        </span>
        <span className="muted attempt-latency">{formatLatency(attempt.latency_ms)}</span>
      </div>
      {attempt.status !== "succeeded" && (
        <div className="attempt-why muted">
          {[
            error?.code,
            error?.provider_status != null ? `HTTP ${error.provider_status}` : null,
            error?.class ? CLASS_LABELS[error.class] : null,
            DECISION_LABELS[attempt.retry_decision],
          ]
            .filter(Boolean)
            .join(" · ")}
        </div>
      )}
    </li>
  );
}

/**
 * Links a leading web address, such as `https://… (2026-11-06)`, and
 * shows the rest, or anything that is not a web address, as text.
 */
function PriceSource({ source }: { source: string }) {
  const match = /^(https?:\/\/\S+)(.*)$/s.exec(source);
  if (!match) return <>{source}</>;
  const [, url, rest] = match;
  return (
    <>
      <a href={url} target="_blank" rel="noreferrer noopener">
        {url}
      </a>
      {rest}
    </>
  );
}

function Copyable({ value }: { value: string }) {
  const [copied, setCopied] = useState(false);
  const canCopy = typeof navigator !== "undefined" && !!navigator.clipboard;
  return (
    <span className="copyable">
      <code>{value}</code>
      {canCopy && (
        <button
          type="button"
          className="secondary small"
          onClick={() =>
            navigator.clipboard.writeText(value).then(
              () => setCopied(true),
              () => setCopied(false),
            )
          }
        >
          {copied ? "Copied" : "Copy"}
        </button>
      )}
    </span>
  );
}

import { useEffect, useState } from "react";

import { ApiError, type Filters, type HistoryPage, requests } from "./api";
import { FilterForm } from "./FilterForm";
import { RequestTable } from "./RequestTable";

interface Props {
  /** The session ended or was never valid: back to login. */
  onSessionEnded: () => void;
}

/** Request history: filters, one page at a time, newest first. */
export function History({ onSessionEnded }: Props) {
  const [filters, setFilters] = useState<Filters>({});
  // Each older page is reached by the previous page's `next_before`; the
  // stack remembers them so "Newer" can go back.
  const [cursors, setCursors] = useState<string[]>([]);
  const [reloads, setReloads] = useState(0);
  const [page, setPage] = useState<HistoryPage | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  const before = cursors.at(-1) ?? null;

  useEffect(() => {
    const controller = new AbortController();
    setLoading(true);
    setProblem(null);
    requests(filters, before, controller.signal)
      .then((next) => {
        if (controller.signal.aborted) return;
        setPage(next);
        setLoading(false);
      })
      .catch((error: unknown) => {
        if (controller.signal.aborted) return;
        if (error instanceof ApiError && error.status === 401) {
          onSessionEnded();
          return;
        }
        setProblem(error instanceof Error ? error.message : "The requests could not be loaded.");
        setPage(null);
        setLoading(false);
      });
    // A newer query replaces this one; its answer must not land late.
    return () => controller.abort();
  }, [filters, before, reloads, onSessionEnded]);

  function apply(next: Filters) {
    setFilters(next);
    setCursors([]);
  }

  const nextBefore = page?.next_before ?? null;

  return (
    <section aria-label="Request history">
      <FilterForm onApply={apply} />

      <nav className="pager" aria-label="Pages">
        <button
          type="button"
          className="secondary"
          disabled={loading || cursors.length === 0}
          onClick={() => setCursors((stack) => stack.slice(0, -1))}
        >
          ← Newer
        </button>
        <span className="muted">Page {cursors.length + 1}</span>
        <button
          type="button"
          className="secondary"
          disabled={loading || nextBefore === null}
          onClick={() => nextBefore && setCursors((stack) => [...stack, nextBefore])}
        >
          Older →
        </button>
        <button
          type="button"
          className="secondary"
          disabled={loading}
          onClick={() => setReloads((count) => count + 1)}
        >
          Refresh
        </button>
      </nav>

      {loading ? (
        <p role="status">Loading requests…</p>
      ) : problem ? (
        <div role="alert" className="problem-box">
          <p>{problem}</p>
          <button type="button" onClick={() => setReloads((count) => count + 1)}>
            Try again
          </button>
        </div>
      ) : page && page.requests.length > 0 ? (
        <RequestTable requests={page.requests} />
      ) : (
        <p className="empty">No requests match these filters.</p>
      )}
    </section>
  );
}

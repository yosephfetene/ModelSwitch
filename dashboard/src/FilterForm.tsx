import { type FormEvent, useState } from "react";

import type { Filters, Status } from "./api";
import { STATUS_LABELS, utcInputToRfc3339 } from "./format";

interface Props {
  onApply: (filters: Filters) => void;
}

const EMPTY = { from: "", to: "", provider: "", status: "" };

export function FilterForm({ onApply }: Props) {
  const [draft, setDraft] = useState(EMPTY);
  const [problem, setProblem] = useState<string | null>(null);

  function set(name: keyof typeof EMPTY, value: string) {
    setDraft((current) => ({ ...current, [name]: value }));
  }

  function apply(event: FormEvent) {
    event.preventDefault();
    const filters: Filters = {
      from: utcInputToRfc3339(draft.from),
      to: utcInputToRfc3339(draft.to),
      provider: draft.provider.trim() || undefined,
      status: (draft.status || undefined) as Status | undefined,
    };
    // RFC 3339 times in UTC with the same layout compare as text.
    if (filters.from && filters.to && filters.from >= filters.to) {
      setProblem("“From” must be before “To”.");
      return;
    }
    setProblem(null);
    onApply(filters);
  }

  function reset() {
    setDraft(EMPTY);
    setProblem(null);
    onApply({});
  }

  return (
    <form className="filters" onSubmit={apply} aria-label="Filters">
      <label>
        From (UTC)
        <input
          type="datetime-local"
          value={draft.from}
          onChange={(event) => set("from", event.target.value)}
        />
      </label>
      <label>
        To (UTC)
        <input
          type="datetime-local"
          value={draft.to}
          onChange={(event) => set("to", event.target.value)}
        />
      </label>
      <label>
        Provider
        <input
          type="text"
          placeholder="any"
          spellCheck={false}
          value={draft.provider}
          onChange={(event) => set("provider", event.target.value)}
        />
      </label>
      <label>
        Status
        <select value={draft.status} onChange={(event) => set("status", event.target.value)}>
          <option value="">Any</option>
          {Object.entries(STATUS_LABELS).map(([value, label]) => (
            <option key={value} value={value}>
              {label}
            </option>
          ))}
        </select>
      </label>
      <div className="filter-actions">
        <button type="submit">Apply</button>
        <button type="button" className="secondary" onClick={reset}>
          Reset
        </button>
      </div>
      {problem && (
        <p role="alert" className="problem">
          {problem}
        </p>
      )}
    </form>
  );
}

import type { RequestSummary } from "./api";
import { STATUS_LABELS, formatCost, formatLatency, formatTime, formatTokens } from "./format";

interface Props {
  requests: RequestSummary[];
  /** The request whose details are open, if any. */
  selected: string | null;
  onSelect: (id: string) => void;
}

export function RequestTable({ requests, selected, onSelect }: Props) {
  return (
    <div className="table-wrap">
      <table>
        <thead>
          <tr>
            <th scope="col">Time (UTC)</th>
            <th scope="col">Key</th>
            <th scope="col">Requested</th>
            <th scope="col">Answered by</th>
            <th scope="col">Status</th>
            <th scope="col" className="num">
              Attempts
            </th>
            <th scope="col" className="num">
              Latency
            </th>
            <th scope="col" className="num">
              Tokens in / out
            </th>
            <th scope="col" className="num">
              Cost
            </th>
          </tr>
        </thead>
        <tbody>
          {requests.map((request) => (
            <tr
              key={request.id}
              className={request.id === selected ? "selected" : undefined}
              onClick={() => onSelect(request.id)}
            >
              <td>
                {/* A real button, so a keyboard can open the details too. */}
                <button
                  type="button"
                  className="row-link"
                  title={request.id}
                  aria-current={request.id === selected ? "true" : undefined}
                  aria-label={`Details of the request at ${formatTime(request.created_at)}`}
                  onClick={(event) => {
                    event.stopPropagation();
                    onSelect(request.id);
                  }}
                >
                  {formatTime(request.created_at)}
                </button>
              </td>
              <td>
                {request.key.name} <code className="muted">{request.key.prefix}</code>
              </td>
              <td>
                <code>{request.requested_model}</code>
              </td>
              <td>
                {request.provider} / <code>{request.model}</code>
              </td>
              <td>
                <span className={`status status-${request.status}`}>
                  {STATUS_LABELS[request.status] ?? request.status}
                </span>
                {request.error && (
                  <span className="error-code">{request.error.code ?? request.error.kind}</span>
                )}
              </td>
              <td className="num">{request.attempt_count}</td>
              <td className="num">{formatLatency(request.latency_ms)}</td>
              <td
                className="num"
                title={request.usage.source === "unavailable" ? "Not reported by the provider" : undefined}
              >
                {formatTokens(request.usage.input_tokens, request.usage.output_tokens)}
              </td>
              <td className="num" title={request.cost ? undefined : "No price for this model"}>
                {formatCost(request.cost?.usd)}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

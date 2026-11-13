import { describe, expect, it } from "vitest";

import { requestsQuery } from "./api";
import { formatCost, formatLatency, formatTime, formatTokens, utcInputToRfc3339 } from "./format";

describe("format", () => {
  it("shows times in UTC to the second", () => {
    expect(formatTime("2026-11-12T18:00:01.123Z")).toBe("2026-11-12 18:00:01");
  });

  it("reads datetime-local values as UTC", () => {
    expect(utcInputToRfc3339("2026-11-12T18:00")).toBe("2026-11-12T18:00:00Z");
    expect(utcInputToRfc3339("2026-11-12T18:00:30")).toBe("2026-11-12T18:00:30Z");
    expect(utcInputToRfc3339("")).toBeUndefined();
  });

  it("keeps costs as exact decimal text", () => {
    // As a float, 0.1 + 0.2 shows the rounding the API avoids.
    expect(formatCost("0.30000000000000000001")).toBe("$0.30000000000000000001");
    expect(formatCost("0.0000375")).toBe("$0.0000375");
    expect(formatCost(null)).toBe("—");
    expect(formatCost(undefined)).toBe("—");
  });

  it("never shows missing usage as zero", () => {
    expect(formatTokens(null, null)).toBe("—");
    expect(formatTokens(0, 0)).toBe("0 / 0");
    expect(formatTokens(12, null)).toBe("12 / —");
  });

  it("shows latency in ms below a second", () => {
    expect(formatLatency(412)).toBe("412 ms");
    expect(formatLatency(1530)).toBe("1.53 s");
  });
});

describe("requestsQuery", () => {
  it("sends only the filters that are set", () => {
    expect(requestsQuery({}, null)).toBe("/admin/api/requests");
    expect(
      requestsQuery(
        { from: "2026-11-12T00:00:00Z", provider: "groq", status: "failed", to: undefined },
        "req_x",
      ),
    ).toBe(
      "/admin/api/requests?from=2026-11-12T00%3A00%3A00Z&provider=groq&status=failed&before=req_x",
    );
  });
});

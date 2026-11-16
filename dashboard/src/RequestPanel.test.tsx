import { act, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import { App } from "./App";
import { apiError, attempt, detail, fakeGateway, json, page, summary } from "./test/gateway";

const ID_A = "req_0190f5a2-0000-7000-8000-00000000000a";
const ID_B = "req_0190f5a2-0000-7000-8000-00000000000b";
const loggedIn = () => json(200, { expires_in_secs: 3600 });

const fallback = detail({
  id: ID_A,
  requested_model: "reverse",
  client_request_id: "order-42",
  attempts: [
    attempt({
      number: 1,
      provider: "gemini",
      model: "gemini-2.5-flash",
      status: "failed",
      error: { kind: "status", code: "API_KEY_INVALID", provider_status: 400, class: "target_problem" },
      retry_decision: "fallback",
      latency_ms: 263,
    }),
    attempt({ number: 2, provider: "groq", model: "openai/gpt-oss-20b", latency_ms: 812 }),
  ],
});

function gateway(details: Record<string, () => Response | Promise<Response>> = {}) {
  return fakeGateway({
    "GET /admin/api/session": loggedIn,
    "GET /admin/api/requests": () =>
      json(200, page([summary({ id: ID_A }), summary({ id: ID_B, created_at: "2026-11-12T17:00:00.000Z" })])),
    "GET /admin/api/requests/*": (call) => {
      const id = call.path.split("/").pop() ?? "";
      const handler = details[id];
      return handler ? handler() : apiError(404, "request_not_found", "No request has this ID.");
    },
  });
}

const detailCalls = (g: ReturnType<typeof fakeGateway>) =>
  g.calls.filter((call) => call.path.startsWith("/admin/api/requests/"));

async function openFirstRow(user: ReturnType<typeof userEvent.setup>) {
  await user.click(await screen.findByRole("button", { name: /Details of the request at 2026-11-12 18:00:01/ }));
  return screen.findByRole("complementary", { name: "Request" });
}

describe("request panel", () => {
  it("opens beside the table with the routing reason, attempts, and price", async () => {
    gateway({ [ID_A]: () => json(200, fallback) });
    const user = userEvent.setup();
    render(<App />);
    const panel = within(await openFirstRow(user));

    expect(screen.getByRole("table")).toBeInTheDocument();
    const steps = within(await panel.findByRole("list", { name: "Routing reason" }))
      .getAllByRole("listitem")
      .map((item) => item.textContent);
    expect(steps).toEqual([
      "Alias reverse",
      "gemini rejected the key, fell back",
      "answered by groq / openai/gpt-oss-20b",
    ]);
    expect(panel.getByText("Attempts (2)")).toBeInTheDocument();
    expect(panel.getByText("API_KEY_INVALID · HTTP 400 · target problem · fell back")).toBeInTheDocument();
    expect(panel.getByText("$0.0000375")).toBeInTheDocument();
    expect(panel.getByText("$0.05 in / $0.08 out")).toBeInTheDocument();
    expect(panel.getByRole("link", { name: "https://groq.com/pricing" })).toHaveAttribute(
      "rel",
      "noreferrer noopener",
    );
    expect(panel.getByText(ID_A)).toBeInTheDocument();
    expect(panel.getByText("order-42")).toBeInTheDocument();
  });

  it("puts the request in the URL so Back and links work", async () => {
    gateway({ [ID_A]: () => json(200, fallback) });
    const user = userEvent.setup();
    render(<App />);
    await openFirstRow(user);

    expect(window.location.hash).toBe(`#${ID_A}`);
    expect(screen.getByRole("button", { name: /18:00:01/ })).toHaveAttribute("aria-current", "true");

    act(() => window.history.back());
    await screen.findByRole("table");
    await waitForGone();
    expect(window.location.hash).toBe("");
  });

  it("opens from a shared link once the history loads", async () => {
    window.history.replaceState(null, "", `/#${ID_A}`);
    const g = gateway({ [ID_A]: () => json(200, fallback) });
    render(<App />);

    const panel = within(await screen.findByRole("complementary", { name: "Request" }));
    expect(await panel.findByText("Attempts (2)")).toBeInTheDocument();
    expect(detailCalls(g).map((call) => call.path)).toEqual([`/admin/api/requests/${ID_A}`]);
  });

  it("ignores a hash that is not a request ID", async () => {
    window.history.replaceState(null, "", "/#req_../../session");
    const g = gateway();
    render(<App />);

    await screen.findByRole("table");
    expect(screen.queryByRole("complementary")).not.toBeInTheDocument();
    expect(detailCalls(g)).toHaveLength(0);
  });

  it("closes with the button or Escape", async () => {
    gateway({ [ID_A]: () => json(200, fallback) });
    const user = userEvent.setup();
    render(<App />);

    await openFirstRow(user);
    await user.click(screen.getByRole("button", { name: "Close details" }));
    expect(screen.queryByRole("complementary")).not.toBeInTheDocument();
    expect(window.location.hash).toBe("");

    await openFirstRow(user);
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("complementary")).not.toBeInTheDocument();
  });

  it("moves focus into the panel when it opens", async () => {
    gateway({ [ID_A]: () => json(200, fallback) });
    const user = userEvent.setup();
    render(<App />);
    await openFirstRow(user);

    expect(screen.getByRole("heading", { name: "Request" })).toHaveFocus();
  });

  it("says when the request is not in the records", async () => {
    gateway();
    const user = userEvent.setup();
    render(<App />);
    const panel = within(await openFirstRow(user));

    expect(await panel.findByRole("alert")).toHaveTextContent("This request is not in the records.");
  });

  it("can try again after a failure", async () => {
    let fail = true;
    gateway({
      [ID_A]: () =>
        fail ? apiError(503, "service_unavailable", "The database is not answering.") : json(200, fallback),
    });
    const user = userEvent.setup();
    render(<App />);
    const panel = within(await openFirstRow(user));

    expect(await panel.findByRole("alert")).toHaveTextContent("The database is not answering.");
    fail = false;
    await user.click(panel.getByRole("button", { name: "Try again" }));
    expect(await panel.findByText("Attempts (2)")).toBeInTheDocument();
  });

  it("returns to login when the session ends", async () => {
    gateway({ [ID_A]: () => apiError(401, "login_required") });
    const user = userEvent.setup();
    render(<App />);
    await user.click(await screen.findByRole("button", { name: /18:00:01/ }));

    expect(await screen.findByText("Your session has ended. Log in again.")).toBeInTheDocument();
  });

  it("shows the latest row clicked, never a late answer", async () => {
    let releaseA: (response: Response) => void = () => {};
    gateway({
      [ID_A]: () =>
        new Promise<Response>((resolve) => {
          releaseA = resolve;
        }),
      [ID_B]: () => json(200, detail({ id: ID_B, requested_model: "row-b" })),
    });
    const user = userEvent.setup();
    render(<App />);

    await user.click(await screen.findByRole("button", { name: /18:00:01/ }));
    await user.click(screen.getByRole("button", { name: /17:00:00/ }));
    const panel = within(await screen.findByRole("complementary", { name: "Request" }));
    expect(await panel.findByText("Alias row-b")).toBeInTheDocument();

    releaseA(json(200, fallback));
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(panel.queryByText("Alias reverse")).not.toBeInTheDocument();
    expect(panel.getByText("Alias row-b")).toBeInTheDocument();
  });

  it("links only the address when a note follows it", async () => {
    gateway({
      [ID_A]: () =>
        json(
          200,
          detail({
            id: ID_A,
            price: {
              input_usd_per_million: "1",
              output_usd_per_million: "2",
              source: "https://console.groq.com/docs/models (2026-11-06)",
            },
          }),
        ),
    });
    const user = userEvent.setup();
    render(<App />);
    const panel = within(await openFirstRow(user));

    const link = await panel.findByRole("link");
    expect(link).toHaveAttribute("href", "https://console.groq.com/docs/models");
    expect(link.parentElement).toHaveTextContent("https://console.groq.com/docs/models (2026-11-06)");
  });

  it("never links a price source that is not a web address", async () => {
    gateway({
      [ID_A]: () =>
        json(200, detail({ id: ID_A, price: { input_usd_per_million: "1", output_usd_per_million: "2", source: "javascript:alert(1)" } })),
    });
    const user = userEvent.setup();
    render(<App />);
    const panel = within(await openFirstRow(user));

    expect(await panel.findByText("javascript:alert(1)")).toBeInTheDocument();
    expect(panel.queryByRole("link")).not.toBeInTheDocument();
  });
});

async function waitForGone() {
  for (let i = 0; i < 50 && screen.queryByRole("complementary"); i++) {
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  expect(screen.queryByRole("complementary")).not.toBeInTheDocument();
}

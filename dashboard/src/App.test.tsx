import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import { App } from "./App";
import { apiError, fakeGateway, json, noContent, page, summary } from "./test/gateway";

const TOKEN = "admin-token-for-tests-0123456789abcdef";
const loggedIn = () => json(200, { expires_in_secs: 3600 });
const loggedOut = () => apiError(401, "login_required");

async function logIn(user: ReturnType<typeof userEvent.setup>) {
  await user.type(await screen.findByLabelText("Admin token"), TOKEN);
  await user.click(screen.getByRole("button", { name: "Log in" }));
}

describe("login", () => {
  it("asks for the token when there is no session", async () => {
    fakeGateway({ "GET /admin/api/session": loggedOut });
    render(<App />);

    expect(await screen.findByLabelText("Admin token")).toHaveAttribute("type", "password");
    expect(screen.queryByRole("table")).not.toBeInTheDocument();
  });

  it("sends the token once as JSON, then shows history", async () => {
    let session = false;
    const gateway = fakeGateway({
      "GET /admin/api/session": () => (session ? loggedIn() : loggedOut()),
      "POST /admin/api/login": () => {
        session = true;
        return noContent();
      },
      "GET /admin/api/requests": () => json(200, page([summary()])),
    });
    const user = userEvent.setup();
    render(<App />);
    await logIn(user);

    expect(await screen.findByRole("table")).toBeInTheDocument();
    const logins = gateway.calls.filter((call) => call.path === "/admin/api/login");
    expect(logins).toHaveLength(1);
    expect(logins[0]?.method).toBe("POST");
    expect(logins[0]?.headers.get("content-type")).toBe("application/json");
    expect(JSON.parse(logins[0]?.body ?? "")).toEqual({ token: TOKEN });
    // The token goes nowhere else.
    for (const call of gateway.calls.filter((c) => c.path !== "/admin/api/login")) {
      expect(call.body ?? "").not.toContain(TOKEN);
      expect(call.params.toString()).not.toContain(TOKEN);
    }
  });

  it("says when the token is wrong and clears it", async () => {
    fakeGateway({
      "GET /admin/api/session": loggedOut,
      "POST /admin/api/login": () => apiError(401, "login_failed", "Wrong admin token."),
    });
    const user = userEvent.setup();
    render(<App />);
    await logIn(user);

    expect(await screen.findByRole("alert")).toHaveTextContent("That admin token is not right.");
    expect(screen.getByLabelText("Admin token")).toHaveValue("");
  });

  it("says how long to wait after too many failures", async () => {
    fakeGateway({
      "GET /admin/api/session": loggedOut,
      "POST /admin/api/login": () =>
        apiError(429, "login_throttled", "Too many failed logins.", { "retry-after": "42" }),
    });
    const user = userEvent.setup();
    render(<App />);
    await logIn(user);

    expect(await screen.findByRole("alert")).toHaveTextContent("Try again in 42 seconds.");
  });

  it("offers a retry when the gateway cannot be reached", async () => {
    let up = false;
    fakeGateway({
      "GET /admin/api/session": () => {
        if (!up) throw new TypeError("Failed to fetch");
        return loggedOut();
      },
    });
    const user = userEvent.setup();
    render(<App />);

    expect(await screen.findByRole("alert")).toHaveTextContent("The gateway could not be reached.");
    up = true;
    await user.click(screen.getByRole("button", { name: "Try again" }));
    expect(await screen.findByLabelText("Admin token")).toBeInTheDocument();
  });

  it("logs out and returns to login", async () => {
    const gateway = fakeGateway({
      "GET /admin/api/session": loggedIn,
      "GET /admin/api/requests": () => json(200, page([])),
      "POST /admin/api/logout": noContent,
    });
    const user = userEvent.setup();
    render(<App />);
    await user.click(await screen.findByRole("button", { name: "Log out" }));

    expect(await screen.findByText("Logged out.")).toBeInTheDocument();
    expect(screen.getByLabelText("Admin token")).toBeInTheDocument();
    expect(gateway.calls.some((c) => c.method === "POST" && c.path === "/admin/api/logout")).toBe(true);
  });

  it("returns to login when the session ends", async () => {
    fakeGateway({
      "GET /admin/api/session": loggedIn,
      "GET /admin/api/requests": loggedOut,
    });
    render(<App />);

    expect(await screen.findByText("Your session has ended. Log in again.")).toBeInTheDocument();
    expect(screen.getByLabelText("Admin token")).toBeInTheDocument();
  });
});

describe("history", () => {
  function signedIn(requests: Parameters<typeof fakeGateway>[0]["x"]) {
    return fakeGateway({
      "GET /admin/api/session": loggedIn,
      "GET /admin/api/requests": requests,
    });
  }

  it("lists requests with exact costs and missing usage as a dash", async () => {
    signedIn(() =>
      json(
        200,
        page([
          summary(),
          summary({
            id: "req_0190f5a2-0000-7000-8000-000000000002",
            status: "failed",
            provider: "gemini",
            model: "gemini-2.5-flash",
            error: { kind: "provider", code: "API_KEY_INVALID", provider_status: 400 },
            usage: { input_tokens: null, output_tokens: null, total_tokens: null, source: "unavailable" },
            cost: null,
            attempt_count: 2,
          }),
        ]),
      ),
    );
    render(<App />);

    const rows = within(await screen.findByRole("table")).getAllByRole("row").slice(1);
    expect(rows).toHaveLength(2);
    const [ok, failed] = rows.map((row) => within(row));
    expect(ok?.getByText("2026-11-12 18:00:01")).toBeInTheDocument();
    expect(ok?.getByText("$0.0000375")).toBeInTheDocument();
    expect(ok?.getByText("12 / 34")).toBeInTheDocument();
    expect(ok?.getByText("Succeeded")).toBeInTheDocument();
    expect(failed?.getByText("Failed")).toBeInTheDocument();
    expect(failed?.getByText("API_KEY_INVALID")).toBeInTheDocument();
    expect(failed?.getAllByText("—")).toHaveLength(2);
    expect(failed?.getByText("2")).toBeInTheDocument();
  });

  it("says so when nothing matches", async () => {
    signedIn(() => json(200, page([])));
    render(<App />);

    expect(await screen.findByText("No requests match these filters.")).toBeInTheDocument();
    expect(screen.queryByRole("table")).not.toBeInTheDocument();
  });

  it("shows the gateway's message on failure and can try again", async () => {
    let fail = true;
    const gateway = signedIn(() =>
      fail
        ? apiError(503, "service_unavailable", "The database is not answering.")
        : json(200, page([summary()])),
    );
    const user = userEvent.setup();
    render(<App />);

    expect(await screen.findByRole("alert")).toHaveTextContent("The database is not answering.");
    fail = false;
    await user.click(screen.getByRole("button", { name: "Try again" }));
    expect(await screen.findByRole("table")).toBeInTheDocument();
    expect(gateway.requestCalls()).toHaveLength(2);
  });

  it("sends filters in UTC and starts again from the newest page", async () => {
    const gateway = signedIn((call) =>
      json(200, page([summary()], call.params.has("before") ? null : "req_cursor")),
    );
    const user = userEvent.setup();
    render(<App />);
    await user.click(await screen.findByRole("button", { name: "Older →" }));
    await screen.findByText("Page 2");

    const filters = screen.getByRole("form", { name: "Filters" });
    await user.type(within(filters).getByLabelText("From (UTC)"), "2026-11-12T08:30");
    await user.type(within(filters).getByLabelText("Provider"), " groq ");
    await user.selectOptions(within(filters).getByLabelText("Status"), "failed");
    await user.click(within(filters).getByRole("button", { name: "Apply" }));

    await screen.findByText("Page 1");
    await waitFor(() => expect(gateway.requestCalls()).toHaveLength(3));
    const params = gateway.requestCalls().at(-1)?.params;
    expect(params?.get("from")).toBe("2026-11-12T08:30:00Z");
    expect(params?.get("provider")).toBe("groq");
    expect(params?.get("status")).toBe("failed");
    expect(params?.has("to")).toBe(false);
    expect(params?.has("before")).toBe(false);
  });

  it("refuses a time range that ends before it starts", async () => {
    const gateway = signedIn(() => json(200, page([summary()])));
    const user = userEvent.setup();
    render(<App />);
    await screen.findByRole("table");

    const filters = screen.getByRole("form", { name: "Filters" });
    await user.type(within(filters).getByLabelText("From (UTC)"), "2026-11-12T10:00");
    await user.type(within(filters).getByLabelText("To (UTC)"), "2026-11-12T09:00");
    await user.click(within(filters).getByRole("button", { name: "Apply" }));

    expect(within(filters).getByRole("alert")).toHaveTextContent("“From” must be before “To”.");
    expect(gateway.requestCalls()).toHaveLength(1);
  });

  it("pages older with next_before and back to newer", async () => {
    const gateway = signedIn((call) => {
      const before = call.params.get("before");
      if (before === null) return json(200, page([summary()], "req_a"));
      if (before === "req_a") return json(200, page([summary({ id: "req_b" })], "req_b"));
      return json(200, page([summary({ id: "req_c" })], null));
    });
    const user = userEvent.setup();
    render(<App />);
    const older = await screen.findByRole("button", { name: "Older →" });
    const newer = screen.getByRole("button", { name: "← Newer" });
    expect(newer).toBeDisabled();

    await user.click(older);
    await screen.findByText("Page 2");
    await user.click(screen.getByRole("button", { name: "Older →" }));
    await screen.findByText("Page 3");
    await waitFor(() => expect(screen.getByRole("button", { name: "Older →" })).toBeDisabled());

    await user.click(screen.getByRole("button", { name: "← Newer" }));
    await screen.findByText("Page 2");
    const befores = gateway.requestCalls().map((call) => call.params.get("before"));
    expect(befores).toEqual([null, "req_a", "req_b", "req_a"]);
  });

  it("ignores an answer that arrives after a newer query", async () => {
    let releaseFirst: (response: Response) => void = () => {};
    signedIn((call) => {
      if (!call.params.has("status")) {
        return new Promise<Response>((resolve) => {
          releaseFirst = resolve;
        });
      }
      return json(200, page([summary({ provider: "newer-answer" })]));
    });
    const user = userEvent.setup();
    render(<App />);

    const filters = await screen.findByRole("form", { name: "Filters" });
    await user.selectOptions(within(filters).getByLabelText("Status"), "succeeded");
    await user.click(within(filters).getByRole("button", { name: "Apply" }));
    expect(await screen.findByText(/newer-answer/)).toBeInTheDocument();

    releaseFirst(json(200, page([summary({ provider: "late-answer" })])));
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(screen.queryByText(/late-answer/)).not.toBeInTheDocument();
    expect(screen.getByText(/newer-answer/)).toBeInTheDocument();
  });
});

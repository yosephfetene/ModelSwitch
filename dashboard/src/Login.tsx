import { type FormEvent, useState } from "react";

import { ApiError, login } from "./api";

interface Props {
  onLoggedIn: () => void;
  /** Why the operator is here, such as an ended session. */
  notice?: string;
}

function loginProblem(error: unknown): string {
  if (error instanceof ApiError) {
    if (error.code === "login_failed") return "That admin token is not right.";
    if (error.code === "login_throttled") {
      const wait = error.retryAfterSecs ?? 60;
      return `Too many failed logins. Try again in ${wait} seconds.`;
    }
    return error.message;
  }
  return "Login failed.";
}

export function Login({ onLoggedIn, notice }: Props) {
  const [token, setToken] = useState("");
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);

  async function submit(event: FormEvent) {
    event.preventDefault();
    setBusy(true);
    setProblem(null);
    try {
      await login(token);
      onLoggedIn();
    } catch (error) {
      setProblem(loginProblem(error));
      setBusy(false);
    } finally {
      // The token is only needed for this one request.
      setToken("");
    }
  }

  return (
    <main className="login">
      <h1>ModelSwitch</h1>
      <p className="muted">Operator dashboard</p>
      {notice && <p className="notice">{notice}</p>}
      <form onSubmit={submit}>
        <label htmlFor="token">Admin token</label>
        <input
          id="token"
          type="password"
          autoComplete="off"
          spellCheck={false}
          value={token}
          onChange={(event) => setToken(event.target.value)}
          required
        />
        {problem && (
          <p role="alert" className="problem">
            {problem}
          </p>
        )}
        <button type="submit" disabled={busy || token === ""}>
          {busy ? "Logging in…" : "Log in"}
        </button>
      </form>
    </main>
  );
}

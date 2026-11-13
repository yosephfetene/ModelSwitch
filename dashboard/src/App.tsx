import { useCallback, useEffect, useState } from "react";

import { ApiError, logout, session } from "./api";
import { History } from "./History";
import { Login } from "./Login";

type View =
  | { kind: "checking" }
  | { kind: "unreachable"; message: string }
  | { kind: "login"; notice?: string }
  | { kind: "ready" };

export function App() {
  const [view, setView] = useState<View>({ kind: "checking" });
  const [checks, setChecks] = useState(0);

  useEffect(() => {
    let current = true;
    session()
      .then(() => current && setView({ kind: "ready" }))
      .catch((error: unknown) => {
        if (!current) return;
        if (error instanceof ApiError && error.status === 401) {
          setView({ kind: "login" });
        } else {
          const message = error instanceof Error ? error.message : "The gateway could not be reached.";
          setView({ kind: "unreachable", message });
        }
      });
    return () => {
      current = false;
    };
  }, [checks]);

  const sessionEnded = useCallback(
    () => setView({ kind: "login", notice: "Your session has ended. Log in again." }),
    [],
  );

  async function logOut() {
    try {
      await logout();
    } catch {
      // The cookie expires on its own; the page forgets the session anyway.
    }
    setView({ kind: "login", notice: "Logged out." });
  }

  switch (view.kind) {
    case "checking":
      return <p role="status" className="centered">Loading…</p>;
    case "unreachable":
      return (
        <div role="alert" className="centered problem-box">
          <p>{view.message}</p>
          <button type="button" onClick={() => setChecks((count) => count + 1)}>
            Try again
          </button>
        </div>
      );
    case "login":
      return <Login notice={view.notice} onLoggedIn={() => setView({ kind: "ready" })} />;
    case "ready":
      return (
        <>
          <header className="top">
            <h1>ModelSwitch</h1>
            <span className="muted">Request history</span>
            <button type="button" className="secondary" onClick={logOut}>
              Log out
            </button>
          </header>
          <main>
            <History onSessionEnded={sessionEnded} />
          </main>
        </>
      );
  }
}

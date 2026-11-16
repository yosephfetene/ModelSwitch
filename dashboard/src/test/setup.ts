import "@testing-library/jest-dom/vitest";
import { cleanup } from "@testing-library/react";
import { afterEach } from "vitest";

afterEach(() => {
  cleanup();
  // The open request lives in the URL; each test starts without one.
  window.history.replaceState(null, "", "/");
});

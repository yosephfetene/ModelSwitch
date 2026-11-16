import { useCallback, useEffect, useState } from "react";

import { REQUEST_ID } from "./api";

// The open request lives in the URL hash (`#req_...`): links can be shared
// and Back closes the panel, yet the server only ever sees `/`, so the
// page is still served from index.html and assets/ alone.

function fromHash(): string | null {
  let id: string;
  try {
    id = decodeURIComponent(window.location.hash.slice(1));
  } catch {
    return null;
  }
  return REQUEST_ID.test(id) ? id : null;
}

export function useSelectedRequest(): [string | null, (id: string | null) => void] {
  const [selected, setSelected] = useState(fromHash);

  // Back, Forward, and edited addresses all change the hash.
  useEffect(() => {
    const follow = () => setSelected(fromHash());
    window.addEventListener("hashchange", follow);
    return () => window.removeEventListener("hashchange", follow);
  }, []);

  const select = useCallback((id: string | null) => {
    const { pathname, search } = window.location;
    window.history.pushState(null, "", id ? `${pathname}${search}#${id}` : `${pathname}${search}`);
    setSelected(id);
  }, []);

  return [selected, select];
}

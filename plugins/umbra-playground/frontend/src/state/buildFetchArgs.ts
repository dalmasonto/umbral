import type { RequestDraft } from "./store";

export interface FetchArgs {
  url: string;
  init: RequestInit;
}

export type BuildError =
  | { kind: "missing_path_param"; name: string }
  | { kind: "invalid_json_body"; message: string };

export function buildFetchArgs(draft: RequestDraft): {
  ok: true; args: FetchArgs;
} | { ok: false; error: BuildError } {
  // 1. Resolve path template params.
  let url = draft.url;
  const templateNames = [...url.matchAll(/\{([a-zA-Z_][a-zA-Z0-9_]*)\}/g)].map((m) => m[1]);
  for (const name of templateNames) {
    const value = draft.params[name];
    if (!value) {
      return { ok: false, error: { kind: "missing_path_param", name } };
    }
    url = url.replace(`{${name}}`, encodeURIComponent(value));
  }

  // 2. Build query string.
  const queryEntries = Object.entries(draft.params).filter(
    ([k]) => !templateNames.includes(k),
  );
  if (queryEntries.length > 0) {
    const qs = queryEntries
      .map(
        ([k, v]) => `${encodeURIComponent(k)}=${encodeURIComponent(v)}`,
      )
      .join("&");
    url += url.includes("?") ? `&${qs}` : `?${qs}`;
  }

  // 3. Headers.
  const headers: Record<string, string> = { ...draft.headers };
  if (draft.bearerToken) {
    headers["Authorization"] = `Bearer ${draft.bearerToken}`;
  }

  // 4. Body.
  const method = draft.method.toUpperCase();
  let body: string | undefined;
  if (draft.body && method !== "GET" && method !== "HEAD") {
    if (
      !headers["Content-Type"] ||
      headers["Content-Type"].includes("application/json")
    ) {
      try {
        JSON.parse(draft.body);
      } catch (e) {
        return {
          ok: false,
          error: {
            kind: "invalid_json_body",
            message: e instanceof Error ? e.message : String(e),
          },
        };
      }
      if (!headers["Content-Type"]) {
        headers["Content-Type"] = "application/json";
      }
    }
    body = draft.body;
  }

  return {
    ok: true,
    args: {
      url,
      init: { method, headers, body },
    },
  };
}

import type { RequestDraft } from "./store";

export interface FetchArgs {
  url: string;
  init: RequestInit;
}

export type BuildError =
  | { kind: "missing_path_param"; name: string }
  | { kind: "invalid_json_body"; message: string };

export function buildFetchArgs(draft: RequestDraft): {
  ok: true;
  args: FetchArgs;
} | {
  ok: false;
  error: BuildError;
} {
  // 1. Resolve path template params.
  let url = draft.url;
  const templateNames = [...url.matchAll(/\{([a-zA-Z_][a-zA-Z0-9_]*)\}/g)].map(
    (m) => m[1],
  );
  for (const name of templateNames) {
    const item = draft.params.find((p) => p.key === name && p.enabled);
    const value = item?.value ?? "";
    if (!value) {
      return { ok: false, error: { kind: "missing_path_param", name } };
    }
    url = url.replace(`{${name}}`, encodeURIComponent(value));
  }

  // 2. Build query string from enabled params that are NOT path templates.
  const queryEntries = draft.params.filter(
    (p) => p.enabled && p.key && !templateNames.includes(p.key),
  );
  if (queryEntries.length > 0) {
    const qs = queryEntries
      .map(
        ({ key, value }) =>
          `${encodeURIComponent(key)}=${encodeURIComponent(value)}`,
      )
      .join("&");
    url += url.includes("?") ? `&${qs}` : `?${qs}`;
  }

  // 3. Headers.
  const headers: Record<string, string> = {};
  for (const h of draft.headers) {
    if (h.enabled && h.key) headers[h.key] = h.value;
  }

  // 4. Auth.
  if (draft.authToken) {
    headers["Authorization"] = `${draft.authScheme} ${draft.authToken}`;
  }

  // 5. Body.
  const method = draft.method.toUpperCase();
  let body: string | undefined;
  if (method !== "GET" && method !== "HEAD") {
    if (draft.bodyType === "json") {
      if (draft.body) {
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
    } else if (draft.bodyType === "form") {
      const entries = draft.formFields.filter((f) => f.enabled && f.key);
      if (entries.length > 0) {
        const qs = entries
          .map(
            ({ key, value }) =>
              `${encodeURIComponent(key)}=${encodeURIComponent(value)}`,
          )
          .join("&");
        body = qs;
        headers["Content-Type"] = "application/x-www-form-urlencoded";
      }
    }
  }

  return {
    ok: true,
    args: {
      url,
      init: { method, headers, body },
    },
  };
}

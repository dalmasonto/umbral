import type { KVItem, RequestDraft } from "./store";
import { getFile } from "./fileRegistry";

export interface FetchArgs {
  url: string;
  init: RequestInit;
}

export interface BuildFetchOptions {
  baseUrl?: string;
  variables?: KVItem[];
  includeCredentials?: boolean;
}

export type BuildError =
  | { kind: "missing_path_param"; name: string }
  | { kind: "invalid_json_body"; message: string };

const PATH_TEMPLATE_RE = /(?<!\{)\{([a-zA-Z_][a-zA-Z0-9_]*)\}(?!\})/g;

function variableMap(variables: KVItem[] = []): Record<string, string> {
  const map: Record<string, string> = {};
  for (const variable of variables) {
    if (!variable.enabled || !variable.key) continue;
    map[variable.key] = variable.value;
  }
  return map;
}

function interpolate(value: string, variables: Record<string, string>): string {
  return value.replace(/\{\{\s*([^{}\s]+)\s*\}\}/g, (match, key: string) =>
    Object.prototype.hasOwnProperty.call(variables, key)
      ? variables[key]
      : match,
  );
}

function joinBaseUrl(url: string, baseUrl?: string): string {
  const base = baseUrl?.trim();
  if (!base || /^[a-z][a-z\d+.-]*:/i.test(url)) return url;
  return `${base.replace(/\/+$/, "")}/${url.replace(/^\/+/, "")}`;
}

export function buildFetchArgs(draft: RequestDraft, options: BuildFetchOptions = {}): {
  ok: true;
  args: FetchArgs;
} | {
  ok: false;
  error: BuildError;
} {
  const variables = variableMap(options.variables);

  // 1. Resolve path template params.
  let url = interpolate(draft.url, variables).split("?")[0]; // strip any existing query string
  const templateNames = [...url.matchAll(PATH_TEMPLATE_RE)].map((m) => m[1]);
  for (const name of templateNames) {
    const item = draft.params.find((p) => p.key === name && p.enabled);
    const value = interpolate(item?.value ?? "", variables);
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
          `${encodeURIComponent(key)}=${encodeURIComponent(
            interpolate(value, variables),
          )}`,
      )
      .join("&");
    url += `?${qs}`;
  }
  url = joinBaseUrl(url, interpolate(options.baseUrl ?? "", variables));

  // 3. Headers.
  const headers: Record<string, string> = {};
  for (const h of draft.headers) {
    if (h.enabled && h.key) headers[h.key] = interpolate(h.value, variables);
  }

  // 4. Auth.
  if (draft.authToken) {
    headers["Authorization"] = `${interpolate(
      draft.authScheme,
      variables,
    )} ${interpolate(draft.authToken, variables)}`;
  }

  // 5. Body.
  const method = draft.method.toUpperCase();
  let body: BodyInit | undefined;
  if (method !== "GET" && method !== "HEAD") {
    if (draft.bodyType === "json") {
      if (draft.body) {
        const resolvedBody = interpolate(draft.body, variables);
        if (
          !headers["Content-Type"] ||
          headers["Content-Type"].includes("application/json")
        ) {
          try {
            JSON.parse(resolvedBody);
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
        body = resolvedBody;
      }
    } else if (draft.bodyType === "form") {
      const entries = draft.formFields.filter((f) => f.enabled && f.key);
      const hasFiles = entries.some((f) => f.type === "file");

      if (hasFiles) {
        const formData = new FormData();
        for (const f of entries) {
          if (f.type === "file") {
            const file = getFile(`${f.key}`);
            if (file) {
              formData.append(f.key, file);
            }
          } else {
            formData.append(f.key, interpolate(f.value, variables));
          }
        }
        body = formData;
        // Browser sets Content-Type with boundary automatically
        delete headers["Content-Type"];
      } else if (entries.length > 0) {
        const qs = entries
          .map(
            ({ key, value }) =>
              `${encodeURIComponent(key)}=${encodeURIComponent(
                interpolate(value, variables),
              )}`,
          )
          .join("&");
        body = qs;
        headers["Content-Type"] = "application/x-www-form-urlencoded";
      }
    }
  }

  const init: RequestInit = { method, headers, body };
  if (options.includeCredentials) {
    init.credentials = "include";
  }

  return {
    ok: true,
    args: {
      url,
      init,
    },
  };
}

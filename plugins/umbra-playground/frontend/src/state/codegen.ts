/** Multi-language code generation for the captured request.
 *
 *  The pre-existing `toCurl` in `ResponseViewer.tsx` rendered
 *  `req.url` raw — the path the user typed in the URL bar
 *  (`/api/product/`) instead of the fully-resolved URL including
 *  base, path-template substitutions, and query string. It also
 *  skipped workspace defaults (settings.defaultHeaders,
 *  settings.globalAuth, variable interpolation). The codegen
 *  helpers in this module pipe every record through the canonical
 *  `buildFetchArgs` so the snippet is byte-for-byte what the
 *  playground actually sent.
 *
 *  Languages, in render order (JS/TS first — the request was JS/TS
 *  in the playground, the snippet's home is the consumer's
 *  JS/TS app):
 *
 *  - JavaScript / TypeScript via `fetch`
 *  - cURL
 *  - Python via `requests`
 *  - Rust via `reqwest` (blocking)
 */

import { buildFetchArgs } from "./buildFetchArgs";
import type { KVItem, PlaygroundSettings, ResponseRecord } from "./store";

export type CodegenLanguage = "js" | "curl" | "python" | "rust";

export const LANGUAGES: { id: CodegenLanguage; label: string }[] = [
  { id: "js", label: "JS / TS" },
  { id: "curl", label: "cURL" },
  { id: "python", label: "Python" },
  { id: "rust", label: "Rust" },
];

/** A resolved snapshot of the request that lands on the wire.
 *  Generated via `buildFetchArgs` so query string, path params,
 *  base URL, default headers, and global auth all match what the
 *  browser actually sent. */
export interface RequestSnapshot {
  method: string;
  /** Fully-resolved URL. Includes the resolved base URL when one
   *  is configured, and the query-string when params are set. */
  url: string;
  /** Every header that will go on the wire, post-merge. Keys are
   *  the verbatim names the user picked; `fetch` doesn't lowercase
   *  them. */
  headers: Record<string, string>;
  /** JSON or form body as a string, or `null` for GET / HEAD /
   *  empty bodies. Multipart uploads land as `null` here — see
   *  `formFields` for the source. */
  body: string | null;
  /** Original body shape, so the generator can pick `application/
   *  json` vs `application/x-www-form-urlencoded` vs `multipart/
   *  form-data` rendering. */
  bodyType: "json" | "form" | "none";
  /** Multipart fields (post-interpolation) when a form body
   *  contains files; empty for non-multipart cases. The
   *  generators pick this up to render `-F` flags / `FormData`
   *  appends. */
  formFields: KVItem[];
  /** `credentials: include` flag — needed for the JS snippet to
   *  match the browser's cookie behaviour. */
  includeCredentials: boolean;
}

/** Build a `RequestSnapshot` from a recorded response + the
 *  current workspace settings. Returns `null` when the recorded
 *  request can't be rebuilt (a path param the response captured
 *  but the current draft no longer has — extremely rare). */
export function snapshotFromRecord(
  record: ResponseRecord,
  settings: PlaygroundSettings,
): RequestSnapshot | null {
  // `buildFetchArgs` only reads headers off the draft; the
  // workspace-default headers are merged into `current.headers`
  // by `resetCurrent` at operation-pick time. Replay that step
  // here so the generated snippet matches the wire even when the
  // draft on the recorded request didn't carry the defaults
  // explicitly (e.g. when we're driving codegen from a freshly
  // constructed record in a test or from the History dialog
  // pre-replay).
  const requestWithDefaults = {
    ...record.request,
    headers: mergeDefaultHeaders(record.request.headers, settings.defaultHeaders),
  };
  const result = buildFetchArgs(requestWithDefaults, {
    baseUrl: settings.baseUrl,
    variables: settings.variables,
    includeCredentials: settings.includeCredentials,
    globalAuth: settings.globalAuth,
  });
  if (!result.ok) return null;
  const init = result.args.init;
  const headers = (init.headers as Record<string, string>) ?? {};
  // FormData lands as `body` on `init`, but we don't want to dump
  // an opaque FormData object into a string snippet — fall back
  // to the original `formFields` list and let each generator
  // render multipart correctly.
  const body =
    typeof init.body === "string" && init.body.length > 0 ? init.body : null;
  return {
    method: (init.method ?? record.request.method).toUpperCase(),
    url: absolutize(result.args.url),
    headers,
    body,
    bodyType:
      record.request.method === "GET" || record.request.method === "HEAD"
        ? "none"
        : record.request.bodyType,
    formFields: record.request.formFields,
    includeCredentials: settings.includeCredentials,
  };
}

/** Merge workspace defaults into the per-request header list.
 *  Per-request entries with the same key (case-insensitive) win;
 *  defaults fill in missing slots. */
function mergeDefaultHeaders(
  requestHeaders: KVItem[],
  defaultHeaders: KVItem[],
): KVItem[] {
  const merged = defaultHeaders.map((h) => ({ ...h }));
  for (const header of requestHeaders) {
    const idx = merged.findIndex(
      (existing) => existing.key.toLowerCase() === header.key.toLowerCase(),
    );
    if (idx >= 0) merged[idx] = { ...header };
    else merged.push({ ...header });
  }
  return merged;
}

/** Promote a path-relative URL (`"/api/product/"`) to an absolute
 *  one when running in a browser context. Generators always want
 *  a full URL so the snippet works in someone else's terminal /
 *  REPL / project. */
function absolutize(url: string): string {
  if (/^[a-z][a-z\d+.-]*:/i.test(url)) return url;
  if (typeof window !== "undefined") {
    try {
      return new URL(url, window.location.origin).toString();
    } catch {
      // fall through
    }
  }
  return url;
}

/** Generate code in the requested language. */
export function generate(
  language: CodegenLanguage,
  snapshot: RequestSnapshot,
): string {
  switch (language) {
    case "js":
      return toJs(snapshot);
    case "curl":
      return toCurl(snapshot);
    case "python":
      return toPython(snapshot);
    case "rust":
      return toRust(snapshot);
  }
}

// =========================================================================
// JavaScript / TypeScript — fetch API
// =========================================================================

function toJs(s: RequestSnapshot): string {
  const lines: string[] = [];
  const headerEntries = Object.entries(s.headers);

  if (s.bodyType === "form" && hasFiles(s.formFields)) {
    lines.push("const formData = new FormData();");
    for (const f of s.formFields) {
      if (!f.enabled || !f.key) continue;
      if (f.type === "file") {
        lines.push(
          `// Replace with a File or Blob from your form input:`,
          `formData.append(${jsString(f.key)}, /* File */ ${jsString(
            f.fileName ?? "",
          )});`,
        );
      } else {
        lines.push(`formData.append(${jsString(f.key)}, ${jsString(f.value)});`);
      }
    }
    lines.push("");
  }

  lines.push(`const response = await fetch(${jsString(s.url)}, {`);
  lines.push(`  method: ${jsString(s.method)},`);
  if (headerEntries.length > 0) {
    // Don't emit Content-Type when using FormData — fetch sets
    // it automatically with the boundary. Mirrors the browser
    // behaviour.
    const filtered =
      s.bodyType === "form" && hasFiles(s.formFields)
        ? headerEntries.filter(([k]) => k.toLowerCase() !== "content-type")
        : headerEntries;
    if (filtered.length > 0) {
      lines.push(`  headers: {`);
      for (const [k, v] of filtered) {
        lines.push(`    ${jsString(k)}: ${jsString(v)},`);
      }
      lines.push(`  },`);
    }
  }
  if (s.bodyType === "form" && hasFiles(s.formFields)) {
    lines.push(`  body: formData,`);
  } else if (s.body !== null) {
    lines.push(`  body: ${jsString(s.body)},`);
  }
  if (s.includeCredentials) {
    lines.push(`  credentials: "include",`);
  }
  lines.push(`});`);
  lines.push("");
  lines.push(`if (!response.ok) {`);
  lines.push(`  throw new Error(\`HTTP \${response.status} \${response.statusText}\`);`);
  lines.push(`}`);
  lines.push(`const data = await response.json();`);
  lines.push(`console.log(data);`);
  return lines.join("\n");
}

function jsString(v: string): string {
  return JSON.stringify(v);
}

// =========================================================================
// cURL
// =========================================================================

function toCurl(s: RequestSnapshot): string {
  const parts: string[] = [`curl -X ${s.method}`];
  // Header order: explicit (workspace + per-request merged) first.
  const headerEntries = Object.entries(s.headers);
  const skipContentType =
    s.bodyType === "form" && hasFiles(s.formFields);
  for (const [k, v] of headerEntries) {
    if (skipContentType && k.toLowerCase() === "content-type") continue;
    parts.push(`-H ${shellSingleQuote(`${k}: ${v}`)}`);
  }
  if (s.includeCredentials) {
    // Caller would pass a cookie jar in a real terminal — we
    // hint at it without inventing one.
    parts.push(`# add: --cookie 'session=...' for credentialed requests`);
  }
  if (s.bodyType === "form" && hasFiles(s.formFields)) {
    for (const f of s.formFields) {
      if (!f.enabled || !f.key) continue;
      if (f.type === "file") {
        parts.push(`-F ${shellSingleQuote(`${f.key}=@${f.fileName || "FILE"}`)}`);
      } else {
        parts.push(`-F ${shellSingleQuote(`${f.key}=${f.value}`)}`);
      }
    }
  } else if (s.body !== null) {
    parts.push(`--data-raw ${shellSingleQuote(s.body)}`);
  }
  parts.push(shellSingleQuote(s.url));
  return parts.join(" \\\n  ");
}

function shellSingleQuote(value: string): string {
  // POSIX-safe: single-quote everything; inline `'` as `'\''`.
  return `'${value.replace(/'/g, "'\\''")}'`;
}

// =========================================================================
// Python — requests
// =========================================================================

function toPython(s: RequestSnapshot): string {
  const lines = ["import requests", ""];
  lines.push(`url = ${pyString(s.url)}`);

  const headerEntries = Object.entries(s.headers);
  const skipContentType =
    s.bodyType === "form" && hasFiles(s.formFields);
  if (headerEntries.length > 0) {
    lines.push("headers = {");
    for (const [k, v] of headerEntries) {
      if (skipContentType && k.toLowerCase() === "content-type") continue;
      lines.push(`    ${pyString(k)}: ${pyString(v)},`);
    }
    lines.push("}");
  } else {
    lines.push("headers = {}");
  }

  let bodyKwarg = "";
  if (s.bodyType === "form" && hasFiles(s.formFields)) {
    lines.push("files = {");
    const data: string[] = [];
    for (const f of s.formFields) {
      if (!f.enabled || !f.key) continue;
      if (f.type === "file") {
        lines.push(
          `    ${pyString(f.key)}: open(${pyString(f.fileName || "FILE")}, "rb"),`,
        );
      } else {
        data.push(`    ${pyString(f.key)}: ${pyString(f.value)},`);
      }
    }
    lines.push("}");
    if (data.length > 0) {
      lines.push("data = {");
      lines.push(...data);
      lines.push("}");
      bodyKwarg = ", files=files, data=data";
    } else {
      bodyKwarg = ", files=files";
    }
  } else if (s.body !== null) {
    if (s.bodyType === "json") {
      // requests will set Content-Type when `json=` is used; we
      // already include the explicit header, so use `data=` here
      // with the raw string to keep what the user typed verbatim.
      lines.push(`data = ${pyString(s.body)}`);
      bodyKwarg = ", data=data";
    } else {
      lines.push(`data = ${pyString(s.body)}`);
      bodyKwarg = ", data=data";
    }
  }

  const credsKwarg = s.includeCredentials ? ", cookies={...}" : "";
  lines.push(
    `response = requests.request(${pyString(s.method)}, url, headers=headers${bodyKwarg}${credsKwarg})`,
  );
  lines.push("response.raise_for_status()");
  lines.push("print(response.json())");
  return lines.join("\n");
}

function pyString(v: string): string {
  // Python repr is close enough to a safe literal for our shape;
  // JSON.stringify is portable and escapes the same characters
  // Python's repr does for ASCII strings.
  return JSON.stringify(v);
}

// =========================================================================
// Rust — reqwest blocking
// =========================================================================

function toRust(s: RequestSnapshot): string {
  const lines = [
    "use reqwest::blocking::Client;",
    "use reqwest::header::HeaderMap;",
    "",
    "fn main() -> Result<(), Box<dyn std::error::Error>> {",
    "    let client = Client::new();",
    "    let mut headers = HeaderMap::new();",
  ];

  const headerEntries = Object.entries(s.headers);
  const skipContentType =
    s.bodyType === "form" && hasFiles(s.formFields);
  for (const [k, v] of headerEntries) {
    if (skipContentType && k.toLowerCase() === "content-type") continue;
    lines.push(`    headers.insert(${rustString(k)}.parse()?, ${rustString(v)}.parse()?);`);
  }

  const methodFn = methodToReqwestFn(s.method);
  lines.push("");
  lines.push(`    let response = client`);
  lines.push(`        .${methodFn}(${rustString(s.url)})`);
  lines.push(`        .headers(headers)`);

  if (s.bodyType === "form" && hasFiles(s.formFields)) {
    lines.push(`        // multipart form — requires reqwest's "multipart" feature`);
    lines.push(`        .multipart({`);
    lines.push(`            let mut form = reqwest::blocking::multipart::Form::new();`);
    for (const f of s.formFields) {
      if (!f.enabled || !f.key) continue;
      if (f.type === "file") {
        lines.push(
          `            form = form.file(${rustString(f.key)}, ${rustString(f.fileName || "FILE")})?;`,
        );
      } else {
        lines.push(
          `            form = form.text(${rustString(f.key)}, ${rustString(f.value)});`,
        );
      }
    }
    lines.push(`            form`);
    lines.push(`        })`);
  } else if (s.body !== null) {
    lines.push(`        .body(${rustString(s.body)})`);
  }

  lines.push(`        .send()?;`);
  lines.push("");
  lines.push(`    let body = response.text()?;`);
  lines.push(`    println!("{}", body);`);
  lines.push(`    Ok(())`);
  lines.push(`}`);
  return lines.join("\n");
}

function methodToReqwestFn(method: string): string {
  switch (method.toUpperCase()) {
    case "GET":
      return "get";
    case "POST":
      return "post";
    case "PUT":
      return "put";
    case "PATCH":
      return "patch";
    case "DELETE":
      return "delete";
    case "HEAD":
      return "head";
    default:
      return `request(reqwest::Method::${method.toUpperCase()},`;
  }
}

function rustString(v: string): string {
  // Rust string literal: escape backslash + double-quote. Newlines
  // can stay as `\n` via JSON.stringify, since `\n` is a valid
  // escape in Rust string literals.
  return JSON.stringify(v);
}

// =========================================================================
// Shared helpers
// =========================================================================

function hasFiles(fields: KVItem[]): boolean {
  return fields.some((f) => f.enabled && f.type === "file");
}

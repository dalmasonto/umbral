import type { RequestDraft } from "./store";

/** Render a `curl` command equivalent to the given draft. */
export function toCurl(draft: RequestDraft): string {
  const parts: string[] = [`curl -X ${draft.method}`];
  for (const [k, v] of Object.entries(draft.headers)) {
    parts.push(`-H '${k}: ${v.replace(/'/g, "'\\''")}'`);
  }
  if (draft.bearerToken) {
    parts.push(`-H 'Authorization: Bearer ${draft.bearerToken}'`);
  }
  if (draft.body && draft.method !== "GET" && draft.method !== "HEAD") {
    parts.push(`--data '${draft.body.replace(/'/g, "'\\''")}'`);
  }
  parts.push(`'${draft.url}'`);
  return parts.join(" ");
}

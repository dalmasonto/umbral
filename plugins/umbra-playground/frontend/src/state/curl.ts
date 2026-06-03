import type { RequestDraft } from "./store";

/** Render a `curl` command equivalent to the given draft. */
export function toCurl(draft: RequestDraft): string {
  const parts: string[] = [`curl -X ${draft.method}`];
  for (const h of draft.headers) {
    if (!h.enabled || !h.key) continue;
    parts.push(`-H '${h.key}: ${h.value.replace(/'/g, "'\\''")}'`);
  }
  if (draft.authToken) {
    parts.push(`-H 'Authorization: ${draft.authScheme} ${draft.authToken}'`);
  }
  if (
    draft.bodyType === "json" &&
    draft.body &&
    draft.method !== "GET" &&
    draft.method !== "HEAD"
  ) {
    parts.push(`-d '${draft.body.replace(/'/g, "'\\''")}'`);
  } else if (
    draft.bodyType === "form" &&
    draft.formFields.length > 0 &&
    draft.method !== "GET" &&
    draft.method !== "HEAD"
  ) {
    const qs = draft.formFields
      .filter((f) => f.enabled && f.key)
      .map(
        ({ key, value }) => `${encodeURIComponent(key)}=${encodeURIComponent(value)}`,
      )
      .join("&");
    parts.push(`-d '${qs.replace(/'/g, "'\\''")}'`);
  }
  parts.push(`'${draft.url}'`);
  return parts.join(" \\\n  ");
}

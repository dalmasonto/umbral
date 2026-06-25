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

  if (draft.bodyType === "json" && draft.body) {
    if (draft.method !== "GET" && draft.method !== "HEAD") {
      parts.push(`-d '${draft.body.replace(/'/g, "'\\''")}'`);
    }
  } else if (draft.bodyType === "form") {
    const entries = draft.formFields.filter((f) => f.enabled && f.key);
    const hasFiles = entries.some((f) => f.type === "file");
    if (hasFiles) {
      for (const f of entries) {
        if (f.type === "file") {
          parts.push(`-F '${f.key}=@${f.fileName || "file"}'`);
        } else {
          parts.push(`-F '${f.key}=${f.value.replace(/'/g, "'\\''")}'`);
        }
      }
    } else {
      const qs = entries
        .map(
          ({ key, value }) =>
            `${encodeURIComponent(key)}=${encodeURIComponent(value)}`,
        )
        .join("&");
      if (qs) parts.push(`-d '${qs.replace(/'/g, "'\\''")}'`);
    }
  }

  parts.push(`'${draft.url}'`);
  return parts.join(" \\\n  ");
}

/** Per-operation request-draft persistence on Dexie.
 *
 *  Every endpoint (`operationId`) gets its own row in the
 *  `drafts` table. The user's in-progress URL, params, headers,
 *  body, form fields, and per-request auth survive a tab close /
 *  page reload without the user having to "save" anything
 *  explicitly.
 *
 *  All writes are fire-and-forget — the store schedules them via
 *  `scheduleDraftSave` and they resolve asynchronously without
 *  blocking the setter that triggered them. Failures are logged
 *  once and silently swallowed; the in-memory draft still drives
 *  the next request build, so a failed persist is at worst a
 *  missing rehydrate after reload. */

import { db, DRAFT_SCHEMA_VERSION } from "./db";
import type { RequestDraft } from "./store";

/** Best-effort fetch of a stored draft. Returns `null` for missing
 *  rows, schema-version mismatches we don't understand, and any
 *  Dexie / IndexedDB error. Never throws — the caller's "use
 *  defaults" path is the right fallback for every failure mode. */
export async function loadDraft(operationId: string): Promise<RequestDraft | null> {
  try {
    const row = await db.drafts.get(operationId);
    if (!row) return null;
    if (row.schema > DRAFT_SCHEMA_VERSION) return null;
    return row.draft;
  } catch (e) {
    if (typeof console !== "undefined") {
      console.warn("[umbral-playground] draft read failed", e);
    }
    return null;
  }
}

/** Write the latest draft for `operationId`. Fire-and-forget —
 *  the caller doesn't await this, so a slow IndexedDB write
 *  can't block keystrokes. */
export async function saveDraft(
  operationId: string,
  draft: RequestDraft,
): Promise<void> {
  try {
    await db.drafts.put({
      operationId,
      schema: DRAFT_SCHEMA_VERSION,
      draft,
      updatedAt: Date.now(),
    });
  } catch (e) {
    if (typeof console !== "undefined") {
      console.warn("[umbral-playground] draft save failed", e);
    }
  }
}

/** Remove the stored draft for an operation. Used when the user
 *  explicitly resets the request — the persisted state shouldn't
 *  outlive the in-memory one. */
export async function deleteDraft(operationId: string): Promise<void> {
  try {
    await db.drafts.delete(operationId);
  } catch {
    // ignore
  }
}

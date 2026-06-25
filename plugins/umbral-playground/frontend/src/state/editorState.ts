import { db } from "./db";

/** Best-effort fetch of a persisted UI slot. Returns `fallback` on
 *  any read failure (slot missing, Dexie unavailable, IndexedDB
 *  blocked). Never throws — the UI must keep working even when the
 *  storage layer is hosed. */
export async function loadEditorState<T>(
  key: string,
  fallback: T,
): Promise<T> {
  try {
    const row = await db.editorState.get(key);
    if (!row) return fallback;
    return row.value as T;
  } catch {
    return fallback;
  }
}

/** Persist a UI slot. Swallows write errors silently — see
 *  `loadEditorState` for the rationale. */
export async function saveEditorState(
  key: string,
  value: unknown,
): Promise<void> {
  try {
    await db.editorState.put({ key, value });
  } catch {
    // IndexedDB quota / closed / disabled — drop silently.
  }
}

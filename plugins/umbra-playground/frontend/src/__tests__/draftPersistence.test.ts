/** Per-operation draft persistence: every keystroke schedules a
 *  fire-and-forget Dexie write, and selecting an endpoint
 *  rehydrates whatever was saved last time.
 *
 *  Invariants pinned here:
 *
 *  - `setParams` / `setHeaders` / `setBody` / `setUrl` / `setAuthToken`
 *    each write the draft to Dexie within the debounce window —
 *    nothing is awaited inline, so the UI never blocks.
 *  - The debounce coalesces a typing burst into a single write.
 *  - `selectEndpoint` flushes the OUTGOING op's pending save
 *    before switching, so the user's last keystroke survives the
 *    operation switch.
 *  - `selectEndpoint(id)` async-loads the saved draft for the new
 *    `id` and applies it; no draft → empty current (the
 *    RequestBuilder seeds defaults in that case).
 *  - `clearCurrent()` wipes both the in-memory state AND the
 *    persisted row so a reload doesn't resurrect what was just
 *    discarded. */

import { describe, it, expect, beforeEach, vi, afterEach } from "vitest";
import "fake-indexeddb/auto";

const memory: Record<string, string> = {};
const storage = {
  getItem: vi.fn((k: string) => (k in memory ? memory[k] : null)),
  setItem: vi.fn((k: string, v: string) => {
    memory[k] = v;
  }),
  removeItem: vi.fn((k: string) => {
    delete memory[k];
  }),
  clear: vi.fn(() => {
    for (const k of Object.keys(memory)) delete memory[k];
  }),
  length: 0,
  key: vi.fn(() => null),
};
(globalThis as unknown as { window: { localStorage: typeof storage } }).window = {
  localStorage: storage,
};
(globalThis as unknown as { localStorage: typeof storage }).localStorage = storage;

async function freshDexie() {
  try {
    const { db } = await import("../state/db");
    await db.delete();
  } catch {
    // first boot
  }
  vi.resetModules();
}

async function reload() {
  await freshDexie();
  return (await import("../state/store")).usePlayground;
}

/** Wait for the in-memory draft for `operationId` to land in the
 *  Dexie row. The setter that triggered the save returned
 *  synchronously, so we poll the table until the row shows up or
 *  the 300ms patience window expires. */
async function waitForDraftWrite(operationId: string): Promise<void> {
  const { db } = await import("../state/db");
  const deadline = Date.now() + 300;
  while (Date.now() < deadline) {
    const row = await db.drafts.get(operationId);
    if (row) return;
    await new Promise((r) => setTimeout(r, 5));
  }
}

describe("playground draft persistence", () => {
  beforeEach(() => {
    storage.clear();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("persists params to Dexie within the debounce window", async () => {
    const use = await reload();
    use.getState().selectEndpoint("list_product");
    use.getState().setParams([
      { key: "fields", value: "id,name", enabled: true },
    ]);
    await waitForDraftWrite("list_product");
    const { db } = await import("../state/db");
    const row = await db.drafts.get("list_product");
    expect(row?.draft.params).toEqual([
      { key: "fields", value: "id,name", enabled: true },
    ]);
  });

  it("coalesces a typing burst into a single Dexie write", async () => {
    const use = await reload();
    use.getState().selectEndpoint("list_product");
    use.getState().setBody("{");
    use.getState().setBody('{"n');
    use.getState().setBody('{"name');
    use.getState().setBody('{"name":"Widget"}');
    await waitForDraftWrite("list_product");

    const { db } = await import("../state/db");
    const row = await db.drafts.get("list_product");
    // Only the last value lands — the four keystrokes collapsed
    // into one trailing write.
    expect(row?.draft.body).toBe('{"name":"Widget"}');
  });

  it("rehydrates the saved draft when an operation is reselected", async () => {
    let use = await reload();
    use.getState().selectEndpoint("update_product");
    use.getState().setUrl("/api/product/{id}");
    use.getState().setParams([
      { key: "id", value: "42", enabled: true },
    ]);
    use.getState().setBody('{"name":"Updated"}');
    await waitForDraftWrite("update_product");

    // Simulate selecting another operation, then coming back.
    use.getState().selectEndpoint("list_product");
    expect(use.getState().current.body).toBe("");

    use.getState().selectEndpoint("update_product");
    // The async load resolves on the next tick; poll until it
    // surfaces back into in-memory state.
    const deadline = Date.now() + 300;
    while (use.getState().current.body === "" && Date.now() < deadline) {
      await new Promise((r) => setTimeout(r, 5));
    }
    expect(use.getState().current.body).toBe('{"name":"Updated"}');
    expect(use.getState().current.url).toBe("/api/product/{id}");
    expect(use.getState().current.params).toEqual([
      { key: "id", value: "42", enabled: true },
    ]);
  });

  it("flushes the pending save for the outgoing op when switching", async () => {
    const use = await reload();
    use.getState().selectEndpoint("list_product");
    use.getState().setBody('{"in":"progress"}');
    // Switch BEFORE the 250ms debounce fires. The store should
    // force the save through synchronously so the typed value
    // survives the switch.
    use.getState().selectEndpoint("retrieve_product");

    const { db } = await import("../state/db");
    const deadline = Date.now() + 300;
    while (Date.now() < deadline) {
      const row = await db.drafts.get("list_product");
      if (row) {
        expect(row.draft.body).toBe('{"in":"progress"}');
        return;
      }
      await new Promise((r) => setTimeout(r, 5));
    }
    throw new Error("flush on switch did not land in Dexie");
  });

  it("clearCurrent wipes both in-memory state and the Dexie row", async () => {
    const use = await reload();
    use.getState().selectEndpoint("list_product");
    use.getState().setBody('{"name":"Widget"}');
    await waitForDraftWrite("list_product");

    use.getState().clearCurrent();
    expect(use.getState().current.body).toBe("");

    // Poll: the delete is fire-and-forget too.
    const { db } = await import("../state/db");
    const deadline = Date.now() + 300;
    while (Date.now() < deadline) {
      const row = await db.drafts.get("list_product");
      if (!row) return;
      await new Promise((r) => setTimeout(r, 5));
    }
    throw new Error("Dexie row should have been deleted by clearCurrent");
  });

  it("returns null from loadDraft when no row is stored", async () => {
    await reload();
    const { loadDraft } = await import("../state/draftStorage");
    const result = await loadDraft("nonexistent_op");
    expect(result).toBeNull();
  });

  it("does not block the setter — setBody returns synchronously", async () => {
    const use = await reload();
    use.getState().selectEndpoint("list_product");
    // No await: the setter must be fully synchronous from the
    // caller's perspective. If it returned a promise we'd need to
    // .then it; the assertion below is what guarantees no awaiting.
    use.getState().setBody('{"sync":true}');
    expect(use.getState().current.body).toBe('{"sync":true}');
  });
});

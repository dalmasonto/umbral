/** Verifies the gap #76 save path on Dexie / IndexedDB:
 *
 *  - In-memory edits land in IndexedDB via the `settings` table.
 *  - `saveStatus` flips to "saved" on a confirmed Dexie write,
 *    "dirty" on failure.
 *  - The localStorage boot cache is updated as a side-effect so
 *    the next page load can render synchronously.
 *  - Round-trip across a "reload" (module reset + re-import) reads
 *    the persisted settings back. */

import { describe, it, expect, beforeEach, vi, afterEach } from "vitest";

// Install fake-indexeddb BEFORE importing Dexie. The auto-import
// shape registers the polyfill on globalThis.indexedDB so Dexie
// finds it the first time it tries to open a database.
import "fake-indexeddb/auto";

// 2. Install a synchronous localStorage shim. The boot cache is
//    sync-read at module load, so it must be present before the
//    store module imports.
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
  // Delete the existing per-app Dexie DB before resetting modules
  // so the *next* import of db.ts opens a fresh Dexie instance
  // against an empty IndexedDB. Calling `.delete()` closes the
  // existing instance — that's why we must reset modules AFTER,
  // not before; the closed instance must not survive into the
  // next test.
  try {
    const { db } = await import("../state/db");
    await db.delete();
  } catch {
    // First boot, or db.ts not in module cache yet — nothing to delete.
  }
  vi.resetModules();
}

async function reload() {
  await freshDexie();
  return (await import("../state/store")).usePlayground;
}

/** Async test helper — `setSettings` schedules the save as a
 *  microtask AND awaits Dexie (which on fake-indexeddb is a few
 *  macrotask hops). One tick isn't always enough; this loops
 *  until either the predicate fires or we run out of patience. */
async function tick(times = 4) {
  for (let i = 0; i < times; i++) {
    await new Promise((resolve) => setTimeout(resolve, 0));
  }
}

/** Wait until `storage.setItem` has been called at least `n`
 *  times, or 200ms pass. Lets the round-trip tests assert on the
 *  cache write without flaking on Dexie's async overhead. */
async function waitForSetItemCalls(n: number) {
  const deadline = Date.now() + 200;
  while (
    (globalThis as unknown as { window: { localStorage: typeof storage } })
      .window.localStorage.setItem.mock.calls.length < n
  ) {
    if (Date.now() > deadline) return;
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
}

describe("playground settings persistence (Dexie + boot cache)", () => {
  beforeEach(() => {
    storage.clear();
    storage.setItem.mockClear();
    storage.getItem.mockClear();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("persists baseUrl into the Dexie settings table", async () => {
    const use = await reload();
    use.getState().setBaseUrl("https://api.example.com");
    await tick();
    await tick();
    const { db } = await import("../state/db");
    const row = await db.settings.get("workspace");
    expect(row?.value.baseUrl).toBe("https://api.example.com");
  });

  it("mirrors writes into the localStorage boot cache too", async () => {
    const use = await reload();
    use.getState().setBaseUrl("https://api.example.com");
    await tick();
    await tick();
    // Last write should be the per-app scoped key.
    const lastCall = storage.setItem.mock.calls.at(-1);
    expect(lastCall).toBeDefined();
    const [key, raw] = lastCall!;
    expect(key).toMatch(/umbral-playground-settings:v1$/);
    expect(JSON.parse(raw).baseUrl).toBe("https://api.example.com");
  });

  it("flips saveStatus to 'saved' after a successful write", async () => {
    const use = await reload();
    use.getState().setBaseUrl("https://api.example.com");
    expect(use.getState().saveStatus).toBe("saving");
    await tick();
    await tick();
    expect(use.getState().saveStatus).toBe("saved");
    expect(use.getState().lastSavedAt).not.toBeNull();
  });

  it("flips saveStatus to 'dirty' when Dexie write throws", async () => {
    const use = await reload();
    const { db } = await import("../state/db");
    const original = db.settings.put.bind(db.settings);
    db.settings.put = vi.fn(() => {
      throw new Error("simulated quota");
    }) as unknown as typeof db.settings.put;
    use.getState().setBaseUrl("oops");
    await tick();
    await tick();
    expect(use.getState().saveStatus).toBe("dirty");
    // In-memory state still reflects the attempt.
    expect(use.getState().settings.baseUrl).toBe("oops");
    db.settings.put = original;
  });

  it("globalAuth round-trips across a reload via Dexie", async () => {
    let use = await reload();
    use.getState().setGlobalAuth({
      enabled: true,
      scheme: "Token",
      token: "abc123",
    });
    await waitForSetItemCalls(1);
    // Sanity: the boot cache also has the new value. (Dexie holds
    // the source of truth; the LS cache mirrors it for fast boot.)
    const lastWrite = storage.setItem.mock.calls.at(-1);
    expect(lastWrite, "boot cache write should have landed").toBeDefined();
    expect(JSON.parse(lastWrite![1]).globalAuth.token).toBe("abc123");

    // Reload — drops the in-memory store. Dexie persists across
    // the reload because fake-indexeddb is process-wide unless we
    // call db.delete().
    use = (await import("../state/store")).usePlayground;
    vi.resetModules();
    use = (await import("../state/store")).usePlayground;
    // Hydrate from Dexie explicitly (mirrors what App.tsx does on
    // mount).
    await use.getState().hydrateFromDexie();
    expect(use.getState().settings.globalAuth).toEqual({
      enabled: true,
      scheme: "Token",
      token: "abc123",
    });
  });

  it("variables survive across a reload via Dexie", async () => {
    let use = await reload();
    use.getState().setVariables([
      { key: "api_key", value: "secret-value", enabled: true, type: "text" },
    ]);
    await tick();
    await tick();

    vi.resetModules();
    use = (await import("../state/store")).usePlayground;
    await use.getState().hydrateFromDexie();
    expect(use.getState().settings.variables).toEqual([
      { key: "api_key", value: "secret-value", enabled: true, type: "text" },
    ]);
  });

  it("saveSettingsNow flushes immediately and returns true on success", async () => {
    const use = await reload();
    use.getState().setBaseUrl("https://x.test");
    await tick();
    await tick();
    const ok = await use.getState().saveSettingsNow();
    expect(ok).toBe(true);
    expect(use.getState().saveStatus).toBe("saved");
  });

  it("saveSettingsNow returns false when Dexie throws", async () => {
    const use = await reload();
    const { db } = await import("../state/db");
    const original = db.settings.put.bind(db.settings);
    db.settings.put = vi.fn(() => {
      throw new Error("disabled");
    }) as unknown as typeof db.settings.put;
    const ok = await use.getState().saveSettingsNow();
    expect(ok).toBe(false);
    expect(use.getState().saveStatus).toBe("dirty");
    db.settings.put = original;
  });

  it("hydrateFromDexie migrates a localStorage-only row into Dexie", async () => {
    // Fresh empty Dexie so the migration path actually fires —
    // it only seeds Dexie from localStorage when the settings
    // table is empty.
    await freshDexie();

    // Pre-seed the boot cache as if an older build had written
    // there before the Dexie migration shipped.
    storage.setItem(
      "default:umbral-playground-settings:v1",
      JSON.stringify({
        baseUrl: "https://migrated.example",
        variables: [
          { key: "foo", value: "bar", enabled: true, type: "text" },
        ],
        defaultHeaders: [],
        includeCredentials: true,
        globalAuth: { enabled: false, scheme: "Bearer", token: "" },
      }),
    );

    // Sanity: storage actually holds the seed before we boot the
    // store. Catches mock-helper regressions.
    expect(JSON.parse(memory["default:umbral-playground-settings:v1"]).baseUrl).toBe(
      "https://migrated.example",
    );

    // freshDexie already ran vi.resetModules() — just import.
    const use = (await import("../state/store")).usePlayground;
    await use.getState().hydrateFromDexie();
    expect(use.getState().settings.baseUrl).toBe("https://migrated.example");

    // Dexie should now hold the migrated row too — the next reload
    // won't need the localStorage cache.
    const { db } = await import("../state/db");
    const row = await db.settings.get("workspace");
    expect(row?.value.baseUrl).toBe("https://migrated.example");
  });

  it("auto-save (setSettings) is silent — no toast, but the badge updates", async () => {
    const use = await reload();
    const { useToastStore } = await import("../state/toastStore");
    const before = useToastStore.getState().toasts.length;
    use.getState().setBaseUrl("https://silent.test");
    await tick();
    await tick();
    // No new toast was pushed — auto-save is silent. The
    // persistent SaveStatusIndicator in the header is the
    // only feedback (verified via saveStatus flipping to
    // "saved" below).
    expect(useToastStore.getState().toasts.length).toBe(before);
    expect(use.getState().saveStatus).toBe("saved");
  });

  it("manual save (saveSettingsNow) pushes a toast", async () => {
    const use = await reload();
    const { useToastStore } = await import("../state/toastStore");
    const before = useToastStore.getState().toasts.length;
    await use.getState().saveSettingsNow();
    expect(useToastStore.getState().toasts.length).toBe(before + 1);
    const last = useToastStore.getState().toasts.at(-1);
    expect(last?.message).toBe("Settings saved");
  });
});

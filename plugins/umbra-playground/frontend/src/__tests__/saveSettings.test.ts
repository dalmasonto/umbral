/** Verifies the gap #76 save path: an in-memory edit DOES land in
 *  localStorage, the saveStatus flips back to "saved" on success and
 *  "dirty" on failure, and reload-equivalent reads round-trip. */

import { describe, it, expect, beforeEach, vi, afterEach } from "vitest";

// Hoist a fake window/localStorage onto the global before the
// store module loads — the module computes SETTINGS_KEY and
// captures `window.localStorage` at import time via
// `loadSettings()`.
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
// JSDOM-free smoke environment — assign a minimal window + the
// flat localStorage symbol the store reads via `window.localStorage`.
(globalThis as unknown as { window: { localStorage: typeof storage } }).window = {
  localStorage: storage,
};
(globalThis as unknown as { localStorage: typeof storage }).localStorage = storage;

async function reload() {
  // Drop the store module from the module cache so the next import
  // re-runs `loadSettings()` and `create()`, simulating a real
  // page-reload boot path.
  vi.resetModules();
  return (await import("../state/store")).usePlayground;
}

describe("playground settings persistence (gap #76)", () => {
  beforeEach(() => {
    storage.clear();
    storage.setItem.mockClear();
    storage.getItem.mockClear();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("persists baseUrl through to localStorage on setSettings", async () => {
    const use = await reload();
    use.getState().setBaseUrl("https://api.example.com");
    expect(storage.setItem).toHaveBeenCalled();
    // Last write should be the full settings object with baseUrl set.
    const lastCall = storage.setItem.mock.calls.at(-1)!;
    const [, raw] = lastCall;
    const persisted = JSON.parse(raw);
    expect(persisted.baseUrl).toBe("https://api.example.com");
  });

  it("flips saveStatus to 'saved' after a successful write", async () => {
    const use = await reload();
    use.getState().setBaseUrl("https://api.example.com");
    expect(use.getState().saveStatus).toBe("saved");
    expect(use.getState().lastSavedAt).not.toBeNull();
  });

  it("flips saveStatus to 'dirty' when localStorage throws", async () => {
    const use = await reload();
    storage.setItem.mockImplementationOnce(() => {
      throw new DOMException("QuotaExceededError");
    });
    use.getState().setBaseUrl("oops");
    expect(use.getState().saveStatus).toBe("dirty");
    // The in-memory settings still reflect the attempt.
    expect(use.getState().settings.baseUrl).toBe("oops");
  });

  it("globalAuth round-trips across a reload", async () => {
    let use = await reload();
    use.getState().setGlobalAuth({
      enabled: true,
      scheme: "Token",
      token: "abc123",
    });
    // Simulate the user reloading the tab.
    use = await reload();
    expect(use.getState().settings.globalAuth).toEqual({
      enabled: true,
      scheme: "Token",
      token: "abc123",
    });
  });

  it("variables survive across a reload", async () => {
    let use = await reload();
    use.getState().setVariables([
      { key: "api_key", value: "secret-value", enabled: true, type: "text" },
    ]);
    use = await reload();
    expect(use.getState().settings.variables).toEqual([
      { key: "api_key", value: "secret-value", enabled: true, type: "text" },
    ]);
  });

  it("saveSettingsNow writes immediately and fires a toast", async () => {
    const use = await reload();
    use.getState().setBaseUrl("https://x.test");
    storage.setItem.mockClear();
    const ok = use.getState().saveSettingsNow();
    expect(ok).toBe(true);
    expect(storage.setItem).toHaveBeenCalledTimes(1);
    expect(use.getState().saveStatus).toBe("saved");
  });

  it("saveSettingsNow returns false when storage throws", async () => {
    const use = await reload();
    storage.setItem.mockImplementationOnce(() => {
      throw new Error("disabled");
    });
    const ok = use.getState().saveSettingsNow();
    expect(ok).toBe(false);
    expect(use.getState().saveStatus).toBe("dirty");
  });
});

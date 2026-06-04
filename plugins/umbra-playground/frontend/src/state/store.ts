import { create } from "zustand";
import type { OpenAPIV3 } from "openapi-types";
import { buildFetchArgs } from "./buildFetchArgs";
import { saveHistoryDebounced } from "./history";
import { mockSpec, getMockResponse } from "../data/mockSpec";
import { scopedKey } from "./scope";
import { deleteDraft, loadDraft, saveDraft } from "./draftStorage";
import {
  hydrateInitialSettings,
  persistSettings,
  readLocalStorageCache,
  writeLocalStorageCache,
} from "./settingsStorage";
import { pushToast } from "./toastStore";

export interface KVItem {
  key: string;
  value: string;
  enabled: boolean;
  type?: "text" | "file";
  fileName?: string;
}

/** A request as the user has constructed it in the builder. */
export interface RequestDraft {
  method: string;
  url: string;
  params: KVItem[];
  headers: KVItem[];
  bodyType: "json" | "form";
  body: string;
  formFields: KVItem[];
  authScheme: string;
  authToken: string;
}

export interface GlobalAuthSettings {
  /** Master toggle so users can stash a token without it leaking
   *  into every request. */
  enabled: boolean;
  /** Scheme prefix — `Bearer`, `Token`, `Basic`, anything custom. */
  scheme: string;
  /** Token value. Variable interpolation works inside it. */
  token: string;
}

export interface PlaygroundSettings {
  baseUrl: string;
  variables: KVItem[];
  defaultHeaders: KVItem[];
  includeCredentials: boolean;
  /** Workspace-wide Authorization fallback (gap #75). Per-request
   *  auth on the builder still wins; this only fires when the
   *  request didn't set its own. */
  globalAuth: GlobalAuthSettings;
}

/** A completed request/response pair, persisted in history. */
export interface ResponseRecord {
  operationId: string;
  request: RequestDraft;
  status: number;
  statusText: string;
  durationMs: number;
  sizeBytes: number;
  headers: Record<string, string>;
  bodyText: string;
  timestamp: number;
  error?: string;
}

// Per-app storage keys (gap #71). The bare strings used to be
// shared across every umbra app served to the same browser; now
// each app's localStorage slot is `<app>:<key>`. The full
// `umbra-playground-settings:v1` slot moved to Dexie's `settings`
// table — `settingsStorage.ts` owns that path now; the slot below
// is only the selected-endpoint pointer, which is a single short
// string that the next page load still needs synchronously.
const SELECTED_KEY = scopedKey("umbra-playground:selected-operation:v1");

/** Persist the currently-selected operationId so reloads land back on
 *  the same endpoint. Sync localStorage write is fine here — this is
 *  a single short string that the next page load needs synchronously
 *  before zustand initialises. */
function loadSelectedOperationId(): string | null {
  if (typeof window === "undefined") return null;
  try {
    return window.localStorage.getItem(SELECTED_KEY);
  } catch {
    return null;
  }
}

function saveSelectedOperationId(id: string | null) {
  if (typeof window === "undefined") return;
  try {
    if (id) {
      window.localStorage.setItem(SELECTED_KEY, id);
    } else {
      window.localStorage.removeItem(SELECTED_KEY);
    }
  } catch {
    // Storage full / disabled — silently drop.
  }
}

const factoryDefaultHeaders: KVItem[] = [
  { key: "Content-Type", value: "application/json", enabled: true },
  { key: "Accept", value: "application/json", enabled: true },
];

const baseGlobalAuth: GlobalAuthSettings = {
  enabled: false,
  scheme: "Bearer",
  token: "",
};

const baseSettings: PlaygroundSettings = {
  baseUrl: "",
  variables: [],
  defaultHeaders: factoryDefaultHeaders.map((h) => ({ ...h })),
  includeCredentials: true,
  globalAuth: { ...baseGlobalAuth },
};

const emptyDraft: RequestDraft = {
  method: "GET",
  url: "",
  params: [],
  headers: [],
  bodyType: "json",
  body: "",
  formFields: [],
  authScheme: "Bearer",
  authToken: "",
};

function cloneRows(rows: KVItem[]): KVItem[] {
  return rows.map((row) => ({ ...row }));
}

function normalizeRows(rows: unknown): KVItem[] {
  if (!Array.isArray(rows)) return [];
  return rows
    .filter((row): row is Partial<KVItem> => row !== null && typeof row === "object")
    .map((row) => ({
      key: typeof row.key === "string" ? row.key : "",
      value: typeof row.value === "string" ? row.value : "",
      enabled: row.enabled !== false,
      type: row.type === "file" ? "file" : "text",
      fileName: typeof row.fileName === "string" ? row.fileName : undefined,
    }));
}

function normalizeGlobalAuth(
  raw: Partial<GlobalAuthSettings> | undefined,
): GlobalAuthSettings {
  if (!raw || typeof raw !== "object") return { ...baseGlobalAuth };
  return {
    enabled: raw.enabled === true,
    scheme: typeof raw.scheme === "string" && raw.scheme ? raw.scheme : "Bearer",
    token: typeof raw.token === "string" ? raw.token : "",
  };
}

function normalizeSettings(settings: Partial<PlaygroundSettings>): PlaygroundSettings {
  const defaultHeaders = normalizeRows(settings.defaultHeaders);
  return {
    baseUrl: typeof settings.baseUrl === "string" ? settings.baseUrl : "",
    variables: normalizeRows(settings.variables),
    defaultHeaders:
      defaultHeaders.length > 0
        ? defaultHeaders
        : cloneRows(factoryDefaultHeaders),
    includeCredentials: settings.includeCredentials !== false,
    globalAuth: normalizeGlobalAuth(settings.globalAuth),
  };
}

/** Sync read for the initial store state. Hits the localStorage
 *  boot cache that mirrors Dexie — fast, no flash, but possibly
 *  stale if another tab wrote to Dexie since this tab last rendered.
 *  The store fires `hydrateFromDexie` after mount to settle that. */
function loadSettings(): PlaygroundSettings {
  const cached = readLocalStorageCache();
  return normalizeSettings(cached ?? baseSettings);
}

/** Authoritative settings save. Writes Dexie first, then mirrors
 *  to the localStorage boot cache, then returns whether the Dexie
 *  write actually landed. The store branches `saveStatus` on the
 *  return value so the UI tells the truth instead of always
 *  claiming "saved." */
async function saveSettings(settings: PlaygroundSettings): Promise<boolean> {
  return persistSettings(settings);
}

function mergeHeaders(
  requestHeaders: KVItem[],
  defaultHeaders: KVItem[],
): KVItem[] {
  const merged = cloneRows(defaultHeaders);
  for (const header of requestHeaders) {
    const idx = merged.findIndex(
      (existing) => existing.key.toLowerCase() === header.key.toLowerCase(),
    );
    if (idx >= 0) {
      merged[idx] = { ...header };
    } else {
      merged.push({ ...header });
    }
  }
  return merged;
}

interface PlaygroundState {
  // spec
  spec: OpenAPIV3.Document | null;
  specError: string | null;
  loadingSpec: boolean;
  loadSpec: () => Promise<void>;

  // selection
  selectedOperationId: string | null;
  selectEndpoint: (id: string | null) => void;

  // current request
  current: RequestDraft;
  setMethod: (m: string) => void;
  setUrl: (u: string) => void;
  setParams: (params: KVItem[]) => void;
  setHeaders: (headers: KVItem[]) => void;
  setBodyType: (t: "json" | "form") => void;
  setBody: (raw: string) => void;
  setFormFields: (fields: KVItem[]) => void;
  setAuthScheme: (s: string) => void;
  setAuthToken: (t: string) => void;
  resetCurrent: (draft: Partial<RequestDraft>) => void;
  /** Discard the in-memory draft AND the persisted Dexie row for
   *  the current operation. Reload won't bring the cleared draft
   *  back. */
  clearCurrent: () => void;

  // response
  lastResponse: ResponseRecord | null;
  inFlight: boolean;
  send: () => Promise<void>;

  // history
  history: Record<string, ResponseRecord[]>;
  clearHistory: (operationId: string) => void;

  // workspace settings
  settings: PlaygroundSettings;
  /** Visible save state — populates the badge + Save-button label
   *  in the settings sheet. "saving" is the in-flight burst window;
   *  "saved" is the resting state immediately after a successful
   *  write; "dirty" appears only when the most recent write failed
   *  and the in-memory state is out of sync with localStorage. */
  saveStatus: "saved" | "saving" | "dirty";
  /** Timestamp of the last successful save, for the "saved 3s ago"
   *  text. `null` if nothing has ever been saved this session. */
  lastSavedAt: number | null;
  setSettings: (settings: Partial<PlaygroundSettings>) => void;
  setBaseUrl: (baseUrl: string) => void;
  setVariables: (variables: KVItem[]) => void;
  setDefaultHeaders: (headers: KVItem[]) => void;
  setIncludeCredentials: (enabled: boolean) => void;
  setGlobalAuth: (patch: Partial<GlobalAuthSettings>) => void;
  applyDefaultHeaders: () => void;
  resetSettings: () => void;
  /** Manual flush — useful both as a "save retry" after an
   *  in-memory edit hit a transient storage error AND as the click
   *  handler for the visible Save button. Resolves with the success
   *  bit so the caller can fire its own toast. */
  saveSettingsNow: () => Promise<boolean>;
  /** Pull the authoritative settings out of Dexie once after mount.
   *  No-op when Dexie matches the localStorage cache; replaces the
   *  in-memory state when another tab (or a stale cache) caused a
   *  divergence. Idempotent — safe to call on every mount. */
  hydrateFromDexie: () => Promise<void>;
}

const initialSettings = loadSettings();

// Per-operation request-draft persistence. setMethod / setUrl /
// setParams / setHeaders / setBody / setFormFields / setAuth* all
// fire-and-forget through `scheduleDraftSave` so the UI stays
// responsive. The 250ms debounce coalesces a typing burst into a
// single Dexie write. Module-scoped state because zustand isn't
// the right place for transient timer handles.
let draftSaveTimer: ReturnType<typeof setTimeout> | null = null;
let draftSavePending: { operationId: string; draft: RequestDraft } | null = null;

function scheduleDraftSave(operationId: string | null, draft: RequestDraft) {
  if (!operationId) return;
  draftSavePending = { operationId, draft };
  if (draftSaveTimer) return; // already scheduled
  draftSaveTimer = setTimeout(() => {
    draftSaveTimer = null;
    const snapshot = draftSavePending;
    draftSavePending = null;
    if (snapshot) {
      // Fire-and-forget on purpose — the setter that scheduled
      // this returned synchronously, so awaiting here would not
      // block the UI but would also not be observable. The
      // caller doesn't need a confirmation.
      void saveDraft(snapshot.operationId, snapshot.draft);
    }
  }, 250);
}

// Debounced "Settings saved" toast (gap #76). setSettings fires on
// every keystroke; the debounce coalesces a typing burst into a
// single confirmation. Lives at module scope so a global reset can
// clear it without exposing it on the store.
let saveToastTimer: ReturnType<typeof setTimeout> | null = null;
function scheduleSaveToast(success: boolean) {
  if (saveToastTimer) clearTimeout(saveToastTimer);
  saveToastTimer = setTimeout(() => {
    pushToast(
      success
        ? { kind: "success", message: "Settings saved" }
        : {
            kind: "error",
            message:
              "Couldn't save settings — your browser may be blocking localStorage (private mode? quota full?).",
            durationMs: 5000,
          },
    );
    saveToastTimer = null;
  }, 600);
}

export const usePlayground = create<PlaygroundState>((set, get) => ({
  spec: null,
  specError: null,
  loadingSpec: false,

  loadSpec: async () => {
    set({ loadingSpec: true, specError: null });
    try {
      const res = await fetch("/openapi/openapi.json");
      if (!res.ok) {
        throw new Error(`HTTP ${res.status} fetching spec`);
      }
      const spec = (await res.json()) as OpenAPIV3.Document;
      set({ spec, loadingSpec: false });
    } catch {
      set({ spec: mockSpec, loadingSpec: false, specError: null });
    }
  },

  selectedOperationId: loadSelectedOperationId(),
  selectEndpoint: (id) => {
    saveSelectedOperationId(id);
    // Flush the pending draft save for the OUTGOING op so the
    // user's last keystroke before switching doesn't get lost in
    // the debounce window.
    if (draftSaveTimer && draftSavePending) {
      clearTimeout(draftSaveTimer);
      const snapshot = draftSavePending;
      draftSavePending = null;
      draftSaveTimer = null;
      void saveDraft(snapshot.operationId, snapshot.draft);
    }
    // Reset to an empty draft synchronously so the request
    // builder doesn't briefly show the previous operation's
    // values. The async draftStorage.loadDraft below either
    // hydrates the saved draft for the new op or leaves the
    // empty state in place (in which case RequestBuilder's
    // effect fills in the operation defaults).
    set({
      selectedOperationId: id,
      lastResponse: null,
      current: { ...emptyDraft },
    });
    if (!id) return;
    // Fire-and-forget — the load can complete on the next tick
    // and `set` it then. Subscribers re-render naturally.
    void loadDraft(id).then((saved) => {
      if (!saved) return;
      // Only apply if the user hasn't already navigated away,
      // and only if they haven't started typing into the empty
      // draft we set synchronously above (i.e. their typed
      // values would otherwise be clobbered).
      const cur = get();
      if (cur.selectedOperationId !== id) return;
      if (cur.current.url !== "" || cur.current.body !== "") return;
      set({ current: saved });
    });
  },

  current: { ...emptyDraft },
  setMethod: (m) =>
    set((s) => {
      const next = { ...s.current, method: m };
      scheduleDraftSave(s.selectedOperationId, next);
      return { current: next };
    }),
  setUrl: (u) =>
    set((s) => {
      const next = { ...s.current, url: u };
      scheduleDraftSave(s.selectedOperationId, next);
      return { current: next };
    }),
  setParams: (params) =>
    set((s) => {
      const next = { ...s.current, params };
      scheduleDraftSave(s.selectedOperationId, next);
      return { current: next };
    }),
  setHeaders: (headers) =>
    set((s) => {
      const next = { ...s.current, headers };
      scheduleDraftSave(s.selectedOperationId, next);
      return { current: next };
    }),
  setBodyType: (t) =>
    set((s) => {
      const next = { ...s.current, bodyType: t };
      scheduleDraftSave(s.selectedOperationId, next);
      return { current: next };
    }),
  setBody: (raw) =>
    set((s) => {
      const next = { ...s.current, body: raw };
      scheduleDraftSave(s.selectedOperationId, next);
      return { current: next };
    }),
  setFormFields: (fields) =>
    set((s) => {
      const next = { ...s.current, formFields: fields };
      scheduleDraftSave(s.selectedOperationId, next);
      return { current: next };
    }),
  setAuthScheme: (scheme) =>
    set((s) => {
      const next = { ...s.current, authScheme: scheme };
      scheduleDraftSave(s.selectedOperationId, next);
      return { current: next };
    }),
  setAuthToken: (token) =>
    set((s) => {
      const next = { ...s.current, authToken: token };
      scheduleDraftSave(s.selectedOperationId, next);
      return { current: next };
    }),
  resetCurrent: (draft) => {
    const base = { ...emptyDraft, ...draft };
    base.headers = mergeHeaders(base.headers, get().settings.defaultHeaders);
    set({ current: base });
    // resetCurrent fires when the RequestBuilder picks up a fresh
    // operation that has no saved draft yet — capture the seeded
    // defaults so the next reload still sees them under this op.
    scheduleDraftSave(get().selectedOperationId, base);
  },
  /** Explicit "wipe my work for this endpoint" path. Clears the
   *  in-memory draft AND the Dexie row so a reload doesn't
   *  resurrect what the user just discarded. */
  clearCurrent: () => {
    const opId = get().selectedOperationId;
    set({ current: { ...emptyDraft } });
    if (opId) {
      void deleteDraft(opId);
    }
  },

  lastResponse: null,
  inFlight: false,
  send: async () => {
    const state = get();
    const result = buildFetchArgs(state.current, {
      baseUrl: state.settings.baseUrl,
      variables: state.settings.variables,
      includeCredentials: state.settings.includeCredentials,
      globalAuth: state.settings.globalAuth,
    });
    if (!result.ok) {
      const message =
        result.error.kind === "missing_path_param"
          ? `Missing path parameter: ${result.error.name}`
          : `Invalid JSON body: ${result.error.message}`;
      set({
        lastResponse: {
          operationId: state.selectedOperationId ?? "unknown",
          request: { ...state.current },
          status: 0,
          statusText: "Build error",
          durationMs: 0,
          sizeBytes: 0,
          headers: {},
          bodyText: message,
          timestamp: Date.now(),
          error: message,
        },
      });
      return;
    }

    set({ inFlight: true });
    const start = performance.now();

    try {
      const mock = getMockResponse(
        state.selectedOperationId ?? "",
        state.current.method === "DELETE"
          ? 204
          : state.current.method === "POST"
            ? 201
            : 200,
      );

      if (mock) {
        await new Promise((r) => setTimeout(r, 300 + Math.random() * 400));
        const durationMs = Math.round(performance.now() - start);
        const record: ResponseRecord = {
          operationId: state.selectedOperationId ?? "unknown",
          request: { ...state.current },
          status:
            state.current.method === "DELETE"
              ? 204
              : state.current.method === "POST"
                ? 201
                : 200,
          statusText: state.current.method === "DELETE" ? "No Content" : "OK",
          durationMs,
          sizeBytes: new Blob([mock.body]).size,
          headers: mock.headers,
          bodyText: mock.body,
          timestamp: Date.now(),
        };
        set((s) => {
          const opId = state.selectedOperationId ?? "unknown";
          const existing = s.history[opId] ?? [];
          return {
            lastResponse: record,
            inFlight: false,
            history: {
              ...s.history,
              [opId]: [...existing, record],
            },
          };
        });
        saveHistoryDebounced(get().history);
        return;
      }

      const res = await fetch(result.args.url, result.args.init);
      const bodyText = await res.text();
      const durationMs = Math.round(performance.now() - start);
      const headers: Record<string, string> = {};
      res.headers.forEach((v, k) => {
        headers[k] = v;
      });
      const record: ResponseRecord = {
        operationId: state.selectedOperationId ?? "unknown",
        request: { ...state.current },
        status: res.status,
        statusText: res.statusText,
        durationMs,
        sizeBytes: new Blob([bodyText]).size,
        headers,
        bodyText,
        timestamp: Date.now(),
      };
      set((s) => {
        const opId = state.selectedOperationId ?? "unknown";
        const existing = s.history[opId] ?? [];
        return {
          lastResponse: record,
          inFlight: false,
          history: {
            ...s.history,
            [opId]: [...existing, record],
          },
        };
      });
      saveHistoryDebounced(get().history);
    } catch (e) {
      const message = e instanceof Error ? e.message : String(e);
      set({
        inFlight: false,
        lastResponse: {
          operationId: state.selectedOperationId ?? "unknown",
          request: { ...state.current },
          status: 0,
          statusText: "Network error",
          durationMs: Math.round(performance.now() - start),
          sizeBytes: 0,
          headers: {},
          bodyText: "",
          timestamp: Date.now(),
          error: message,
        },
      });
    }
  },

  history: {},
  clearHistory: (operationId) =>
    set((s) => {
      const { [operationId]: _op, ...rest } = s.history;
      return { history: rest };
    }),

  settings: initialSettings,
  saveStatus: "saved",
  lastSavedAt: null,
  setSettings: (patch) => {
    // 1. Apply the patch synchronously and flip to "saving" so the
    //    settings sheet's badge reflects the burst-window state.
    set((s) => ({
      settings: normalizeSettings({ ...s.settings, ...patch }),
      saveStatus: "saving",
    }));
    // 2. Persist asynchronously. Dexie is the source of truth,
    //    localStorage is a boot cache. saveStatus flips to "saved"
    //    on a confirmed Dexie write, "dirty" otherwise. The toast
    //    is debounced so a typing burst surfaces one confirmation.
    void saveSettings(get().settings).then((ok) => {
      set({
        saveStatus: ok ? "saved" : "dirty",
        lastSavedAt: ok ? Date.now() : get().lastSavedAt,
      });
      scheduleSaveToast(ok);
    });
  },
  setBaseUrl: (baseUrl) => get().setSettings({ baseUrl }),
  setVariables: (variables) => get().setSettings({ variables: cloneRows(variables) }),
  setDefaultHeaders: (headers) =>
    get().setSettings({ defaultHeaders: cloneRows(headers) }),
  setIncludeCredentials: (enabled) =>
    get().setSettings({ includeCredentials: enabled }),
  setGlobalAuth: (patch) => {
    const current = get().settings.globalAuth;
    get().setSettings({ globalAuth: { ...current, ...patch } });
  },
  saveSettingsNow: async () => {
    const ok = await saveSettings(get().settings);
    set({
      saveStatus: ok ? "saved" : "dirty",
      lastSavedAt: ok ? Date.now() : get().lastSavedAt,
    });
    // Manual flush bypasses the debounce — fire the toast right
    // away so the click feels responsive.
    if (saveToastTimer) {
      clearTimeout(saveToastTimer);
      saveToastTimer = null;
    }
    pushToast(
      ok
        ? { kind: "success", message: "Settings saved" }
        : {
            kind: "error",
            message:
              "Couldn't save settings — IndexedDB may be blocked (private mode? full?).",
            durationMs: 5000,
          },
    );
    return ok;
  },
  hydrateFromDexie: async () => {
    try {
      const fromDexie = await hydrateInitialSettings();
      if (!fromDexie) return;
      const normalized = normalizeSettings(fromDexie);
      // Only replace if the snapshot actually differs from what
      // we already have — the JSON-string compare is good enough
      // for our small settings shape and avoids spuriously
      // re-rendering subscribers when localStorage and Dexie
      // agree.
      const currentJson = JSON.stringify(get().settings);
      if (JSON.stringify(normalized) === currentJson) return;
      set({
        settings: normalized,
        saveStatus: "saved",
        lastSavedAt: Date.now(),
      });
      // Refresh the localStorage cache so future boots match Dexie
      // without going through hydration first.
      writeLocalStorageCache(normalized);
    } catch (e) {
      if (typeof console !== "undefined") {
        console.warn("[umbra-playground] Dexie hydration failed", e);
      }
    }
  },
  applyDefaultHeaders: () =>
    set((s) => ({
      current: {
        ...s.current,
        headers: mergeHeaders(s.current.headers, s.settings.defaultHeaders),
      },
    })),
  resetSettings: () => {
    const settings = normalizeSettings(baseSettings);
    set({ settings, saveStatus: "saving" });
    void saveSettings(settings).then((ok) => {
      set({
        saveStatus: ok ? "saved" : "dirty",
        lastSavedAt: ok ? Date.now() : get().lastSavedAt,
      });
      // Reset is an explicit user action — bypass the debounce so
      // the confirmation lands immediately. Different toast text
      // distinguishes it from the autosave path.
      if (saveToastTimer) {
        clearTimeout(saveToastTimer);
        saveToastTimer = null;
      }
      pushToast(
        ok
          ? { kind: "info", message: "Settings reset to defaults" }
          : {
              kind: "error",
              message:
                "Reset applied in memory but couldn't persist to IndexedDB.",
              durationMs: 5000,
            },
      );
    });
  },
}));

import { create } from "zustand";
import type { OpenAPIV3 } from "openapi-types";
import { buildFetchArgs } from "./buildFetchArgs";
import { saveHistoryDebounced } from "./history";
import { mockSpec, getMockResponse } from "../data/mockSpec";
import { scopedKey } from "./scope";
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
// each app's localStorage slot is `<app>:<key>`.
const SETTINGS_KEY = scopedKey("umbra-playground-settings:v1");
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

function loadSettings(): PlaygroundSettings {
  if (typeof window === "undefined") return normalizeSettings(baseSettings);
  try {
    const raw = window.localStorage.getItem(SETTINGS_KEY);
    if (!raw) return normalizeSettings(baseSettings);
    return normalizeSettings(JSON.parse(raw) as Partial<PlaygroundSettings>);
  } catch {
    return normalizeSettings(baseSettings);
  }
}

/** `saveSettings` had been a fire-and-forget localStorage write
 *  with no error handling. If the browser threw (quota exceeded,
 *  private mode, storage disabled) the in-memory state still
 *  updated but the next reload silently restored the old settings
 *  — the symptom users actually report as "settings aren't being
 *  saved." Returns `true` on a confirmed write, `false` on any
 *  failure mode (including SSR, where there is no window). The
 *  caller threads this through `saveStatus` so the UI can tell
 *  the truth instead of always saying "saved." */
function saveSettings(settings: PlaygroundSettings): boolean {
  if (typeof window === "undefined") return false;
  try {
    window.localStorage.setItem(SETTINGS_KEY, JSON.stringify(settings));
    return true;
  } catch (e) {
    if (typeof console !== "undefined") {
      console.warn("umbra-playground: settings save failed", e);
    }
    return false;
  }
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
   *  handler for the visible Save button. Returns the success bit
   *  so the caller can fire its own toast. */
  saveSettingsNow: () => boolean;
}

const initialSettings = loadSettings();

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
    // Clear `lastResponse` so the response panel doesn't keep showing
    // the previous endpoint's body/headers/status. The History tab
    // still has all the per-op records (Dexie-backed), so nothing is
    // lost — just hidden when you navigate away. ResponseViewer's
    // smart-default-tab effect picks History as the landing tab when
    // lastResponse is null and history exists for the new endpoint.
    set({ selectedOperationId: id, lastResponse: null });
  },

  current: { ...emptyDraft },
  setMethod: (m) => set((s) => ({ current: { ...s.current, method: m } })),
  setUrl: (u) => set((s) => ({ current: { ...s.current, url: u } })),
  setParams: (params) => set((s) => ({ current: { ...s.current, params } })),
  setHeaders: (headers) => set((s) => ({ current: { ...s.current, headers } })),
  setBodyType: (t) => set((s) => ({ current: { ...s.current, bodyType: t } })),
  setBody: (raw) => set((s) => ({ current: { ...s.current, body: raw } })),
  setFormFields: (fields) =>
    set((s) => ({ current: { ...s.current, formFields: fields } })),
  setAuthScheme: (scheme) =>
    set((s) => ({ current: { ...s.current, authScheme: scheme } })),
  setAuthToken: (token) =>
    set((s) => ({ current: { ...s.current, authToken: token } })),
  resetCurrent: (draft) => {
    const base = { ...emptyDraft, ...draft };
    base.headers = mergeHeaders(base.headers, get().settings.defaultHeaders);
    set({ current: base });
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
    // 2. Persist. The write is synchronous; the toast is debounced
    //    so a typing burst surfaces one confirmation, not one per
    //    keystroke. saveStatus flips to "saved" on success, "dirty"
    //    on failure — that way the UI tells the truth about
    //    persistence rather than just trusting the in-memory state.
    const ok = saveSettings(get().settings);
    set({
      saveStatus: ok ? "saved" : "dirty",
      lastSavedAt: ok ? Date.now() : get().lastSavedAt,
    });
    scheduleSaveToast(ok);
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
  saveSettingsNow: () => {
    const ok = saveSettings(get().settings);
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
              "Couldn't save settings — your browser may be blocking localStorage.",
            durationMs: 5000,
          },
    );
    return ok;
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
    const ok = saveSettings(settings);
    set({
      settings,
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
              "Reset applied in memory but couldn't persist to localStorage.",
            durationMs: 5000,
          },
    );
  },
}));

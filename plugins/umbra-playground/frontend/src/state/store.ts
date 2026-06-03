import { create } from "zustand";
import type { OpenAPIV3 } from "openapi-types";
import { buildFetchArgs } from "./buildFetchArgs";
import { saveHistoryDebounced } from "./history";
import { mockSpec, getMockResponse } from "../data/mockSpec";

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

export interface PlaygroundSettings {
  baseUrl: string;
  variables: KVItem[];
  defaultHeaders: KVItem[];
  includeCredentials: boolean;
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

const SETTINGS_KEY = "umbra-playground-settings:v1";
const SELECTED_KEY = "umbra-playground:selected-operation:v1";

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

const baseSettings: PlaygroundSettings = {
  baseUrl: "",
  variables: [],
  defaultHeaders: factoryDefaultHeaders.map((h) => ({ ...h })),
  includeCredentials: true,
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

function saveSettings(settings: PlaygroundSettings) {
  if (typeof window === "undefined") return;
  window.localStorage.setItem(SETTINGS_KEY, JSON.stringify(settings));
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
  setSettings: (settings: Partial<PlaygroundSettings>) => void;
  setBaseUrl: (baseUrl: string) => void;
  setVariables: (variables: KVItem[]) => void;
  setDefaultHeaders: (headers: KVItem[]) => void;
  setIncludeCredentials: (enabled: boolean) => void;
  applyDefaultHeaders: () => void;
  resetSettings: () => void;
}

const initialSettings = loadSettings();

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
    set({ selectedOperationId: id });
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
  setSettings: (patch) =>
    set((s) => {
      const settings = normalizeSettings({ ...s.settings, ...patch });
      saveSettings(settings);
      return { settings };
    }),
  setBaseUrl: (baseUrl) => get().setSettings({ baseUrl }),
  setVariables: (variables) => get().setSettings({ variables: cloneRows(variables) }),
  setDefaultHeaders: (headers) =>
    get().setSettings({ defaultHeaders: cloneRows(headers) }),
  setIncludeCredentials: (enabled) =>
    get().setSettings({ includeCredentials: enabled }),
  applyDefaultHeaders: () =>
    set((s) => ({
      current: {
        ...s.current,
        headers: mergeHeaders(s.current.headers, s.settings.defaultHeaders),
      },
    })),
  resetSettings: () => {
    const settings = normalizeSettings(baseSettings);
    saveSettings(settings);
    set({ settings });
  },
}));

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

const defaultHeaders: KVItem[] = [
  { key: "Content-Type", value: "application/json", enabled: true },
  { key: "Accept", value: "application/json", enabled: true },
];

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

  selectedOperationId: null,
  selectEndpoint: (id) => set({ selectedOperationId: id }),

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
    // Merge default headers with any user-provided ones, deduping by key.
    const mergedHeaders = [...defaultHeaders];
    for (const h of base.headers) {
      const idx = mergedHeaders.findIndex((d) => d.key === h.key);
      if (idx >= 0) {
        mergedHeaders[idx] = { ...h };
      } else {
        mergedHeaders.push({ ...h });
      }
    }
    base.headers = mergedHeaders;
    set({ current: base });
  },

  lastResponse: null,
  inFlight: false,
  send: async () => {
    const state = get();
    const result = buildFetchArgs(state.current);
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
}));

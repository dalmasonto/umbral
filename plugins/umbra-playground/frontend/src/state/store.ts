import { create } from "zustand";
import type { OpenAPIV3 } from "openapi-types";
import { buildFetchArgs } from "./buildFetchArgs";
import { saveHistoryDebounced } from "./history";

/** A request as the user has constructed it in the builder. */
export interface RequestDraft {
  method: string;
  url: string;
  params: Record<string, string>;
  headers: Record<string, string>;
  body: string;
  bearerToken: string;
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
  setParam: (name: string, value: string) => void;
  setHeader: (name: string, value: string) => void;
  setBody: (raw: string) => void;
  setBearerToken: (t: string) => void;
  resetCurrent: (draft: Partial<RequestDraft>) => void;

  // response
  lastResponse: ResponseRecord | null;
  inFlight: boolean;
  send: () => Promise<void>;

  // history
  history: Record<string, ResponseRecord[]>;
  clearHistory: (operationId: string) => void;
}

const emptyDraft: RequestDraft = {
  method: "GET",
  url: "",
  params: {},
  headers: {},
  body: "",
  bearerToken: "",
};

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
    } catch (e) {
      set({
        specError: e instanceof Error ? e.message : String(e),
        loadingSpec: false,
      });
    }
  },

  selectedOperationId: null,
  selectEndpoint: (id) => set({ selectedOperationId: id }),

  current: { ...emptyDraft },
  setMethod: (m) => set((s) => ({ current: { ...s.current, method: m } })),
  setUrl: (u) => set((s) => ({ current: { ...s.current, url: u } })),
  setParam: (name, value) =>
    set((s) => ({
      current: { ...s.current, params: { ...s.current.params, [name]: value } },
    })),
  setHeader: (name, value) =>
    set((s) => ({
      current: { ...s.current, headers: { ...s.current.headers, [name]: value } },
    })),
  setBody: (raw) => set((s) => ({ current: { ...s.current, body: raw } })),
  setBearerToken: (t) =>
    set((s) => ({ current: { ...s.current, bearerToken: t } })),
  resetCurrent: (draft) =>
    set({ current: { ...emptyDraft, ...draft } }),

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
      const res = await fetch(result.args.url, result.args.init);
      const bodyText = await res.text();
      const durationMs = performance.now() - start;
      const headers: Record<string, string> = {};
      res.headers.forEach((v, k) => {
        headers[k] = v;
      });
      const record: ResponseRecord = {
        operationId: state.selectedOperationId ?? "unknown",
        request: { ...state.current },
        status: res.status,
        statusText: res.statusText,
        durationMs: Math.round(durationMs),
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

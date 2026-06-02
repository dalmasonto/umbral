import { create } from "zustand";
import type { OpenAPIV3 } from "openapi-types";

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
    set((s) => ({ current: { ...emptyDraft, ...draft } })),

  lastResponse: null,
  inFlight: false,
  send: async () => {
    // Implementation lands in M4.
    throw new Error("send() not yet implemented — M4");
  },

  history: {},
  clearHistory: (operationId) =>
    set((s) => {
      const { [operationId]: _op, ...rest } = s.history;
      return { history: rest };
    }),
}));

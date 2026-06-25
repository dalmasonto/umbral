import { create } from "zustand";

export type ToastKind = "success" | "info" | "error";

export interface ToastEntry {
  id: number;
  kind: ToastKind;
  message: string;
  /** Auto-dismiss timeout in ms. Defaults to 2500. */
  durationMs?: number;
}

interface ToastState {
  toasts: ToastEntry[];
  push: (entry: Omit<ToastEntry, "id">) => number;
  dismiss: (id: number) => void;
}

let nextId = 1;

export const useToastStore = create<ToastState>((set) => ({
  toasts: [],
  push: (entry) => {
    const id = nextId++;
    set((s) => ({
      toasts: [...s.toasts, { id, durationMs: 2500, ...entry }],
    }));
    return id;
  },
  dismiss: (id) =>
    set((s) => ({ toasts: s.toasts.filter((t) => t.id !== id) })),
}));

/** Imperative helper — convenient from non-React code paths
 *  (e.g. the saveSettings hook in store.ts). */
export function pushToast(entry: Omit<ToastEntry, "id">): number {
  return useToastStore.getState().push(entry);
}

import type { RequestDraft } from "./store";

export interface FetchArgs {
  url: string;
  init: RequestInit;
}

export type BuildError =
  | { kind: "missing_path_param"; name: string }
  | { kind: "invalid_json_body"; message: string };

export function buildFetchArgs(draft: RequestDraft): {
  ok: true; args: FetchArgs;
} | { ok: false; error: BuildError } {
  // Full implementation lands in M4. This is a stub that errors so
  // we can wire up the error path in tests before the real logic.
  return {
    ok: false,
    error: { kind: "invalid_json_body", message: "not yet implemented" },
  };
}

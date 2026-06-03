import { useEffect, useRef, useState } from "react";
import { loadEditorState, saveEditorState } from "@/state/editorState";

/** `useState`-shaped hook backed by the Dexie `editorState` table.
 *
 *  The dance:
 *  1. On mount, start with `defaultValue` so the UI has something to
 *     render synchronously.
 *  2. Asynchronously load the persisted slot. When it arrives,
 *     `setValue` updates the state, triggering a re-render with the
 *     hydrated value.
 *  3. After hydration, every subsequent setValue call also writes the
 *     value to Dexie. We gate the write on hydration so the initial
 *     default-value render doesn't clobber the stored slot before
 *     we've read it.
 *
 *  Returns the same `[value, setValue]` tuple shape as `useState`.
 *
 *  The `key` should be stable across renders (a string literal at the
 *  call site is fine). Changing the key mid-component lifecycle is
 *  undefined — the hydration effect re-runs but the persist effect's
 *  closure can race the load. We don't currently exercise that path. */
export function usePersistedState<T>(
  key: string,
  defaultValue: T,
): [T, (value: T) => void] {
  const [value, setValue] = useState<T>(defaultValue);
  const hydratedRef = useRef(false);

  useEffect(() => {
    let alive = true;
    void loadEditorState<T>(key, defaultValue).then((loaded) => {
      if (!alive) return;
      setValue(loaded);
      hydratedRef.current = true;
    });
    return () => {
      alive = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key]);

  useEffect(() => {
    if (!hydratedRef.current) return;
    void saveEditorState(key, value);
  }, [key, value]);

  return [value, setValue];
}

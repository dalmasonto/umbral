import { useEffect, useState } from "react";
import Editor from "@monaco-editor/react";

/** Tracks the `.dark` class on `<html>` so Monaco's theme follows
 *  the app theme toggle without us threading it through every parent
 *  prop. Defined as a hook (not a context) because Monaco's theme
 *  prop wants a string and the override is purely visual. */
export function useIsDark(): boolean {
  const [dark, setDark] = useState(() =>
    document.documentElement.classList.contains("dark"),
  );
  useEffect(() => {
    const obs = new MutationObserver(() =>
      setDark(document.documentElement.classList.contains("dark")),
    );
    obs.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["class"],
    });
    return () => obs.disconnect();
  }, []);
  return dark;
}

interface ReadonlyMonacoProps {
  value: string;
  language: string;
  /** Optional minimum height. Defaults to filling the parent with
   *  `flex-1 min-h-0 h-full`, which is what the ResponseViewer's
   *  body tab wants. Override for fixed-size contexts like the
   *  history detail dialog where we want a tall but bounded panel. */
  className?: string;
  /** Override Monaco's height prop. Defaults to "100%". */
  height?: string | number;
}

/** Monaco editor wired in read-only mode with the playground's
 *  shared defaults: minimap off, soft-wrap, JetBrains-ish mono
 *  stack, dark/light theme tracked from the `.dark` class on html.
 *
 *  Used by both `ResponseViewer` (Body / cURL tabs) and
 *  `HistoryRecordDialog` (request body, response body) so any
 *  styling tweaks land in one place. */
export function ReadonlyMonaco({
  value,
  language,
  className,
  height = "100%",
}: ReadonlyMonacoProps) {
  const isDark = useIsDark();
  return (
    <div
      className={
        className ??
        "flex-1 min-h-0 h-full rounded-md overflow-hidden border border-border"
      }
    >
      <Editor
        height={height}
        language={language}
        theme={isDark ? "vs-dark" : "light"}
        value={value}
        options={{
          readOnly: true,
          minimap: { enabled: false },
          lineNumbers: "on",
          wordWrap: "on",
          folding: true,
          scrollBeyondLastLine: false,
          automaticLayout: true,
          fontSize: 13,
          fontFamily:
            'ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", monospace',
          tabSize: 2,
        }}
      />
    </div>
  );
}

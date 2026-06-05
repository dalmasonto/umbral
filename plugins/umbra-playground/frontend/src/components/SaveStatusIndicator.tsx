import { useEffect, useReducer } from "react";
import { usePlayground } from "@/state/store";
import { Badge } from "@/components/ui/badge";
import { AlertCircle, CheckCircle2 } from "lucide-react";

function formatAgo(timestamp: number): string {
  const seconds = Math.max(0, Math.floor((Date.now() - timestamp) / 1000));
  if (seconds < 5) return "just now";
  if (seconds < 60) return `${seconds}s ago`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.floor(minutes / 60);
  return `${hours}h ago`;
}

/** Persistent save-state indicator. Sits in the header so the
 *  user always knows whether their settings are in sync with
 *  storage, without having to open the settings sheet.
 *
 *  Three states:
 *  - "saving" — pulse + grey badge during the autosave burst.
 *  - "saved"  — green check + "Saved" (or "Saved <n>s ago").
 *  - "dirty"  — red alert + "Unsaved" so the user knows the
 *               last write attempt didn't actually land.
 *
 *  Auto-save is silent (no toast) — this badge is the
 *  persistent feedback. Manual Ctrl+S / "Save now" is the
 *  loud path: toast AND badge update. */
export function SaveStatusIndicator() {
  const saveStatus = usePlayground((s) => s.saveStatus);
  const lastSavedAt = usePlayground((s) => s.lastSavedAt);
  // Re-render every 5s so the "Saved <n>s ago" text creeps
  // without demanding an upstream tick.
  const [, force] = useReducer((x: number) => x + 1, 0);
  useEffect(() => {
    if (saveStatus !== "saved" || lastSavedAt === null) return;
    const handle = setInterval(force, 5000);
    return () => clearInterval(handle);
  }, [saveStatus, lastSavedAt]);

  if (saveStatus === "saving") {
    return (
      <Badge variant="outline" className="gap-1.5" title="Saving…">
        <span className="size-2 animate-pulse rounded-full bg-amber-500" />
        <span className="hidden sm:inline">Saving</span>
      </Badge>
    );
  }
  if (saveStatus === "dirty") {
    return (
      <Badge
        variant="destructive"
        className="gap-1.5"
        title="Last write attempt failed — your settings may be out of sync with storage."
      >
        <AlertCircle className="size-3" />
        <span className="hidden sm:inline">Unsaved</span>
      </Badge>
    );
  }
  return (
    <Badge
      variant="secondary"
      className="gap-1.5 text-emerald-600 dark:text-emerald-400"
      title={
        lastSavedAt === null
          ? "No settings saved this session."
          : `Last saved ${formatAgo(lastSavedAt)}.`
      }
    >
      <CheckCircle2 className="size-3" />
      <span className="hidden sm:inline">
        {lastSavedAt === null ? "Saved" : `Saved ${formatAgo(lastSavedAt)}`}
      </span>
    </Badge>
  );
}

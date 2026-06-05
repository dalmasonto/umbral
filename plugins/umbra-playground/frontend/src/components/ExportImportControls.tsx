import { useRef, useState } from "react";
import { usePlayground } from "@/state/store";
import { db, type HistoryRow } from "@/state/db";
import { getAppScope } from "@/state/scope";
import { loadDraft } from "@/state/draftStorage";
import { pushToast } from "@/state/toastStore";
import { Button } from "@/components/ui/button";
import { Download, Upload } from "lucide-react";

const SNAPSHOT_VERSION = 1 as const;
const PER_OPERATION_CAP = 50;

interface Snapshot {
  version: typeof SNAPSHOT_VERSION;
  exportedAt: number;
  appScope: string;
  tabs: ReturnType<typeof usePlayground.getState>["openTabs"];
  drafts: Record<
    string,
    NonNullable<Awaited<ReturnType<typeof loadDraft>>>
  >;
  history: HistoryRow[];
  settings: ReturnType<typeof usePlayground.getState>["settings"];
}

function isSnapshot(value: unknown): value is Snapshot {
  if (typeof value !== "object" || value === null) return false;
  const v = value as Record<string, unknown>;
  return (
    v["version"] === SNAPSHOT_VERSION &&
    Array.isArray(v["tabs"]) &&
    typeof v["drafts"] === "object" &&
    v["drafts"] !== null &&
    !Array.isArray(v["drafts"]) &&
    Array.isArray(v["history"]) &&
    typeof v["settings"] === "object" &&
    v["settings"] !== null
  );
}

function formatDate(timestamp: number): string {
  const d = new Date(timestamp);
  const yyyy = d.getFullYear();
  const mm = String(d.getMonth() + 1).padStart(2, "0");
  const dd = String(d.getDate()).padStart(2, "0");
  return `${yyyy}-${mm}-${dd}`;
}

/** Two icon buttons — Download triggers an export of the current
 *  workspace, Upload opens a file picker. Sits at the right end
 *  of the tab strip. */
export function ExportImportControls() {
  const fileInputRef = useRef<HTMLInputElement | null>(null);
  const [busy, setBusy] = useState(false);
  const openTabs = usePlayground((s) => s.openTabs);
  const setActiveTab = usePlayground((s) => s.setActiveTab);
  const openTab = usePlayground((s) => s.openTab);

  const onExport = async () => {
    if (busy) return;
    setBusy(true);
    try {
      const state = usePlayground.getState();
      const drafts: Snapshot["drafts"] = {};
      for (const tab of openTabs) {
        const draft = await loadDraft(tab.operationId);
        if (draft) drafts[tab.operationId] = draft;
      }
      const history = await db.history.toArray();
      const snapshot: Snapshot = {
        version: SNAPSHOT_VERSION,
        exportedAt: Date.now(),
        appScope: getAppScope(),
        tabs: openTabs,
        drafts,
        history,
        settings: state.settings,
      };
      const blob = new Blob([JSON.stringify(snapshot, null, 2)], {
        type: "application/json",
      });
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = `umbra-playground-${getAppScope()}-${formatDate(
        snapshot.exportedAt,
      )}.json`;
      document.body.appendChild(a);
      a.click();
      a.remove();
      URL.revokeObjectURL(url);
      pushToast({
        kind: "success",
        message: `Exported ${openTabs.length} tab${openTabs.length === 1 ? "" : "s"}, ${history.length} history rows`,
      });
    } catch (e) {
      pushToast({
        kind: "error",
        message: `Export failed: ${e instanceof Error ? e.message : String(e)}`,
        durationMs: 5000,
      });
    } finally {
      setBusy(false);
    }
  };

  const onFile = async (event: React.ChangeEvent<HTMLInputElement>) => {
    const file = event.target.files?.[0];
    // Always reset the input so picking the same file again
    // re-fires onChange.
    event.target.value = "";
    if (!file) return;
    if (busy) return;
    setBusy(true);
    try {
      const text = await file.text();
      let parsed: unknown;
      try {
        parsed = JSON.parse(text);
      } catch {
        pushToast({ kind: "error", message: "Not a valid JSON file." });
        return;
      }
      if (!isSnapshot(parsed)) {
        pushToast({
          kind: "error",
          message: "Not a playground snapshot.",
        });
        return;
      }
      const state = usePlayground.getState();
      const localOpIds = new Set(state.openTabs.map((t) => t.operationId));
      const newTabs = parsed.tabs
        .filter((t) => !localOpIds.has(t.operationId))
        .map((t) => ({
          ...t,
          id:
            typeof crypto !== "undefined" && "randomUUID" in crypto
              ? crypto.randomUUID()
              : `tab-${Date.now()}-${Math.random().toString(36).slice(2)}`,
        }));
      const nextOpenTabs = [...state.openTabs, ...newTabs];
      let nextActiveId = state.activeTabId;
      if (newTabs.length > 0 && !nextActiveId) {
        nextActiveId = newTabs[0]!.id;
      }
      usePlayground.setState({
        openTabs: nextOpenTabs,
        activeTabId: nextActiveId,
      });
      // Drafts: put only if no local row exists.
      for (const [operationId, draft] of Object.entries(parsed.drafts)) {
        const existing = await db.drafts.get(operationId);
        if (existing) continue;
        await db.drafts.put({
          operationId,
          schema: 1,
          draft,
          updatedAt: Date.now(),
        });
      }
      // History: dedupe by (operationId, timestamp).
      const existingRows = await db.history.toArray();
      const seen = new Set(
        existingRows.map(
          (r) => `${r.operationId}::${r.timestamp}`,
        ),
      );
      const newHistory: HistoryRow[] = [];
      for (const row of parsed.history) {
        const key = `${row.operationId}::${row.timestamp}`;
        if (seen.has(key)) continue;
        seen.add(key);
        newHistory.push(row);
      }
      if (newHistory.length > 0) {
        // Cap per operation at PER_OPERATION_CAP.
        const grouped = new Map<string, HistoryRow[]>();
        for (const r of newHistory) {
          const arr = grouped.get(r.operationId) ?? [];
          arr.push(r);
          grouped.set(r.operationId, arr);
        }
        const allExisting = await db.history.toArray();
        const byOp = new Map<string, number>();
        for (const r of allExisting) {
          byOp.set(r.operationId, (byOp.get(r.operationId) ?? 0) + 1);
        }
        const toAdd: HistoryRow[] = [];
        for (const [op, rows] of grouped) {
          const have = byOp.get(op) ?? 0;
          const room = Math.max(0, PER_OPERATION_CAP - have);
          toAdd.push(...rows.slice(0, room));
        }
        if (toAdd.length > 0) {
          await db.history.bulkAdd(toAdd);
        }
      }
      // Settings: import only if local has none.
      const localSettingsCount = await db.settings.count();
      if (localSettingsCount === 0 && parsed.settings) {
        await db.settings.put({
          key: "workspace",
          schema: 1,
          value: parsed.settings,
          updatedAt: Date.now(),
        });
      }
      // Activate the first newly imported tab so the user
      // immediately sees the change.
      if (newTabs.length > 0) {
        // openTab handles the dedupe and selectEndpoint.
        openTab(newTabs[0]!.operationId);
      } else if (nextActiveId) {
        setActiveTab(nextActiveId);
      }
      pushToast({
        kind: "success",
        message: `Imported ${newTabs.length} tab${newTabs.length === 1 ? "" : "s"}, ${newHistory.length} history rows`,
      });
    } catch (e) {
      pushToast({
        kind: "error",
        message: `Import failed: ${e instanceof Error ? e.message : String(e)}`,
        durationMs: 5000,
      });
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="flex items-center gap-1">
      <Button
        type="button"
        variant="ghost"
        size="icon-xs"
        onClick={() => void onExport()}
        disabled={busy}
        title="Export playground snapshot"
        aria-label="Export playground snapshot"
        className="text-muted-foreground hover:text-foreground"
      >
        <Download className="size-3.5" />
      </Button>
      <Button
        type="button"
        variant="ghost"
        size="icon-xs"
        onClick={() => fileInputRef.current?.click()}
        disabled={busy}
        title="Import playground snapshot"
        aria-label="Import playground snapshot"
        className="text-muted-foreground hover:text-foreground"
      >
        <Upload className="size-3.5" />
      </Button>
      <input
        ref={fileInputRef}
        type="file"
        accept="application/json"
        hidden
        onChange={(e) => void onFile(e)}
      />
    </div>
  );
}

import { useEffect } from "react";
import { usePlayground } from "../state/store";
import { findOperation } from "../state/spec";
import { EmptyState } from "./EmptyState";

/**
 * M3 stub: shows the URL strip populated when an endpoint is selected.
 * Full implementation (tabs, body editor, etc.) lands in M4.
 */
export function RequestBuilder() {
  const spec = usePlayground((s) => s.spec);
  const selected = usePlayground((s) => s.selectedOperationId);
  const current = usePlayground((s) => s.current);
  const resetCurrent = usePlayground((s) => s.resetCurrent);

  const op = spec && selected ? findOperation(spec, selected) : null;

  useEffect(() => {
    if (op) {
      resetCurrent({ method: op.method, url: op.path });
    }
  }, [op?.method, op?.path, resetCurrent]);

  if (!selected) {
    return <EmptyState title="Select an endpoint" />;
  }
  return (
    <div className="p-3 space-y-2">
      <div className="flex items-center gap-2">
        <span className="font-mono text-xs px-2 py-1 rounded bg-slate-900 border border-slate-800 text-slate-300">
          {current.method}
        </span>
        <code className="font-mono text-sm text-slate-200 flex-1 truncate">
          {current.url}
        </code>
      </div>
      <p className="font-mono text-[10px] text-slate-600">
        Request builder tabs land in M4.
      </p>
    </div>
  );
}

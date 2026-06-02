import { usePlayground } from "../state/store";

export function Header() {
  const spec = usePlayground((s) => s.spec);
  const loadSpec = usePlayground((s) => s.loadSpec);

  return (
    <header className="border-b border-slate-800 px-4 py-2 flex items-center justify-between bg-slate-950/60">
      <div className="flex items-baseline gap-3">
        <span className="font-mono text-xs tracking-widest text-slate-500">umbra</span>
        <span className="font-mono text-xs text-slate-300">
          {spec?.info?.title ?? "playground"}
        </span>
        {spec?.info?.version && (
          <span className="font-mono text-[10px] text-slate-500">
            v{spec.info.version}
          </span>
        )}
      </div>
      <button
        type="button"
        onClick={() => void loadSpec()}
        className="text-[10px] font-mono uppercase tracking-widest text-slate-500 hover:text-slate-200 px-2 py-1 rounded focus-visible:outline focus-visible:outline-2 focus-visible:outline-indigo-400"
      >
        Reload spec
      </button>
    </header>
  );
}

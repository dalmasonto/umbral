import { usePlayground } from "../state/store";

export function AuthTab() {
  const bearer = usePlayground((s) => s.current.bearerToken);
  const setBearer = usePlayground((s) => s.setBearerToken);

  return (
    <div className="space-y-3 text-xs">
      <div>
        <label className="block font-mono text-[10px] uppercase tracking-widest text-slate-500 mb-1">
          Bearer token
        </label>
        <input
          type="password"
          value={bearer}
          onChange={(e) => setBearer(e.target.value)}
          placeholder="paste token here"
          className="w-full px-2 py-1 bg-slate-900 border border-slate-800 rounded font-mono text-slate-200 placeholder-slate-600 focus:outline-none focus:border-indigo-500"
        />
        <p className="mt-1.5 text-[10px] text-slate-600">
          Sent as <code className="font-mono text-slate-400">Authorization: Bearer ...</code> on every request.
        </p>
      </div>
      <p className="text-[10px] text-slate-600 leading-relaxed">
        For session-based auth, log into the app in another tab first. The
        playground shares cookies with the rest of the app.
      </p>
    </div>
  );
}

export function ErrorBanner({
  message,
  onRetry,
}: {
  message: string;
  onRetry?: () => void;
}) {
  return (
    <div className="border-b border-rose-900/50 bg-rose-950/30 px-3 py-2 flex items-center justify-between gap-3">
      <span className="text-xs font-mono text-rose-300">
        <span className="font-semibold mr-2">Error:</span>
        {message}
      </span>
      {onRetry && (
        <button
          type="button"
          onClick={onRetry}
          className="text-[10px] font-mono uppercase tracking-widest text-rose-200 hover:text-white px-2 py-0.5 rounded focus-visible:outline focus-visible:outline-2 focus-visible:outline-rose-400"
        >
          Retry
        </button>
      )}
    </div>
  );
}

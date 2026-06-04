import { useEffect } from "react";
import { CheckCircle2, AlertCircle, Info, X } from "lucide-react";
import { useToastStore, type ToastEntry } from "@/state/toastStore";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

/** Bottom-right toast viewport. Mounted once at the App root.
 *  Each toast auto-dismisses after its `durationMs` (default 2.5s);
 *  hovering doesn't pause for now — these are status confirmations,
 *  not actionable messages. */
export function Toaster() {
  const toasts = useToastStore((s) => s.toasts);
  const dismiss = useToastStore((s) => s.dismiss);

  return (
    <div
      role="region"
      aria-label="Notifications"
      className="pointer-events-none fixed bottom-4 right-4 z-[100] flex flex-col gap-2"
    >
      {toasts.map((toast) => (
        <ToastItem key={toast.id} toast={toast} onDismiss={dismiss} />
      ))}
    </div>
  );
}

function ToastItem({
  toast,
  onDismiss,
}: {
  toast: ToastEntry;
  onDismiss: (id: number) => void;
}) {
  useEffect(() => {
    const duration = toast.durationMs ?? 2500;
    const handle = setTimeout(() => onDismiss(toast.id), duration);
    return () => clearTimeout(handle);
  }, [toast.id, toast.durationMs, onDismiss]);

  const Icon =
    toast.kind === "error"
      ? AlertCircle
      : toast.kind === "info"
        ? Info
        : CheckCircle2;
  const iconColour =
    toast.kind === "error"
      ? "text-destructive"
      : toast.kind === "info"
        ? "text-sky-500"
        : "text-emerald-500";

  return (
    <div
      className={cn(
        "pointer-events-auto flex items-center gap-3 rounded-lg border border-border bg-popover px-3 py-2 shadow-lg",
        "min-w-[240px] max-w-[360px]",
        "animate-in slide-in-from-bottom-2 fade-in",
      )}
      role="status"
    >
      <Icon className={cn("size-4 shrink-0", iconColour)} />
      <span className="flex-1 text-sm text-foreground">{toast.message}</span>
      <Button
        type="button"
        variant="ghost"
        size="icon-sm"
        onClick={() => onDismiss(toast.id)}
        className="size-6 text-muted-foreground hover:text-foreground"
        title="Dismiss"
      >
        <X className="size-3.5" />
      </Button>
    </div>
  );
}

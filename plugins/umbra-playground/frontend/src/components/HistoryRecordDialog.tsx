import { useMemo, useState } from "react";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { ScrollArea } from "@/components/ui/scroll-area";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { Clock, HardDrive, Copy, Check, AlertTriangle } from "lucide-react";
import { MethodBadge } from "./MethodBadge";
import { ReadonlyMonaco } from "./ReadonlyMonaco";
import { isHtmlContentType } from "./ResponseViewer";
import type { ResponseRecord } from "@/state/store";

interface HistoryRecordDialogProps {
  record: ResponseRecord | null;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

function statusTone(status: number, error?: string): string {
  if (error) return "border-rose-500/40 text-rose-600";
  if (status >= 200 && status < 300)
    return "border-emerald-500/40 text-emerald-600";
  if (status >= 300 && status < 400)
    return "border-amber-500/40 text-amber-600";
  if (status >= 400) return "border-rose-500/40 text-rose-600";
  return "border-muted-foreground/30 text-muted-foreground";
}

function prettyJson(value: string): string {
  try {
    return JSON.stringify(JSON.parse(value), null, 2);
  } catch {
    return value;
  }
}

function CopyButton({ value }: { value: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <Button
      type="button"
      variant="ghost"
      size="xs"
      onClick={() => {
        void navigator.clipboard.writeText(value);
        setCopied(true);
        setTimeout(() => setCopied(false), 1200);
      }}
      className="text-muted-foreground hover:text-foreground text-[10px] gap-1"
    >
      {copied ? (
        <>
          <Check className="size-3 text-emerald-600" />
          Copied
        </>
      ) : (
        <>
          <Copy className="size-3" />
          Copy
        </>
      )}
    </Button>
  );
}

function KVTable({
  rows,
  emptyLabel,
}: {
  rows: Array<[string, string]>;
  emptyLabel: string;
}) {
  if (rows.length === 0) {
    return (
      <p className="px-2 py-3 text-xs italic text-muted-foreground">
        {emptyLabel}
      </p>
    );
  }
  return (
    <div className="overflow-hidden rounded-md border border-border">
      <Table>
        <TableHeader className="bg-muted/40">
          <TableRow className="hover:bg-muted/40">
            <TableHead className="w-[14rem]">Key</TableHead>
            <TableHead>Value</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {rows.map(([k, v], i) => (
            <TableRow key={`${k}-${i}`}>
              <TableCell className="font-mono text-xs font-medium text-foreground break-all">
                {k}
              </TableCell>
              <TableCell className="font-mono text-xs text-muted-foreground break-all">
                {v}
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
    </div>
  );
}

function CodeBlock({
  value,
  language = "json",
  emptyLabel,
}: {
  value: string;
  language?: string;
  emptyLabel: string;
}) {
  const pretty = useMemo(() => prettyJson(value), [value]);
  if (!value) {
    return (
      <p className="px-2 py-3 text-xs italic text-muted-foreground">
        {emptyLabel}
      </p>
    );
  }
  // Monaco's IntersectionObserver-based auto-layout needs a sized
  // wrapper; 18rem gives enough room for ~15 lines of code while
  // staying inside the dialog's overall max-h without nested scroll.
  return (
    <div className="relative rounded-md border border-border overflow-hidden">
      <div className="absolute right-2 top-2 z-10">
        <CopyButton value={pretty} />
      </div>
      <ReadonlyMonaco
        value={pretty}
        language={language}
        height="18rem"
        className="h-[18rem]"
      />
    </div>
  );
}

function Section({
  title,
  children,
}: {
  title: string;
  children: React.ReactNode;
}) {
  return (
    <section className="space-y-2">
      <p className="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
        {title}
      </p>
      {children}
    </section>
  );
}

export function HistoryRecordDialog({
  record,
  open,
  onOpenChange,
}: HistoryRecordDialogProps) {
  if (!record) return null;

  const requestParams: Array<[string, string]> = record.request.params
    .filter((p) => p.enabled && p.key)
    .map((p) => [p.key, p.value]);
  const requestHeaders: Array<[string, string]> = record.request.headers
    .filter((h) => h.enabled && h.key)
    .map((h) => [h.key, h.value]);
  if (record.request.authToken) {
    requestHeaders.push([
      "Authorization",
      `${record.request.authScheme} ${record.request.authToken}`,
    ]);
  }
  const formFields: Array<[string, string]> = record.request.formFields
    .filter((f) => f.enabled && f.key)
    .map((f) => [
      f.key,
      f.type === "file" ? `(file: ${f.fileName ?? "unnamed"})` : f.value,
    ]);
  const responseHeaders: Array<[string, string]> = Object.entries(
    record.headers,
  ).sort(([a], [b]) => a.toLowerCase().localeCompare(b.toLowerCase()));

  const timestamp = new Date(record.timestamp);
  const tone = statusTone(record.status, record.error);

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-[min(calc(100vw-2rem),64rem)] sm:max-w-[64rem] gap-0 p-0 rounded-2xl border border-border bg-popover shadow-2xl overflow-hidden">
        <DialogHeader className="border-b border-border bg-muted/20 px-6 py-4 gap-2">
          <div className="flex items-center gap-2.5 flex-wrap pr-8">
            <MethodBadge method={record.request.method} />
            <DialogTitle className="font-mono text-sm truncate flex-1 min-w-0 leading-tight">
              {record.request.url}
            </DialogTitle>
            <Badge
              variant="outline"
              className={`font-mono text-[11px] font-bold ${tone}`}
            >
              {record.error ? "ERROR" : `${record.status} ${record.statusText}`}
            </Badge>
          </div>
          <DialogDescription asChild>
            <div className="flex flex-wrap items-center gap-3 text-[11px] text-muted-foreground font-mono">
              <span className="flex items-center gap-1">
                <Clock className="size-3" />
                {record.durationMs}ms
              </span>
              <span className="flex items-center gap-1">
                <HardDrive className="size-3" />
                {(record.sizeBytes / 1024).toFixed(2)} KB
              </span>
              <span className="text-muted-foreground/80">
                {timestamp.toLocaleString()}
              </span>
            </div>
          </DialogDescription>
        </DialogHeader>

        <ScrollArea className="max-h-[calc(100vh-12rem)]">
          <div className="grid gap-6 px-6 py-5 lg:grid-cols-2">
            {/* Request column */}
            <div className="space-y-5">
              <p className="text-[11px] font-semibold uppercase tracking-wider text-primary border-b border-border pb-2">
                Request
              </p>
              <Section title="Query parameters">
                <KVTable
                  rows={requestParams}
                  emptyLabel="No query parameters."
                />
              </Section>
              <Section title="Headers">
                <KVTable
                  rows={requestHeaders}
                  emptyLabel="No headers (auth not configured)."
                />
              </Section>
              {record.request.bodyType === "form" ? (
                <Section title="Form fields">
                  <KVTable rows={formFields} emptyLabel="No form fields." />
                </Section>
              ) : (
                <Section title="JSON body">
                  <CodeBlock
                    value={record.request.body}
                    emptyLabel="No body."
                  />
                </Section>
              )}
            </div>

            {/* Response column */}
            <div className="space-y-5">
              <p className="text-[11px] font-semibold uppercase tracking-wider text-primary border-b border-border pb-2">
                Response
              </p>
              {record.error && (
                <div className="flex items-start gap-2 rounded-md border border-rose-500/30 bg-rose-500/5 p-3 text-xs">
                  <AlertTriangle className="size-3.5 shrink-0 mt-0.5 text-rose-600" />
                  <div className="space-y-0.5">
                    <p className="font-semibold text-rose-600">
                      {record.statusText}
                    </p>
                    <p className="text-[11px] text-rose-600/80 break-all">
                      {record.error}
                    </p>
                  </div>
                </div>
              )}
              <Section title="Headers">
                <KVTable
                  rows={responseHeaders}
                  emptyLabel="No headers received."
                />
              </Section>
              <Section title="Body">
                {isHtmlContentType(record.headers) ? (
                  // HTML responses render in a sandboxed iframe —
                  // same treatment as the live Response tab so a
                  // 500-page template or admin shell is actually
                  // visible, not dumped as raw text.
                  <iframe
                    title="HTML response preview"
                    srcDoc={record.bodyText}
                    sandbox=""
                    className="w-full h-64 border border-border rounded-md bg-white"
                  />
                ) : (
                  <CodeBlock
                    value={record.bodyText}
                    emptyLabel="Empty response body."
                  />
                )}
              </Section>
            </div>
          </div>
        </ScrollArea>
      </DialogContent>
    </Dialog>
  );
}

import { useState, useRef } from "react";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Plus, Trash2, FileText, FileUp } from "lucide-react";
import type { KVItem } from "@/state/store";
import { registerFile } from "@/state/fileRegistry";

interface KeyValueEditorProps {
  rows: KVItem[];
  onChange: (rows: KVItem[]) => void;
  keyPlaceholder?: string;
  valuePlaceholder?: string;
  allowFileType?: boolean;
}

export function KeyValueEditor({
  rows,
  onChange,
  keyPlaceholder = "Key",
  valuePlaceholder = "Value",
  allowFileType = false,
}: KeyValueEditorProps) {
  const [draft, setDraft] = useState<KVItem>({
    key: "",
    value: "",
    enabled: true,
    type: "text",
  });
  const [editing, setEditing] = useState(false);
  const fileInputRef = useRef<HTMLInputElement | null>(null);

  const updateRow = (index: number, patch: Partial<KVItem>) => {
    const next = rows.map((r, i) => (i === index ? { ...r, ...patch } : r));
    onChange(next);
  };

  const removeRow = (index: number) => {
    const row = rows[index];
    if (row?.key && row.type === "file") {
      registerFile(row.key, null);
    }
    onChange(rows.filter((_, i) => i !== index));
  };

  const commitDraft = () => {
    if (!draft.key.trim() && !draft.value.trim() && draft.type !== "file") {
      setEditing(false);
      setDraft({ key: "", value: "", enabled: true, type: "text" });
      return;
    }
    onChange([
      ...rows,
      {
        key: draft.key.trim(),
        value: draft.type === "file" ? draft.fileName || "" : draft.value.trim(),
        enabled: true,
        type: draft.type || "text",
        fileName: draft.fileName,
      },
    ]);
    setDraft({ key: "", value: "", enabled: true, type: "text" });
    // Keep editing open with a fresh row for rapid entry.
  };

  return (
    <div className="space-y-1.5">
      {rows.map((row, i) => (
        <div key={i} className="flex items-center gap-1.5 group">
          <Checkbox
            checked={row.enabled}
            onCheckedChange={(checked) =>
              updateRow(i, { enabled: checked === true })
            }
            className="size-4 shrink-0"
          />
          <Input
            value={row.key}
            onChange={(e) => updateRow(i, { key: e.target.value })}
            placeholder={keyPlaceholder}
            className={`flex-1 font-mono text-sm h-9 rounded-md ${
              !row.enabled ? "opacity-50" : ""
            }`}
          />

          {row.type === "file" ? (
            <div className="flex-[2] flex items-center gap-1.5">
              <label
                className={`flex flex-1 items-center gap-2 px-3 py-1.5 border border-input rounded-md cursor-pointer font-mono text-sm h-9 hover:bg-muted transition-colors ${
                  !row.enabled ? "opacity-50" : ""
                }`}
              >
                <FileUp className="size-3.5 text-muted-foreground shrink-0" />
                <span className="truncate text-foreground">
                  {row.fileName || "Choose file…"}
                </span>
                <input
                  type="file"
                  className="sr-only"
                  onChange={(e) => {
                    const file = e.target.files?.[0] ?? null;
                    if (file) {
                      registerFile(row.key || `row-${i}`, file);
                      updateRow(i, {
                        value: file.name,
                        fileName: file.name,
                      });
                    }
                  }}
                  disabled={!row.enabled}
                />
              </label>
              {allowFileType && (
                <Button
                  type="button"
                  variant="ghost"
                  size="icon-sm"
                  onClick={() => updateRow(i, { type: "text", value: "" })}
                  className="text-muted-foreground hover:text-foreground shrink-0"
                  title="Switch to text"
                >
                  <FileText className="size-3.5" />
                </Button>
              )}
            </div>
          ) : (
            <div className="flex-[2] flex items-center gap-1.5">
              <Input
                value={row.value}
                onChange={(e) => updateRow(i, { value: e.target.value })}
                placeholder={valuePlaceholder}
                className={`flex-1 font-mono text-sm h-9 rounded-md ${
                  !row.enabled ? "opacity-50" : ""
                }`}
              />
              {allowFileType && (
                <Button
                  type="button"
                  variant="ghost"
                  size="icon-sm"
                  onClick={() => {
                    updateRow(i, { type: "file", value: "", fileName: "" });
                  }}
                  className="text-muted-foreground hover:text-foreground shrink-0"
                  title="Switch to file"
                >
                  <FileUp className="size-3.5" />
                </Button>
              )}
            </div>
          )}

          <Button
            type="button"
            variant="ghost"
            size="icon-sm"
            onClick={() => removeRow(i)}
            className="opacity-0 group-hover:opacity-100 transition-opacity text-muted-foreground hover:text-destructive shrink-0"
          >
            <Trash2 className="size-3.5" />
          </Button>
        </div>
      ))}

      {editing ? (
        <div className="flex items-center gap-1.5">
          <Checkbox
            checked={draft.enabled}
            onCheckedChange={(checked) =>
              setDraft((d) => ({ ...d, enabled: checked === true }))
            }
            className="size-4 shrink-0"
          />
          <Input
            autoFocus
            value={draft.key}
            onChange={(e) => setDraft((d) => ({ ...d, key: e.target.value }))}
            placeholder={keyPlaceholder}
            className="flex-1 font-mono text-sm h-9 rounded-md"
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                commitDraft();
              }
              if (e.key === "Escape") {
                setEditing(false);
                setDraft({ key: "", value: "", enabled: true, type: "text" });
              }
            }}
          />

          {draft.type === "file" ? (
            <div className="flex-[2] flex items-center gap-1.5">
              <label
                className="flex flex-1 items-center gap-2 px-3 py-1.5 border border-input rounded-md cursor-pointer font-mono text-sm h-9 hover:bg-muted transition-colors"
              >
                <FileUp className="size-3.5 text-muted-foreground shrink-0" />
                <span className="truncate text-foreground">
                  {draft.fileName || "Choose file…"}
                </span>
                <input
                  ref={fileInputRef}
                  type="file"
                  className="sr-only"
                  onChange={(e) => {
                    const file = e.target.files?.[0] ?? null;
                    const key = draft.key.trim() || "file";
                    if (file) {
                      registerFile(key, file);
                      setDraft((d) => ({
                        ...d,
                        value: file.name,
                        fileName: file.name,
                      }));
                    }
                  }}
                />
              </label>
              {allowFileType && (
                <Button
                  type="button"
                  variant="ghost"
                  size="icon-sm"
                  onClick={() => setDraft((d) => ({ ...d, type: "text", value: "" }))}
                  className="text-muted-foreground hover:text-foreground shrink-0"
                  title="Switch to text"
                >
                  <FileText className="size-3.5" />
                </Button>
              )}
            </div>
          ) : (
            <div className="flex-[2] flex items-center gap-1.5">
              <Input
                value={draft.value}
                onChange={(e) =>
                  setDraft((d) => ({ ...d, value: e.target.value }))
                }
                placeholder={valuePlaceholder}
                className="flex-1 font-mono text-sm h-9 rounded-md"
                onKeyDown={(e) => {
                  if (e.key === "Enter") {
                    e.preventDefault();
                    commitDraft();
                  }
                  if (e.key === "Escape") {
                    setEditing(false);
                    setDraft({ key: "", value: "", enabled: true, type: "text" });
                  }
                }}
              />
              {allowFileType && (
                <Button
                  type="button"
                  variant="ghost"
                  size="icon-sm"
                  onClick={() => setDraft((d) => ({ ...d, type: "file", value: "" }))}
                  className="text-muted-foreground hover:text-foreground shrink-0"
                  title="Switch to file"
                >
                  <FileUp className="size-3.5" />
                </Button>
              )}
            </div>
          )}

          <Button
            type="button"
            variant="ghost"
            size="icon-sm"
            onClick={commitDraft}
            className="text-primary hover:text-primary/80 shrink-0"
          >
            <Plus className="size-3.5" />
          </Button>
        </div>
      ) : (
        <Button
          type="button"
          variant="ghost"
          size="sm"
          onClick={() => setEditing(true)}
          className="text-muted-foreground hover:text-foreground text-[10px] uppercase tracking-wider h-8"
        >
          <Plus className="size-3 mr-1" />
          Add
        </Button>
      )}
    </div>
  );
}

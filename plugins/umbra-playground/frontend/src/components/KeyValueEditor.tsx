import { useState } from "react";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Plus, Trash2 } from "lucide-react";
import type { KVItem } from "@/state/store";

interface KeyValueEditorProps {
  rows: KVItem[];
  onChange: (rows: KVItem[]) => void;
  keyPlaceholder?: string;
  valuePlaceholder?: string;
}

export function KeyValueEditor({
  rows,
  onChange,
  keyPlaceholder = "Key",
  valuePlaceholder = "Value",
}: KeyValueEditorProps) {
  const [draft, setDraft] = useState<KVItem>({
    key: "",
    value: "",
    enabled: true,
  });
  const [editing, setEditing] = useState(false);

  const updateRow = (index: number, patch: Partial<KVItem>) => {
    const next = rows.map((r, i) => (i === index ? { ...r, ...patch } : r));
    onChange(next);
  };

  const removeRow = (index: number) => {
    onChange(rows.filter((_, i) => i !== index));
  };

  const commitDraft = () => {
    if (!draft.key.trim() && !draft.value.trim()) {
      setEditing(false);
      setDraft({ key: "", value: "", enabled: true });
      return;
    }
    onChange([
      ...rows,
      { key: draft.key.trim(), value: draft.value.trim(), enabled: true },
    ]);
    setDraft({ key: "", value: "", enabled: true });
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
          <Input
            value={row.value}
            onChange={(e) => updateRow(i, { value: e.target.value })}
            placeholder={valuePlaceholder}
            className={`flex-[2] font-mono text-sm h-9 rounded-md ${
              !row.enabled ? "opacity-50" : ""
            }`}
          />
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
                setDraft({ key: "", value: "", enabled: true });
              }
            }}
          />
          <Input
            value={draft.value}
            onChange={(e) =>
              setDraft((d) => ({ ...d, value: e.target.value }))
            }
            placeholder={valuePlaceholder}
            className="flex-[2] font-mono text-sm h-9 rounded-md"
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                commitDraft();
              }
              if (e.key === "Escape") {
                setEditing(false);
                setDraft({ key: "", value: "", enabled: true });
              }
            }}
          />
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

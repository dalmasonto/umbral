import { useState } from "react";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import { Plus, Trash2 } from "lucide-react";

export interface KVRow {
  key: string;
  value: string;
}

interface KeyValueEditorProps {
  rows: KVRow[];
  onChange: (rows: KVRow[]) => void;
  keyPlaceholder?: string;
  valuePlaceholder?: string;
}

export function KeyValueEditor({
  rows,
  onChange,
  keyPlaceholder = "Key",
  valuePlaceholder = "Value",
}: KeyValueEditorProps) {
  const [draft, setDraft] = useState<KVRow>({ key: "", value: "" });
  const [editing, setEditing] = useState(false);

  const updateRow = (index: number, patch: Partial<KVRow>) => {
    const next = rows.map((r, i) => (i === index ? { ...r, ...patch } : r));
    onChange(next);
  };

  const removeRow = (index: number) => {
    onChange(rows.filter((_, i) => i !== index));
  };

  const commitDraft = () => {
    if (!draft.key.trim() && !draft.value.trim()) {
      setEditing(false);
      setDraft({ key: "", value: "" });
      return;
    }
    onChange([...rows, { key: draft.key.trim(), value: draft.value.trim() }]);
    setDraft({ key: "", value: "" });
    setEditing(false);
  };

  return (
    <div className="space-y-1.5">
      {rows.map((row, i) => (
        <div key={i} className="flex items-center gap-1.5 group">
          <Input
            value={row.key}
            onChange={(e) => updateRow(i, { key: e.target.value })}
            placeholder={keyPlaceholder}
            className="flex-1 font-mono text-xs h-7"
          />
          <Input
            value={row.value}
            onChange={(e) => updateRow(i, { value: e.target.value })}
            placeholder={valuePlaceholder}
            className="flex-[2] font-mono text-xs h-7"
          />
          <Button
            type="button"
            variant="ghost"
            size="icon-xs"
            onClick={() => removeRow(i)}
            className="opacity-0 group-hover:opacity-100 transition-opacity text-muted-foreground hover:text-destructive"
          >
            <Trash2 className="size-3" />
          </Button>
        </div>
      ))}

      {editing ? (
        <div className="flex items-center gap-1.5">
          <Input
            autoFocus
            value={draft.key}
            onChange={(e) => setDraft({ ...draft, key: e.target.value })}
            placeholder={keyPlaceholder}
            className="flex-1 font-mono text-xs h-7"
            onKeyDown={(e) => {
              if (e.key === "Enter") commitDraft();
              if (e.key === "Escape") {
                setEditing(false);
                setDraft({ key: "", value: "" });
              }
            }}
          />
          <Input
            value={draft.value}
            onChange={(e) => setDraft({ ...draft, value: e.target.value })}
            placeholder={valuePlaceholder}
            className="flex-[2] font-mono text-xs h-7"
            onKeyDown={(e) => {
              if (e.key === "Enter") commitDraft();
              if (e.key === "Escape") {
                setEditing(false);
                setDraft({ key: "", value: "" });
              }
            }}
          />
          <Button
            type="button"
            variant="ghost"
            size="icon-xs"
            onClick={commitDraft}
            className="text-primary hover:text-primary/80"
          >
            <Plus className="size-3" />
          </Button>
        </div>
      ) : (
        <Button
          type="button"
          variant="ghost"
          size="xs"
          onClick={() => setEditing(true)}
          className="text-muted-foreground hover:text-foreground text-[10px] uppercase tracking-wider"
        >
          <Plus className="size-3 mr-1" />
          Add
        </Button>
      )}
    </div>
  );
}

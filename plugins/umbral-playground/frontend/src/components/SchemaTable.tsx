import { Badge } from "@/components/ui/badge";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { Lock, Key, Link2, Tags } from "lucide-react";
import type { FieldInfo } from "@/lib/openapiSchema";

interface SchemaTableProps {
  fields: FieldInfo[];
  emptyLabel?: string;
}

function typeLabel(f: FieldInfo): string {
  if (f.refName) return f.refName;
  if (f.type === "array") {
    if (f.itemsRefName) return `${f.itemsRefName}[]`;
    if (f.itemsType) return `${f.itemsType}[]`;
    return "array";
  }
  if (f.format) return `${f.type} · ${f.format}`;
  return f.type;
}

export function SchemaTable({ fields, emptyLabel }: SchemaTableProps) {
  if (fields.length === 0) {
    return (
      <p className="px-2 py-3 text-xs italic text-muted-foreground">
        {emptyLabel ?? "No fields declared."}
      </p>
    );
  }

  return (
    <div className="overflow-hidden rounded-md border border-border">
      <Table>
        <TableHeader className="bg-muted/40">
          <TableRow className="hover:bg-muted/40">
            <TableHead className="w-[14rem]">Field</TableHead>
            <TableHead className="w-[10rem]">Type</TableHead>
            <TableHead>Constraints</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {fields.map((f) => (
            <TableRow key={f.name}>
              <TableCell className="font-mono text-xs font-medium text-foreground">
                <div className="flex items-center gap-1.5">
                  <span>{f.name}</span>
                  {f.isStringRepr && (
                    <span
                      title="Used as the model's display label"
                      className="text-muted-foreground"
                    >
                      <Key className="size-3" />
                    </span>
                  )}
                  {f.fkTarget && (
                    <span
                      title={`Foreign key → ${f.fkTarget}`}
                      className="text-muted-foreground"
                    >
                      <Link2 className="size-3" />
                    </span>
                  )}
                  {f.isMultichoice && (
                    <span
                      title="Multi-choice (CSV)"
                      className="text-muted-foreground"
                    >
                      <Tags className="size-3" />
                    </span>
                  )}
                  {f.readOnly && (
                    <span title="Read-only" className="text-muted-foreground">
                      <Lock className="size-3" />
                    </span>
                  )}
                </div>
                {f.description && (
                  <p className="mt-0.5 text-[10px] font-normal text-muted-foreground">
                    {f.description}
                  </p>
                )}
              </TableCell>
              <TableCell>
                <span className="font-mono text-[11px] text-foreground">
                  {typeLabel(f)}
                </span>
                {f.fkTarget && (
                  <p className="text-[10px] text-muted-foreground">
                    → {f.fkTarget}
                  </p>
                )}
              </TableCell>
              <TableCell>
                <div className="flex flex-wrap gap-1">
                  {f.required && (
                    <Badge
                      variant="outline"
                      className="h-4 rounded px-1 text-[9px] font-semibold uppercase tracking-wide text-rose-600 border-rose-500/30"
                    >
                      required
                    </Badge>
                  )}
                  {f.nullable && (
                    <Badge
                      variant="outline"
                      className="h-4 rounded px-1 text-[9px] font-semibold uppercase tracking-wide text-amber-600 border-amber-500/30"
                    >
                      nullable
                    </Badge>
                  )}
                  {f.maxLength !== undefined && f.maxLength > 0 && (
                    <Badge
                      variant="outline"
                      className="h-4 rounded px-1 font-mono text-[9px]"
                    >
                      max {f.maxLength}
                    </Badge>
                  )}
                  {f.defaultValue !== undefined && (
                    <Badge
                      variant="outline"
                      className="h-4 rounded px-1 font-mono text-[9px]"
                    >
                      default: {f.defaultValue}
                    </Badge>
                  )}
                  {f.enumValues && f.enumValues.length > 0 && (
                    <div className="flex w-full flex-wrap gap-1">
                      {f.enumValues.map((v, i) => (
                        <Badge
                          key={v}
                          variant="secondary"
                          className="h-4 rounded px-1.5 font-mono text-[9px]"
                          title={f.enumLabels?.[i]}
                        >
                          {f.enumLabels?.[i] ?? v}
                        </Badge>
                      ))}
                    </div>
                  )}
                  {f.isMultichoice && f.enumValues === undefined && (
                    <Badge
                      variant="secondary"
                      className="h-4 rounded px-1 font-mono text-[9px]"
                    >
                      multichoice (CSV)
                    </Badge>
                  )}
                </div>
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
    </div>
  );
}

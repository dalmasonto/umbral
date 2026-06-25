// gaps2 #17 — multi-select pickers for `?include=` and `?fields=`.
//
// Replaces the prior free-text `<Input>` for those two parameters with
// a popover-anchored checkbox picker driven by the OpenAPI spec data
// the playground already has in memory:
//
// - `?include=` → one checkbox per FK column on the resource (the FK
//   set comes from `fieldInfosFromSchema(listItem, spec)
//   .filter(f => f.fkTarget)`).
// - `?fields=` → one checkbox per column on the resource, plus a
//   nested sub-tree for each FK target's columns (resolved through
//   the OpenAPI components map). The nested sub-tree is gated on the
//   corresponding `?include=` entry being active — checking
//   `user.email` auto-enables `?include=user` so the response actually
//   carries that key.
//
// The picker emits a comma-joined string back to the existing param
// store; on entry it parses the same shape. Round-trip is lossless,
// so a user that types into `?include=` in the existing input first,
// then opens the picker, sees their values pre-checked.

import { useMemo, useState } from "react";
import type { OpenAPIV3 } from "openapi-types";
import { Check, ChevronDown, X } from "lucide-react";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import {
  type FieldInfo,
  fieldInfosFromSchema,
} from "@/lib/openapiSchema";

export interface IncludeFieldsPickerProps {
  /** "include" or "fields" — controls which picker shape to render. */
  paramName: "include" | "fields";
  /** Current comma-joined value from the param store. */
  value: string;
  /** Emit a new comma-joined value back to the param store. */
  onChange: (next: string) => void;
  /**
   * The list-item schema for the current operation. For collection
   * GETs that's the array element; for detail GETs the response
   * object itself. The picker reads its FK list and column list
   * from this.
   */
  listItem: OpenAPIV3.SchemaObject | null;
  /**
   * Full OpenAPI document — needed to resolve `$ref`s when an FK
   * field's target lives in `components.schemas.<X>`.
   */
  spec: OpenAPIV3.Document | null;
  /**
   * When the picker is `?fields=`, the user's current `?include=`
   * selection drives which nested sub-trees are clickable. Pass the
   * `?include=` param's comma-joined value here. Used only for the
   * "fields" variant; the "include" variant ignores it.
   */
  includeValue: string;
  /**
   * Called when the "fields" picker needs to auto-enable an include
   * entry (e.g. user ticks `user.email` without having `user` in
   * `?include=` yet). Pass the new comma-joined include value.
   */
  onIncludeChange: (next: string) => void;
}

function parseTokens(value: string): Set<string> {
  return new Set(
    value
      .split(",")
      .map((t) => t.trim())
      .filter((t) => t.length > 0),
  );
}

function serializeTokens(tokens: Iterable<string>): string {
  const list = Array.from(tokens).filter((t) => t.length > 0);
  list.sort();
  return list.join(",");
}

export function IncludeFieldsPicker(props: IncludeFieldsPickerProps) {
  const {
    paramName,
    value,
    onChange,
    listItem,
    spec,
    includeValue,
    onIncludeChange,
  } = props;
  const [open, setOpen] = useState(false);
  const [filter, setFilter] = useState("");

  // Resolve the resource's full field list (once per spec change).
  const fields = useMemo<FieldInfo[]>(
    () => fieldInfosFromSchema(listItem, spec),
    [listItem, spec],
  );

  // FK fields on the resource — drives the `?include=` picker AND the
  // sub-tree gating in the `?fields=` picker.
  const fkFields = useMemo(() => fields.filter((f) => !!f.fkTarget), [fields]);

  const selected = useMemo(() => parseTokens(value), [value]);
  const activeIncludes = useMemo(() => parseTokens(includeValue), [includeValue]);

  // For the fields picker, build the nested sub-trees lazily. Keyed
  // by FK name → its target's FieldInfo[]. Returns an empty array
  // when the FK target's schema isn't in the spec's components.
  const fkChildFields = useMemo(() => {
    const out = new Map<string, FieldInfo[]>();
    if (paramName !== "fields") return out;
    for (const fk of fkFields) {
      if (!fk.fkTarget) continue;
      const targetSchema = spec?.components?.schemas?.[fk.fkTarget];
      if (!targetSchema) {
        out.set(fk.name, []);
        continue;
      }
      out.set(fk.name, fieldInfosFromSchema(targetSchema, spec));
    }
    return out;
  }, [paramName, fkFields, spec]);

  const toggle = (token: string) => {
    const next = new Set(selected);
    if (next.has(token)) {
      next.delete(token);
    } else {
      next.add(token);
    }
    onChange(serializeTokens(next));
  };

  // For the fields picker: ticking a dotted token (`user.email`) also
  // enables the matching include entry (`user`). Avoids the foot-gun
  // where a user picks nested columns and the response still drops
  // them because the parent FK wasn't expanded.
  const toggleDotted = (token: string) => {
    toggle(token);
    if (paramName !== "fields") return;
    const dot = token.indexOf(".");
    if (dot < 0) return;
    const parent = token.slice(0, dot);
    if (activeIncludes.has(parent)) return;
    const nextInclude = new Set(activeIncludes);
    nextInclude.add(parent);
    onIncludeChange(serializeTokens(nextInclude));
  };

  const filteredFields = useMemo(() => {
    const q = filter.trim().toLowerCase();
    if (!q) return fields;
    return fields.filter((f) => f.name.toLowerCase().includes(q));
  }, [fields, filter]);

  const filteredFkFields = useMemo(() => {
    const q = filter.trim().toLowerCase();
    if (!q) return fkFields;
    return fkFields.filter((f) => f.name.toLowerCase().includes(q));
  }, [fkFields, filter]);

  // Source-of-truth schema isn't loaded yet (no operation selected,
  // spec still fetching). Fall back to a raw input so the user can
  // still type values rather than seeing a blank greyed-out button.
  if (fields.length === 0) {
    return (
      <Input
        value={value}
        onChange={(e) => onChange(e.target.value)}
        placeholder={`${paramName}=…`}
        className="h-7 text-xs font-mono rounded-md flex-1 min-w-0"
      />
    );
  }

  const selectedCount = selected.size;
  const buttonLabel =
    selectedCount === 0
      ? `Pick ${paramName}…`
      : selectedCount === 1
        ? Array.from(selected)[0]
        : `${selectedCount} selected`;

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <Button
          type="button"
          variant="outline"
          size="sm"
          className="h-7 flex-1 min-w-0 justify-between px-2 font-mono text-xs"
          title={value || `Pick ${paramName}…`}
        >
          <span className="truncate">{buttonLabel}</span>
          <div className="flex items-center gap-1 shrink-0">
            {selectedCount > 0 && (
              <button
                type="button"
                onClick={(e) => {
                  e.stopPropagation();
                  onChange("");
                }}
                className="rounded p-0.5 text-muted-foreground hover:bg-muted hover:text-foreground"
                title={`Clear all ${paramName} selections`}
              >
                <X className="size-3" />
              </button>
            )}
            <ChevronDown className="size-3 text-muted-foreground" />
          </div>
        </Button>
      </PopoverTrigger>
      <PopoverContent align="start" className="w-80 p-0">
        <div className="border-b border-border p-2">
          <Input
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            placeholder="Filter…"
            className="h-7 text-xs"
          />
        </div>
        <div className="max-h-72 overflow-y-auto p-2 space-y-1">
          {paramName === "include" ? (
            <IncludePickerBody
              fkFields={filteredFkFields}
              selected={selected}
              onToggle={toggle}
            />
          ) : (
            <FieldsPickerBody
              fields={filteredFields}
              selected={selected}
              onToggle={toggle}
              fkChildFields={fkChildFields}
              activeIncludes={activeIncludes}
              onToggleDotted={toggleDotted}
            />
          )}
        </div>
      </PopoverContent>
    </Popover>
  );
}

function IncludePickerBody(props: {
  fkFields: FieldInfo[];
  selected: Set<string>;
  onToggle: (token: string) => void;
}) {
  const { fkFields, selected, onToggle } = props;
  if (fkFields.length === 0) {
    return (
      <p className="px-2 py-3 text-[11px] text-muted-foreground">
        No FK columns on this resource. `?include=` requires foreign-key
        fields to expand — none are declared in the spec.
      </p>
    );
  }
  return (
    <>
      {fkFields.map((f) => {
        const checked = selected.has(f.name);
        return (
          <label
            key={f.name}
            className="flex items-center gap-2 rounded-md px-2 py-1.5 text-xs font-mono hover:bg-muted cursor-pointer"
          >
            <Checkbox
              checked={checked}
              onCheckedChange={() => onToggle(f.name)}
            />
            <span className="truncate flex-1">{f.name}</span>
            {f.fkTarget && (
              <Badge
                variant="secondary"
                className="h-4 rounded px-1 text-[9px] font-mono"
              >
                → {f.fkTarget}
              </Badge>
            )}
          </label>
        );
      })}
    </>
  );
}

function FieldsPickerBody(props: {
  fields: FieldInfo[];
  selected: Set<string>;
  onToggle: (token: string) => void;
  fkChildFields: Map<string, FieldInfo[]>;
  activeIncludes: Set<string>;
  onToggleDotted: (token: string) => void;
}) {
  const {
    fields,
    selected,
    onToggle,
    fkChildFields,
    activeIncludes,
    onToggleDotted,
  } = props;
  if (fields.length === 0) {
    return (
      <p className="px-2 py-3 text-[11px] text-muted-foreground">
        No columns on this resource. Spec is empty.
      </p>
    );
  }
  return (
    <>
      {fields.map((f) => {
        const checked = selected.has(f.name);
        const children = f.fkTarget ? fkChildFields.get(f.name) : undefined;
        const childIsActive = activeIncludes.has(f.name);
        return (
          <div key={f.name} className="space-y-0.5">
            <label className="flex items-center gap-2 rounded-md px-2 py-1.5 text-xs font-mono hover:bg-muted cursor-pointer">
              <Checkbox
                checked={checked}
                onCheckedChange={() => onToggle(f.name)}
              />
              <span className="truncate flex-1">{f.name}</span>
              {f.fkTarget && (
                <Badge
                  variant="secondary"
                  className="h-4 rounded px-1 text-[9px] font-mono"
                >
                  → {f.fkTarget}
                </Badge>
              )}
            </label>
            {children && children.length > 0 && (
              <div className="ml-7 border-l border-border pl-2 space-y-0.5">
                <p
                  className="text-[10px] text-muted-foreground italic"
                  title={
                    childIsActive
                      ? `Already in ?include=${f.name}`
                      : `Will auto-enable ?include=${f.name} on click`
                  }
                >
                  {childIsActive ? (
                    <span className="inline-flex items-center gap-1">
                      <Check className="size-2.5" />
                      include={f.name}
                    </span>
                  ) : (
                    <span>Pick to auto-include {f.name}</span>
                  )}
                </p>
                {children.map((sub) => {
                  const token = `${f.name}.${sub.name}`;
                  const subChecked = selected.has(token);
                  return (
                    <label
                      key={token}
                      className="flex items-center gap-2 rounded-md px-2 py-1 text-[11px] font-mono hover:bg-muted cursor-pointer"
                    >
                      <Checkbox
                        checked={subChecked}
                        onCheckedChange={() => onToggleDotted(token)}
                      />
                      <span className="truncate flex-1">{sub.name}</span>
                    </label>
                  );
                })}
              </div>
            )}
          </div>
        );
      })}
    </>
  );
}

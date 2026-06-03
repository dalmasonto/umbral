import { useState } from "react";
import { ChevronRight, ChevronDown } from "lucide-react";

interface JsonViewProps {
  data: unknown;
  level?: number;
  collapsed?: boolean;
}

function isExpandable(v: unknown): boolean {
  return v !== null && (typeof v === "object" || Array.isArray(v));
}

function countEntries(v: unknown): number {
  if (v === null) return 0;
  if (Array.isArray(v)) return v.length;
  if (typeof v === "object") return Object.keys(v).length;
  return 0;
}

export function JsonView({ data, level = 0, collapsed = false }: JsonViewProps) {
  const [isOpen, setIsOpen] = useState(!collapsed);

  const indent = "  ".repeat(level);
  const innerIndent = "  ".repeat(level + 1);

  if (data === null) return <span className="text-muted-foreground">null</span>;
  if (typeof data === "boolean")
    return <span className="text-violet-600 font-semibold">{data ? "true" : "false"}</span>;
  if (typeof data === "number") return <span className="text-sky-600 font-semibold">{data}</span>;
  if (typeof data === "string")
    return (
      <span className="text-emerald-600">
        &quot;{data}&quot;
      </span>
    );

  if (Array.isArray(data)) {
    if (data.length === 0) return <span className="text-muted-foreground">[]</span>;
    return (
      <span>
        {isExpandable(data) ? (
          <button
            type="button"
            onClick={() => setIsOpen(!isOpen)}
            className="inline-flex items-center gap-0.5 text-muted-foreground hover:text-foreground transition-colors"
          >
            {isOpen ? <ChevronDown className="size-3" /> : <ChevronRight className="size-3" />}
          </button>
        ) : null}
        <span className="text-muted-foreground">{isOpen ? "[" : `[…] ${countEntries(data)} items`}</span>
        {isOpen && (
          <>
            {data.map((item, i) => (
              <div key={i} className="block">
                {innerIndent}
                <JsonView data={item} level={level + 1} collapsed={level >= 2} />
                {i < data.length - 1 ? "," : ""}
              </div>
            ))}
            {indent}]
          </>
        )}
      </span>
    );
  }

  const obj = data as Record<string, unknown>;
  const keys = Object.keys(obj);
  if (keys.length === 0) return <span className="text-muted-foreground">{"{}"}</span>;

  return (
    <span>
      <button
        type="button"
        onClick={() => setIsOpen(!isOpen)}
        className="inline-flex items-center gap-0.5 text-muted-foreground hover:text-foreground transition-colors"
      >
        {isOpen ? <ChevronDown className="size-3" /> : <ChevronRight className="size-3" />}
      </button>
      <span className="text-muted-foreground">{isOpen ? "{" : `{…} ${countEntries(obj)} keys`}</span>
      {isOpen && (
        <>
          {keys.map((key, i) => (
            <div key={key} className="block">
              {innerIndent}
              <span className="text-foreground font-medium">{key}</span>
              <span className="text-muted-foreground">: </span>
              <JsonView data={obj[key]} level={level + 1} collapsed={level >= 1} />
              {i < keys.length - 1 ? "," : ""}
            </div>
          ))}
          {indent}{"}"}
        </>
      )}
    </span>
  );
}

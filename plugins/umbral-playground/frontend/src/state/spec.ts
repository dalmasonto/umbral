import type { OpenAPIV3 } from "openapi-types";

/** An operation with its parent path and method, for the tree. */
export interface TreeEntry {
  operationId: string;
  method: string;
  path: string;
  summary?: string;
  tag: string;
}

/** Walk a spec and produce a flat list of operations, grouped by tag. */
export function listOperations(spec: OpenAPIV3.Document): TreeEntry[] {
  const out: TreeEntry[] = [];
  for (const [path, pathItem] of Object.entries(spec.paths ?? {})) {
    if (!pathItem) continue;
    const methods: Array<[string, OpenAPIV3.OperationObject | undefined]> = [
      ["GET", pathItem.get],
      ["POST", pathItem.post],
      ["PUT", pathItem.put],
      ["PATCH", pathItem.patch],
      ["DELETE", pathItem.delete],
    ];
    for (const [method, op] of methods) {
      if (!op) continue;
      const operationId = op.operationId ?? `${method} ${path}`;
      const tag = op.tags?.[0] ?? "default";
      out.push({
        operationId,
        method,
        path,
        summary: op.summary,
        tag,
      });
    }
  }
  return out;
}

/** Find an operation by id, or null. */
export function findOperation(
  spec: OpenAPIV3.Document,
  operationId: string | null,
): { method: string; path: string; op: OpenAPIV3.OperationObject } | null {
  if (!operationId) return null;
  for (const [path, pathItem] of Object.entries(spec.paths ?? {})) {
    if (!pathItem) continue;
    const methods: Array<[string, OpenAPIV3.OperationObject | undefined]> = [
      ["GET", pathItem.get],
      ["POST", pathItem.post],
      ["PUT", pathItem.put],
      ["PATCH", pathItem.patch],
      ["DELETE", pathItem.delete],
    ];
    for (const [method, op] of methods) {
      if (!op) continue;
      const id = op.operationId ?? `${method} ${path}`;
      if (id === operationId) {
        return { method, path, op };
      }
    }
  }
  return null;
}

import type { OpenAPIV3 } from "openapi-types";

export interface FieldInfo {
  name: string;
  type: string;
  format?: string;
  required: boolean;
  nullable: boolean;
  readOnly: boolean;
  enumValues?: string[];
  enumLabels?: string[];
  maxLength?: number;
  defaultValue?: string;
  isMultichoice?: boolean;
  fkTarget?: string;
  isStringRepr?: boolean;
  description?: string;
  refName?: string;
  itemsType?: string;
  itemsRefName?: string;
}

type SchemaLike =
  | OpenAPIV3.SchemaObject
  | OpenAPIV3.ReferenceObject
  | undefined
  | null;

const REF_PREFIX = "#/components/schemas/";

export function refName(ref: string | undefined): string | undefined {
  if (!ref) return undefined;
  if (ref.startsWith(REF_PREFIX)) return ref.slice(REF_PREFIX.length);
  const parts = ref.split("/");
  return parts[parts.length - 1] || undefined;
}

export function resolveRef(
  schema: SchemaLike,
  doc: OpenAPIV3.Document | null,
): { schema: OpenAPIV3.SchemaObject | null; name?: string } {
  if (!schema || !doc) return { schema: null };
  if ("$ref" in schema) {
    const name = refName(schema.$ref);
    if (!name) return { schema: null };
    const target = doc.components?.schemas?.[name];
    if (!target) return { schema: null, name };
    if ("$ref" in target) return resolveRef(target, doc);
    return { schema: target, name };
  }
  return { schema };
}

interface VendorExt {
  "x-umbral-multichoice"?: boolean;
  "x-umbral-choices"?: string[];
  "x-umbral-choice-labels"?: string[];
  "x-umbral-fk-target"?: string;
  "x-umbral-string-repr"?: boolean;
}

function vendorExt(s: OpenAPIV3.SchemaObject): VendorExt {
  return s as unknown as VendorExt;
}

export function fieldInfosFromSchema(
  schema: SchemaLike,
  doc: OpenAPIV3.Document | null,
): FieldInfo[] {
  const { schema: resolved } = resolveRef(schema, doc);
  if (!resolved || !resolved.properties) return [];
  const required = new Set(resolved.required ?? []);
  const out: FieldInfo[] = [];
  for (const [name, prop] of Object.entries(resolved.properties)) {
    out.push(propToInfo(name, prop, required.has(name), doc));
  }
  return out;
}

function propToInfo(
  name: string,
  prop: OpenAPIV3.SchemaObject | OpenAPIV3.ReferenceObject,
  required: boolean,
  doc: OpenAPIV3.Document | null,
): FieldInfo {
  if ("$ref" in prop) {
    const { schema: target, name: targetName } = resolveRef(prop, doc);
    return {
      name,
      type: targetName ? `→ ${targetName}` : "object",
      required,
      nullable: false,
      readOnly: false,
      refName: targetName,
      description: target?.description,
    };
  }
  const ext = vendorExt(prop);
  const enumValues = prop.enum?.map(String);
  const itemsRef =
    prop.type === "array" && prop.items && "$ref" in prop.items
      ? refName(prop.items.$ref)
      : undefined;
  const itemsType =
    prop.type === "array" && prop.items && !("$ref" in prop.items)
      ? String(prop.items.type ?? "any")
      : undefined;
  return {
    name,
    type: String(prop.type ?? "any"),
    format: prop.format,
    required,
    nullable: prop.nullable === true,
    readOnly: prop.readOnly === true,
    enumValues,
    enumLabels: ext["x-umbral-choice-labels"],
    maxLength: prop.maxLength,
    defaultValue:
      prop.default !== undefined && prop.default !== null
        ? String(prop.default)
        : undefined,
    isMultichoice: ext["x-umbral-multichoice"] === true,
    fkTarget: ext["x-umbral-fk-target"],
    isStringRepr: ext["x-umbral-string-repr"] === true,
    description: prop.description,
    itemsType,
    itemsRefName: itemsRef,
  };
}

export interface ResponseSchemaEntry {
  status: string;
  description?: string;
  contentType?: string;
  schema?: OpenAPIV3.SchemaObject | OpenAPIV3.ReferenceObject;
  resolved: OpenAPIV3.SchemaObject | null;
  resolvedName?: string;
}

export function responseSchemaEntries(
  operation: OpenAPIV3.OperationObject | undefined,
  doc: OpenAPIV3.Document | null,
): ResponseSchemaEntry[] {
  if (!operation?.responses) return [];
  const entries: ResponseSchemaEntry[] = [];
  for (const [status, response] of Object.entries(operation.responses)) {
    if (!response || "$ref" in response) {
      entries.push({ status, resolved: null });
      continue;
    }
    const contentEntries = Object.entries(response.content ?? {});
    if (contentEntries.length === 0) {
      entries.push({
        status,
        description: response.description,
        resolved: null,
      });
      continue;
    }
    for (const [contentType, media] of contentEntries) {
      const { schema, name } = resolveRef(media.schema, doc);
      entries.push({
        status,
        description: response.description,
        contentType,
        schema: media.schema,
        resolved: schema,
        resolvedName: name,
      });
    }
  }
  return entries.sort((a, b) => a.status.localeCompare(b.status));
}

export function requestBodySchema(
  operation: OpenAPIV3.OperationObject | undefined,
  doc: OpenAPIV3.Document | null,
): {
  schema: OpenAPIV3.SchemaObject | null;
  name?: string;
  contentType?: string;
  required: boolean;
} | null {
  const body = operation?.requestBody;
  if (!body) return null;
  if ("$ref" in body) {
    // Request body refs are uncommon; treat as not-found for now.
    return null;
  }
  const required = body.required === true;
  const contentEntries = Object.entries(body.content ?? {});
  if (contentEntries.length === 0) return null;
  const [contentType, media] = contentEntries[0];
  const { schema, name } = resolveRef(media.schema, doc);
  return { schema, name, contentType, required };
}

interface SynthesizeOptions {
  /** Include every field, not just required ones. Default false. */
  allFields?: boolean;
  /** Include `readOnly` fields. Default false — those are server-
   *  generated (created_at, computed columns, etc.) and including
   *  them in a request body usually triggers a 4xx. */
  includeReadOnly?: boolean;
}

/** Pick a sensible placeholder value for one field. Honors:
 *  - explicit `default` from the OpenAPI schema (parsed as JSON if it
 *    looks like a literal, else kept as a string);
 *  - first enum value when there's an `enum`;
 *  - nullable → `null`;
 *  - format-driven hints for common string formats
 *    (`date`, `date-time`, `uuid`, `email`, …);
 *  - bare type defaults otherwise. */
function placeholderForField(f: FieldInfo): unknown {
  if (f.defaultValue !== undefined && f.defaultValue !== "") {
    try {
      return JSON.parse(f.defaultValue);
    } catch {
      return f.defaultValue;
    }
  }
  if (f.enumValues && f.enumValues.length > 0) {
    return f.enumValues[0];
  }
  if (f.nullable) {
    return null;
  }
  switch (f.type) {
    case "string":
      switch (f.format) {
        case "date":
          return "2026-01-01";
        case "date-time":
          return "2026-01-01T00:00:00Z";
        case "time":
          return "00:00:00";
        case "uuid":
          return "00000000-0000-0000-0000-000000000000";
        case "email":
          return "user@example.com";
        case "uri":
        case "url":
          return "https://example.com";
        default:
          return "";
      }
    case "integer":
      return 0;
    case "number":
      return 0;
    case "boolean":
      return false;
    case "array":
      return [];
    case "object":
      return {};
    default:
      return null;
  }
}

/** Build a pretty-printed JSON request body skeleton from a list of
 *  `FieldInfo`s. Used by the Schema tab's autofill buttons in
 *  `RequestBuilder` — "required only" gives the minimum payload the
 *  server will accept; "all fields" shows every key the schema
 *  documents. `readOnly` fields are skipped unless the caller
 *  explicitly opts in (server fills them — sending them is usually a
 *  4xx). */
export function synthesizeJsonBody(
  fields: FieldInfo[],
  options: SynthesizeOptions = {},
): string {
  const { allFields = false, includeReadOnly = false } = options;
  const obj: Record<string, unknown> = {};
  for (const f of fields) {
    if (!allFields && !f.required) continue;
    if (!includeReadOnly && f.readOnly) continue;
    obj[f.name] = placeholderForField(f);
  }
  return JSON.stringify(obj, null, 2);
}

/** Gap 86 — form-body counterpart of [`synthesizeJsonBody`]. Walks the
 *  same `FieldInfo[]` and produces `{ key, value, enabled, type }`
 *  rows the playground's `KeyValueEditor` consumes. Default behaviour
 *  matches the JSON skeleton: required-only by default, skipping
 *  `readOnly` columns. Each value is the same placeholder
 *  `placeholderForField` produces, JSON-stringified when the type
 *  isn't already a string (so a `boolean` field renders as "false"
 *  in the form input rather than as nothing). */
export function synthesizeFormFields(
  fields: FieldInfo[],
  options: SynthesizeOptions = {},
): { key: string; value: string; enabled: boolean; type: "text" }[] {
  const { allFields = false, includeReadOnly = false } = options;
  const out: { key: string; value: string; enabled: boolean; type: "text" }[] = [];
  for (const f of fields) {
    if (!allFields && !f.required) continue;
    if (!includeReadOnly && f.readOnly) continue;
    const raw = placeholderForField(f);
    let value: string;
    if (raw === null) value = "";
    else if (typeof raw === "string") value = raw;
    else value = JSON.stringify(raw);
    out.push({ key: f.name, value, enabled: true, type: "text" });
  }
  return out;
}

/** True when the operation looks like a list-collection endpoint
 *  whose response item is a single schema we can introspect. Used to
 *  decide whether to offer filter affordances in the params tab. */
export function listItemSchema(
  operation: OpenAPIV3.OperationObject | undefined,
  doc: OpenAPIV3.Document | null,
): { schema: OpenAPIV3.SchemaObject; name?: string } | null {
  if (!operation) return null;
  const ok = operation.responses?.["200"];
  if (!ok || "$ref" in ok) return null;
  const media = ok.content?.["application/json"];
  if (!media) return null;
  const { schema: envelope } = resolveRef(media.schema, doc);
  if (!envelope || !envelope.properties) return null;
  // Recognize {results: [Item], count} envelope.
  const results = envelope.properties.results as
    | OpenAPIV3.SchemaObject
    | OpenAPIV3.ReferenceObject
    | undefined;
  if (!results || "$ref" in results) return null;
  if (results.type !== "array" || !results.items) return null;
  const { schema: item, name } = resolveRef(results.items, doc);
  if (!item) return null;
  return { schema: item, name };
}

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
  "x-umbra-multichoice"?: boolean;
  "x-umbra-choices"?: string[];
  "x-umbra-choice-labels"?: string[];
  "x-umbra-fk-target"?: string;
  "x-umbra-string-repr"?: boolean;
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
    enumLabels: ext["x-umbra-choice-labels"],
    maxLength: prop.maxLength,
    defaultValue:
      prop.default !== undefined && prop.default !== null
        ? String(prop.default)
        : undefined,
    isMultichoice: ext["x-umbra-multichoice"] === true,
    fkTarget: ext["x-umbra-fk-target"],
    isStringRepr: ext["x-umbra-string-repr"] === true,
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

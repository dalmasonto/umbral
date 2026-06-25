import Dexie, { type EntityTable } from "dexie";
import { getAppScope } from "./scope";
import type { PlaygroundSettings, RequestDraft, ResponseRecord, Tab } from "./store";

export interface HistoryRow extends ResponseRecord {
  /** Dexie auto-increment PK. Optional on the type because we never
   *  set it on insert — bulkAdd / put assigns it. */
  id?: number;
}

/** Per-key UI state slot. Keyed by a stable string the consumer
 *  picks (e.g. `"endpoint-tree.search"`). Value is whatever JSON the
 *  consumer needs to round-trip — Dexie serialises with
 *  structuredClone so non-primitive values (Records, arrays) work
 *  without us writing a JSON shape per slot. */
export interface EditorStateRow {
  /** The slot key. Primary. Convention: `<component>.<field>`. */
  key: string;
  /** Whatever value the consumer wants persisted. */
  value: unknown;
}

/** Workspace-settings row. Singleton — we always use the key
 *  `"workspace"`. Schema-versioned: bumping the `schema` field lets
 *  us evolve `PlaygroundSettings` shape over time without re-reading
 *  garbage. */
export interface SettingsRow {
  key: string;
  schema: number;
  value: PlaygroundSettings;
  updatedAt: number;
}

/** Per-operation request-draft row. Keyed by `operationId` so each
 *  endpoint remembers its last in-progress state across reloads.
 *  Stores the entire `RequestDraft` shape — method, URL, params,
 *  headers, body, form fields, auth — so the user's work is never
 *  lost to a tab close. Schema-versioned alongside the settings
 *  row for the same forward-compat reason. */
export interface DraftRow {
  operationId: string;
  schema: number;
  draft: RequestDraft;
  updatedAt: number;
}

// Per-app Dexie database name (gap #71). Two apps in the same
// browser get two separate IndexedDB databases — history /
// editorState / settings slots can't bleed across app boundaries.
const DB_NAME = `umbral-playground:${getAppScope()}`;

export const db = new Dexie(DB_NAME) as Dexie & {
  history: EntityTable<HistoryRow, "id">;
  editorState: EntityTable<EditorStateRow, "key">;
  settings: EntityTable<SettingsRow, "key">;
  drafts: EntityTable<DraftRow, "operationId">;
  tabs: EntityTable<TabsRow, "key">;
};

// v1: history table. v2 adds the editorState table for persistent
// UI slots (expanded sidebar groups, search input, active tabs).
// v3 adds the settings table (workspace settings, previously stored
// in localStorage — moved to IndexedDB so the 5MB quota goes away
// and the unreliable-private-mode-throws localStorage edge cases
// stop biting us). v4 adds the drafts table — per-operation
// request state survives reloads so the user never loses an
// in-progress payload. Upgrades are automatic; Dexie keeps the
// existing tables untouched and just adds the new one.
db.version(1).stores({
  history: "++id, operationId, timestamp",
});

db.version(2).stores({
  history: "++id, operationId, timestamp",
  editorState: "&key",
});

db.version(3).stores({
  history: "++id, operationId, timestamp",
  editorState: "&key",
  settings: "&key",
});

db.version(4).stores({
  history: "++id, operationId, timestamp",
  editorState: "&key",
  settings: "&key",
  drafts: "&operationId, updatedAt",
});

// v5 adds the tabs table — singleton row keyed `"workspace"`,
// holding the open tab list and the active tab id. The previous
// v4 tables are kept verbatim so Dexie doesn't drop the user's
// data on upgrade.
db.version(5).stores({
  history: "++id, operationId, timestamp",
  editorState: "&key",
  settings: "&key",
  drafts: "&operationId, updatedAt",
  tabs: "&key",
});

/** Schema version stamped on every `SettingsRow`. Bump alongside any
 *  breaking change to `PlaygroundSettings`. Reads with a lower
 *  version go through the normaliser to fill in missing fields;
 *  reads with a higher version (downgrade) are discarded back to
 *  defaults so we don't pretend to understand them. */
export const SETTINGS_SCHEMA_VERSION = 1;

/** Schema version stamped on every `DraftRow`. See
 *  [`SETTINGS_SCHEMA_VERSION`] for the upgrade convention. */
export const DRAFT_SCHEMA_VERSION = 1;

/** Per-app workspace tabs. One singleton row per app DB, keyed
 *  `"workspace"`, holding the list of open tabs and the id of
 *  the active one. Mirrors the `settings` table shape — same
 *  schema-versioning + per-app DB isolation story. The tabs row
 *  is rewritten whenever the list of open tabs changes or the
 *  pristine snapshot of a tab's draft is updated. It is NOT
 *  rewritten on every keystroke — the per-endpoint draft lives
 *  in the `drafts` table. */
export interface TabsRow {
  /** Singleton key. Always `"workspace"`. */
  key: "workspace";
  schema: number;
  tabs: Tab[];
  activeTabId: string | null;
  updatedAt: number;
}

/** Schema version stamped on every `TabsRow`. Bump alongside
 *  any breaking change to the `Tab` shape. Reads with a higher
 *  version are discarded back to defaults so we don't pretend
 *  to understand them. */
export const TABS_SCHEMA_VERSION = 1;

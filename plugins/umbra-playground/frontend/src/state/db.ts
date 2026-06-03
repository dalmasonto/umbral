import Dexie, { type EntityTable } from "dexie";
import type { ResponseRecord } from "./store";

export interface HistoryRow extends ResponseRecord {
  /** Dexie auto-increment PK. Optional on the type because we never
   *  set it on insert — bulkAdd / put assigns it. */
  id?: number;
}

const DB_NAME = "umbra-playground";

export const db = new Dexie(DB_NAME) as Dexie & {
  history: EntityTable<HistoryRow, "id">;
};

// One indexed table, three indexed columns.
//   ++id        — auto-increment primary key
//   operationId — fast filter for "history for this endpoint"
//   timestamp   — chronological ordering / range pruning
db.version(1).stores({
  history: "++id, operationId, timestamp",
});

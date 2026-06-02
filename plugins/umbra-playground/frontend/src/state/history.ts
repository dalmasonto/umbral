import type { ResponseRecord } from "./store";

const STORAGE_KEY = "umbra-playground:history:v1";
const PER_OPERATION_CAP = 50;
const TOTAL_BYTE_CAP = 5 * 1024 * 1024; // 5MB

export function loadHistory(): Record<string, ResponseRecord[]> {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return {};
    return JSON.parse(raw) as Record<string, ResponseRecord[]>;
  } catch {
    return {};
  }
}

let saveTimer: ReturnType<typeof setTimeout> | null = null;

export function saveHistoryDebounced(
  history: Record<string, ResponseRecord[]>,
): void {
  if (saveTimer) clearTimeout(saveTimer);
  saveTimer = setTimeout(() => {
    const trimmed = enforceCaps(history);
    try {
      localStorage.setItem(STORAGE_KEY, JSON.stringify(trimmed));
    } catch {
      // localStorage full or disabled; silently drop. The in-memory
      // history still works for the current session.
    }
  }, 500);
}

function enforceCaps(
  history: Record<string, ResponseRecord[]>,
): Record<string, ResponseRecord[]> {
  // Per-operation cap.
  const out: Record<string, ResponseRecord[]> = {};
  for (const [k, v] of Object.entries(history)) {
    out[k] = v.slice(-PER_OPERATION_CAP);
  }
  // Total byte cap.
  let serialized = JSON.stringify(out);
  if (serialized.length <= TOTAL_BYTE_CAP) return out;

  // Drop oldest across all operations until under cap.
  const allEntries: Array<[string, ResponseRecord, number]> = [];
  for (const [opId, records] of Object.entries(out)) {
    for (let i = 0; i < records.length; i++) {
      allEntries.push([opId, records[i], records[i].timestamp]);
    }
  }
  allEntries.sort((a, b) => a[2] - b[2]);
  while (serialized.length > TOTAL_BYTE_CAP && allEntries.length > 0) {
    const [opId, record] = allEntries.shift()!;
    out[opId] = out[opId].filter((r) => r !== record);
    serialized = JSON.stringify(out);
  }
  return out;
}

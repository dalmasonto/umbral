/** In-memory registry for File objects selected in the form body.
 *
 * File objects cannot live in Zustand state (not serializable for history
 * or localStorage), so we keep them in this module-level Map keyed by
 * `${fieldKey}-${index}`. The KVItem stores `type`, `fileName`, and
 * `value` (the display name) while the actual File lives here.
 */
const fileRegistry = new Map<string, File>();

export function registerFile(key: string, file: File | null): void {
  if (file) fileRegistry.set(key, file);
  else fileRegistry.delete(key);
}

export function getFile(key: string): File | undefined {
  return fileRegistry.get(key);
}

export function getAllFormFiles(): Map<string, File> {
  return new Map(fileRegistry);
}

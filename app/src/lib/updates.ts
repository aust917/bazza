export interface AvailableUpdate {
  version: string;
  notes: string | null;
  install: (onProgress?: (done: number, total: number | null) => void) => Promise<void>;
}

/** Local builds have no distribution channel or updater signing identity. */
export async function checkForUpdate(): Promise<AvailableUpdate | null> {
  return null;
}

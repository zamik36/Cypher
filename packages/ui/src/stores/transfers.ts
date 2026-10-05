import { createStore } from "solid-js/store";
import type { TransferInfo } from "../platform";

const [transfers, setTransfers] = createStore<TransferInfo[]>([]);

export function upsertTransfer(t: Partial<TransferInfo> & { file_id: string }) {
  const idx = transfers.findIndex((x) => x.file_id === t.file_id);
  if (idx >= 0) {
    setTransfers(idx, t);
    return;
  }
  setTransfers(transfers.length, {
    file_id: t.file_id,
    file_name: t.file_name ?? t.file_id,
    total_size: t.total_size ?? 0,
    progress: t.progress ?? 0,
    direction: t.direction ?? "receive",
    status: t.status ?? "active",
  });
}

export function transferOf(fileId: string): TransferInfo | undefined {
  return transfers.find((x) => x.file_id === fileId);
}

export function hasTransfer(fileId: string): boolean {
  return transfers.some((x) => x.file_id === fileId);
}

export { transfers };

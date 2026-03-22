import { createStore } from "solid-js/store";
const [transfers, setTransfers] = createStore([]);
export function upsertTransfer(t) {
    setTransfers((prev) => {
        const idx = prev.findIndex((x) => x.file_id === t.file_id);
        if (idx >= 0) {
            const next = [...prev];
            next[idx] = { ...next[idx], ...t };
            return next;
        }
        return [
            ...prev,
            {
                file_id: t.file_id,
                file_name: t.file_name ?? t.file_id,
                total_size: t.total_size ?? 0,
                progress: t.progress ?? 0,
                direction: t.direction ?? "receive",
                status: t.status ?? "active",
            },
        ];
    });
}
export { transfers };

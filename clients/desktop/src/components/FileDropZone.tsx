import { createSignal, Show } from "solid-js";
import { api } from "../api/tauri";
import { upsertTransfer } from "../stores/transfers";
import { UploadIcon } from "./Icons";

export default function FileDropZone() {
  const [dragging, setDragging] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  function onDragOver(e: DragEvent) { e.preventDefault(); setDragging(true); }
  function onDragLeave() { setDragging(false); }

  async function onDrop(e: DragEvent) {
    e.preventDefault();
    setDragging(false);
    setError(null);

    const files = e.dataTransfer?.files;
    if (!files || files.length === 0) return;

    for (const file of Array.from(files)) {
      const path = (file as { path?: string }).path ?? file.name;
      try {
        const info = await api.sendFile(path);
        upsertTransfer({
          file_id: info.file_id,
          file_name: info.file_name,
          total_size: info.total_size,
          progress: 0,
          direction: "send",
          status: "active",
        });
      } catch (err) {
        setError(String(err));
      }
    }
  }

  async function onBrowse() {
    setError(null);
    try {
      const infos = await api.browseAndSend();
      for (const info of infos) {
        upsertTransfer({
          file_id: info.file_id,
          file_name: info.file_name,
          total_size: info.total_size,
          progress: 0,
          direction: "send",
          status: "active",
        });
      }
    } catch (err) {
      setError(String(err));
    }
  }

  return (
    <>
      <div
        class={`drop-zone ${dragging() ? "dragging" : ""}`}
        onDragOver={onDragOver}
        onDragLeave={onDragLeave}
        onDrop={onDrop}
        onClick={onBrowse}
      >
        <UploadIcon width="32" height="32" />
        <p>Drop files here or click to browse</p>
      </div>
      <Show when={error()}>
        <p class="error">{error()}</p>
      </Show>
    </>
  );
}

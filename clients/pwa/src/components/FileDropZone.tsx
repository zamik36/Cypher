import { createSignal, Show } from "solid-js";
import { api } from "../api";
import { upsertTransfer } from "../stores/transfers";
import { UploadIcon } from "./Icons";
import { t } from "../i18n";

export default function FileDropZone() {
  const [dragging, setDragging] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  let fileInput: HTMLInputElement | undefined;

  function onDragOver(e: DragEvent) { e.preventDefault(); setDragging(true); }
  function onDragLeave() { setDragging(false); }

  async function sendFiles(files: FileList | File[]) {
    setError(null);
    for (const file of Array.from(files)) {
      try {
        const info = await api.sendFile(file);
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

  async function onDrop(e: DragEvent) {
    e.preventDefault();
    setDragging(false);
    const files = e.dataTransfer?.files;
    if (!files || files.length === 0) return;
    await sendFiles(files);
  }

  function onBrowse() {
    fileInput?.click();
  }

  async function onFileSelected() {
    const files = fileInput?.files;
    if (!files || files.length === 0) return;
    await sendFiles(files);
    if (fileInput) fileInput.value = "";
  }

  return (
    <>
      <input
        type="file"
        ref={fileInput}
        multiple
        style={{ display: "none" }}
        onChange={onFileSelected}
      />
      <div
        class={`drop-zone ${dragging() ? "dragging" : ""}`}
        onDragOver={onDragOver}
        onDragLeave={onDragLeave}
        onDrop={onDrop}
        onClick={onBrowse}
      >
        <UploadIcon width="32" height="32" />
        <p class="drop-zone-desktop">{t().files_drop_desktop}</p>
        <p class="drop-zone-mobile">{t().files_drop_mobile}</p>
        <button class="btn-secondary btn-sm drop-zone-btn" onClick={(e) => { e.stopPropagation(); onBrowse(); }}>
          {t().files_choose}
        </button>
      </div>
      <Show when={error()}>
        <p class="error">{error()}</p>
      </Show>
    </>
  );
}

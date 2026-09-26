import { For, Show } from "solid-js";
import { api } from "../api/tauri";
import { transfers, upsertTransfer } from "../stores/transfers";
import { addToast } from "../stores/toasts";
import { FilesIcon } from "./Icons";
import { t } from "../i18n";

export default function FilesView() {
  async function accept(fileId: string) {
    try {
      await api.acceptFile(fileId);
      upsertTransfer({ file_id: fileId, status: "active" });
    } catch (e) {
      addToast(String(e), "error");
    }
  }

  async function cancel(fileId: string) {
    try {
      await api.cancelTransfer(fileId);
    } catch (e) {
      addToast(String(e), "error");
    }
  }

  return (
    <div class="files-view">
      <div class="transfer-list">
        <Show when={transfers.length > 0}>
          <h3>{t().files_title}</h3>
        </Show>
        <Show when={transfers.length === 0}>
          <div class="empty-state">
            <FilesIcon width="48" height="48" />
            <p>{t().files_empty}</p>
          </div>
        </Show>

        <For each={transfers}>
          {(tr) => {
            const pct = () => Math.max(0, Math.min(100, Math.round(tr.progress * 100)));
            const isSend = tr.direction === "send";
            const done = () => tr.status === "complete";
            return (
              <div class="transfer-item">
                <div class={`transfer-icon ${isSend ? "send" : "receive"}`}>{isSend ? "↑" : "↓"}</div>
                <div class="transfer-info">
                  <div class="transfer-name">{tr.file_name}</div>
                  <div class="transfer-meta">
                    {isSend ? t().files_sending : t().files_receiving}
                    {done() ? t().files_complete : tr.status === "error" ? " ✕" : ` ${pct()}%`}
                  </div>
                </div>
                <div class="progress-bar">
                  <div class={`progress-fill ${done() ? "complete" : ""}`} style={{ width: `${pct()}%` }} />
                </div>
                <Show when={tr.status === "offered"}>
                  <button class="btn-primary btn-sm" onClick={() => accept(tr.file_id)}>{t().files_accept}</button>
                </Show>
                <Show when={tr.status === "offered" || tr.status === "active"}>
                  <button class="btn-secondary btn-sm" onClick={() => cancel(tr.file_id)}>{t().settings_cancel}</button>
                </Show>
              </div>
            );
          }}
        </For>
      </div>
    </div>
  );
}

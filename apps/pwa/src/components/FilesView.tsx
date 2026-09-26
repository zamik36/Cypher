import { For, Show } from "solid-js";
import FileDropZone from "./FileDropZone";
import { transfers } from "../stores/transfers";
import { FilesIcon } from "./Icons";
import { t } from "../i18n";

export default function FilesView() {
  return (
    <div class="files-view">
      <FileDropZone />

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
            const pct = Math.round(tr.progress * 100);
            const isSend = tr.direction === "send";
            const isComplete = tr.status === "complete" || tr.progress >= 1;
            return (
              <div class="transfer-item">
                <div class={`transfer-icon ${isSend ? "send" : "receive"}`}>
                  {isSend ? "\u2191" : "\u2193"}
                </div>
                <div class="transfer-info">
                  <div class="transfer-name">{tr.file_name}</div>
                  <div class="transfer-meta">
                    {isSend ? t().files_sending : t().files_receiving}
                    {isComplete ? t().files_complete : `... ${pct}%`}
                  </div>
                </div>
                <div class="progress-bar">
                  <div
                    class={`progress-fill ${isComplete ? "complete" : ""}`}
                    style={{ width: `${pct}%` }}
                  />
                </div>
                <span class="transfer-percent">{pct}%</span>
              </div>
            );
          }}
        </For>
      </div>
    </div>
  );
}

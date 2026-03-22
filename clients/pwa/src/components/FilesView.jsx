import { For, Show } from "solid-js";
import FileDropZone from "./FileDropZone";
import { transfers } from "../stores/transfers";
import { FilesIcon } from "./Icons";
export default function FilesView() {
    return (<div class="files-view">
      <FileDropZone />

      <div class="transfer-list">
        <Show when={transfers.length > 0}>
          <h3>Transfers</h3>
        </Show>

        <Show when={transfers.length === 0}>
          <div class="empty-state">
            <FilesIcon width="48" height="48"/>
            <p>No file transfers yet. Drop a file above or browse to send.</p>
          </div>
        </Show>

        <For each={transfers}>
          {(t) => {
            const pct = Math.round(t.progress * 100);
            const isSend = t.direction === "send";
            const isComplete = t.status === "complete" || t.progress >= 1;
            return (<div class="transfer-item">
                <div class={`transfer-icon ${isSend ? "send" : "receive"}`}>
                  {isSend ? "\u2191" : "\u2193"}
                </div>
                <div class="transfer-info">
                  <div class="transfer-name">{t.file_name}</div>
                  <div class="transfer-meta">
                    {isSend ? "Sending" : "Receiving"}
                    {isComplete ? " \u2014 Complete" : `... ${pct}%`}
                  </div>
                </div>
                <div class="progress-bar">
                  <div class={`progress-fill ${isComplete ? "complete" : ""}`} style={{ width: `${pct}%` }}/>
                </div>
                <span class="transfer-percent">{pct}%</span>
              </div>);
        }}
        </For>
      </div>
    </div>);
}

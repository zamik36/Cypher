import { Show } from "solid-js";
import { connection } from "../stores/connection";
import { t } from "../i18n";
import LinkStatus from "./LinkStatus";

export default function StatusBar() {
  return (
    <footer class="status-bar">
      <LinkStatus />
      <Show when={connection.peerId}>
        {(id) => (
          <span class="peer-pill" title={id()}>
            {id().slice(0, 8)}
          </span>
        )}
      </Show>
      <Show when={connection.peers.length > 0}>
        <span style={{ color: "var(--text-muted)" }}>&harr;</span>
        <span class="peer-pill">{t().status_peers(connection.peers.length)}</span>
      </Show>
    </footer>
  );
}

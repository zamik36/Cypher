import { Show } from "solid-js";
import { connection } from "../stores/connection";
export default function StatusBar() {
    return (<footer class="status-bar">
      <span class={`dot ${connection.connected ? "connected" : ""}`}/>
      <span>{connection.connected ? "Connected" : "Disconnected"}</span>
      <Show when={connection.peerId}>
        <span class="peer-pill" title={connection.peerId}>
          {connection.peerId.slice(0, 8)}
        </span>
      </Show>
      <Show when={connection.peers.length > 0}>
        <span style={{ color: "var(--text-muted)" }}>&harr;</span>
        <span class="peer-pill">
          {connection.peers.length} peer{connection.peers.length > 1 ? "s" : ""}
        </span>
      </Show>
    </footer>);
}

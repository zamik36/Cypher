import { connection, type LinkState } from "../stores/connection";
import { t } from "../i18n";

/** The connection state as a dot and a word. */
export default function LinkStatus() {
  const label = (state: LinkState) => {
    const tr = t();
    const labels: Record<LinkState, string> = {
      idle: tr.status_offline,
      connecting: tr.status_connecting,
      online: tr.status_connected,
      reconnecting: tr.status_reconnecting,
      failed: tr.status_offline,
      superseded: tr.status_superseded,
      update_required: tr.status_update_required,
    };
    return labels[state];
  };
  return (
    <span class="link-status" role="status">
      <span class={`dot ${connection.link}`} />
      <span>{label(connection.link)}</span>
    </span>
  );
}

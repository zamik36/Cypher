import { createSignal, Match, Switch } from "solid-js";
import "./Overlays.css";
import { api } from "../platform";
import { connection, errorMessage, linkEvent } from "../stores/connection";
import { t } from "../i18n";

/** States the client stopped in, and what the user can do about them. */
export default function ConnectionBanner() {
  const [busy, setBusy] = createSignal(false);

  async function useHere() {
    setBusy(true);
    try {
      linkEvent("start");
      await api.reconnect();
    } catch (e) {
      linkEvent("failed", errorMessage(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Switch>
      <Match when={connection.link === "superseded"}>
        <div class="banner" role="alert">
          <div class="banner__text">
            <strong>{t().banner_superseded_title}</strong>
            <p>{t().banner_superseded_text}</p>
          </div>
          <button class="btn btn--primary" disabled={busy()} onClick={() => void useHere()}>
            {t().banner_use_here}
          </button>
        </div>
      </Match>
      <Match when={connection.link === "update_required"}>
        <div class="banner" role="alert">
          <div class="banner__text">
            <strong>{t().banner_update_title}</strong>
            <p>{t().error_update_required}</p>
          </div>
        </div>
      </Match>
    </Switch>
  );
}

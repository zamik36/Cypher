import { createSignal, Match, Show, Switch } from "solid-js";
import "./Overlays.css";
import DevicesNotice from "./DevicesNotice";
import { api } from "../platform";
import { connection, errorMessage, linkEvent } from "../stores/connection";
import { changedServer, setChangedServer } from "../stores/serverKey";
import { toastError } from "../stores/toasts";
import { t } from "../i18n";

/**
 * States the client stopped in, and what the user can do about them; while
 * it runs, news about the profile's devices.
 */
export default function ConnectionBanner() {
  const [busy, setBusy] = createSignal(false);
  /** The user asked to trust the new server key and is asked once more. */
  const [confirming, setConfirming] = createSignal(false);

  async function trustServer() {
    setBusy(true);
    try {
      await api.trustServerKey();
      setChangedServer(null);
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
      setConfirming(false);
    }
  }

  async function erase() {
    setBusy(true);
    try {
      await api.eraseDevice();
      location.reload();
    } catch (e) {
      linkEvent("failed", errorMessage(e));
      setBusy(false);
    }
  }

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
    <Switch fallback={<DevicesNotice />}>
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
      <Match when={connection.link === "unlinked"}>
        <div class="banner" role="alert">
          <div class="banner__text">
            <strong>{t().banner_unlinked_title}</strong>
            <p>{t().banner_unlinked_text}</p>
          </div>
          <button class="btn btn--danger" disabled={busy()} onClick={() => void erase()}>
            {t().banner_erase}
          </button>
        </div>
      </Match>
      <Match when={changedServer()}>
        <div class="banner" role="alert" data-testid="server-key-changed">
          <div class="banner__text">
            <strong>{t().server_key_title}</strong>
            <p>{confirming() ? t().server_key_confirm : t().server_key_text}</p>
          </div>
          <Show
            when={confirming()}
            fallback={
              <button class="btn btn--secondary" onClick={() => setConfirming(true)}>
                {t().server_key_trust}
              </button>
            }
          >
            <button class="btn btn--secondary" disabled={busy()} onClick={() => setConfirming(false)}>
              {t().common_cancel}
            </button>
            <button class="btn btn--danger" disabled={busy()} onClick={() => void trustServer()}>
              {t().server_key_trust_confirm}
            </button>
          </Show>
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

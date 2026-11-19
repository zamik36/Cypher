import { createSignal, onMount, Show } from "solid-js";
import "./Overlays.css";
import { api } from "../platform";
import { isCmdErrorShape } from "../errors";
import { lockNow } from "../stores/lock";
import { addToast, toastError } from "../stores/toasts";
import { t } from "../i18n";

/**
 * A newer version of the app, once per start: installed (after its
 * signature is checked) only when the user asks, then the app restarts.
 */
export default function UpdateBanner() {
  const [version, setVersion] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);

  async function look() {
    try {
      setVersion((await api.updates?.check()) ?? null);
    } catch {
      // Offline, or no release yet: nothing to offer.
    }
  }
  onMount(() => void look());

  async function install() {
    setBusy(true);
    try {
      await api.updates?.install();
    } catch (e) {
      setBusy(false);
      if (isCmdErrorShape(e) && e.kind === "locked") {
        // The client was stopped for the install: the app locks the way it
        // always does, and the unlock screen says why.
        addToast(t().app_update_failed_locked, "error");
        lockNow();
      } else {
        toastError(e);
      }
    }
  }

  return (
    <Show when={version()}>
      {(v) => (
        <div class="banner" role="status" data-testid="update-banner">
          <div class="banner__text">
            <strong>{t().app_update_title.replace("{version}", v())}</strong>
            <p>{t().app_update_text}</p>
          </div>
          <button class="btn btn--secondary" disabled={busy()} onClick={() => setVersion(null)}>
            {t().app_update_later}
          </button>
          <button class="btn btn--primary" disabled={busy()} onClick={() => void install()}>
            {busy() ? t().app_update_installing : t().app_update_install}
          </button>
        </div>
      )}
    </Show>
  );
}

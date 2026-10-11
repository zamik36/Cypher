import { createSignal, For, Match, Show, Switch } from "solid-js";
import { api } from "../platform";
import { dismissRestored, noRoom, restoredHint } from "../stores/devices";
import { push } from "../stores/nav";
import { toastError } from "../stores/toasts";
import { t } from "../i18n";

/**
 * What the user should know about this profile's devices: the list had no
 * room for this one, or the profile was restored here next to the others.
 */
export default function DevicesNotice() {
  /** The device the user picked to replace, waiting for their yes. */
  const [picked, setPicked] = createSignal<number | null>(null);
  const [busy, setBusy] = createSignal(false);

  async function replace(id: number) {
    setBusy(true);
    try {
      await api.devices.replace(id);
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
      setPicked(null);
    }
  }

  function openDevices() {
    dismissRestored();
    push({ name: "settings-section", section: "devices" });
  }

  return (
    <Switch>
      <Match when={noRoom()}>
        {(full) => (
          <div class="banner" role="alert" data-testid="no-room">
            <div class="banner__text">
              <strong>{t().devices_no_room_title}</strong>
              <p>{t().devices_no_room_text}</p>
            </div>
            <Show
              when={picked()}
              fallback={
                <div class="banner__choices">
                  <For each={full().devices}>
                    {(device) => (
                      <button class="btn btn--secondary" onClick={() => setPicked(device.id)}>
                        {t().devices_unnamed(device.id)}
                      </button>
                    )}
                  </For>
                </div>
              }
            >
              {(id) => (
                <div class="banner__choices">
                  <span>{t().devices_replace_confirm(t().devices_unnamed(id()))}</span>
                  <button class="btn btn--secondary" disabled={busy()} onClick={() => setPicked(null)}>
                    {t().common_cancel}
                  </button>
                  <button class="btn btn--danger" disabled={busy()} onClick={() => void replace(id())}>
                    {t().devices_replace}
                  </button>
                </div>
              )}
            </Show>
          </div>
        )}
      </Match>
      <Match when={restoredHint()}>
        <div class="banner banner--info" role="status" data-testid="restored-hint">
          <div class="banner__text">
            <strong>{t().devices_restored_title}</strong>
            <p>{t().devices_restored_text}</p>
          </div>
          <button class="btn btn--ghost" onClick={dismissRestored}>
            {t().devices_restored_dismiss}
          </button>
          <button class="btn btn--primary" onClick={openDevices}>
            {t().devices_restored_open}
          </button>
        </div>
      </Match>
    </Switch>
  );
}

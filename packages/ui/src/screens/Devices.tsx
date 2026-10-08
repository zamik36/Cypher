import { createSignal, For, Show } from "solid-js";
import Icon from "../components/Icon";
import QrScanner from "../components/QrScanner";
import { api } from "../platform";
import { dropOwnDevice, ownDevices } from "../stores/devices";
import { addToast, toastError } from "../stores/toasts";
import { findDeviceOffer, offerName } from "../utils/device";
import { t } from "../i18n";

/** Settings → Devices: this profile's devices, linking a new one, removing one. */
export default function Devices() {
  const [scanning, setScanning] = createSignal(false);
  const [typing, setTyping] = createSignal(false);
  const [typed, setTyped] = createSignal("");
  /** The offer the user scanned or typed, waiting for their yes. */
  const [offer, setOffer] = createSignal<string | null>(null);
  const [removing, setRemoving] = createSignal<number | null>(null);
  const [busy, setBusy] = createSignal(false);

  const name = (id: number, given: string) => given || t().devices_unnamed(id);

  async function link() {
    const code = offer();
    if (!code) return;
    setBusy(true);
    try {
      await api.devices.link(code);
      addToast(t().devices_linked(offerName(code) || t().devices_unnamed(0)), "success");
      setOffer(null);
      setTyped("");
      setTyping(false);
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  }

  async function remove(id: number) {
    setBusy(true);
    try {
      await api.devices.unlink(id);
      dropOwnDevice(id);
      addToast(t().devices_removed, "info");
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
      setRemoving(null);
    }
  }

  return (
    <>
      <div class="list" data-testid="devices">
        <For each={ownDevices()?.devices ?? []}>
          {(device) => (
            <div class="list-row">
              <span class="list-row__icon">
                <Icon name="devices" size={18} />
              </span>
              <span class="list-row__text">
                <span class="list-row__title">{name(device.id, device.name)}</span>
                <Show when={device.id === ownDevices()?.this}>
                  <span class="list-row__subtitle">{t().devices_this}</span>
                </Show>
              </span>
              <Show when={device.id !== ownDevices()?.this}>
                <Show
                  when={removing() === device.id}
                  fallback={
                    <button class="btn btn--ghost" disabled={busy()} onClick={() => setRemoving(device.id)}>
                      {t().devices_remove}
                    </button>
                  }
                >
                  <button class="btn btn--danger" disabled={busy()} onClick={() => void remove(device.id)}>
                    {t().devices_remove_confirm}
                  </button>
                </Show>
              </Show>
            </div>
          )}
        </For>
      </div>

      <div class="card settings__card">
        <Show
          when={offer()}
          fallback={
            <>
              <p class="hint">{t().devices_link_hint}</p>
              <div class="settings__row">
                <button class="btn btn--primary" onClick={() => setScanning(true)}>
                  <Icon name="scan" size={18} /> {t().devices_link}
                </button>
                <button class="btn btn--ghost" onClick={() => setTyping(!typing())}>
                  {t().devices_enter_code}
                </button>
              </div>
              <Show when={typing()}>
                <form
                  class="settings__row"
                  onSubmit={(e) => {
                    e.preventDefault();
                    setOffer(findDeviceOffer(typed()));
                  }}
                >
                  <input
                    class="field mono"
                    placeholder={t().devices_code_placeholder}
                    aria-label={t().devices_code_placeholder}
                    value={typed()}
                    onInput={(e) => setTyped(e.currentTarget.value)}
                    spellcheck={false}
                    autocomplete="off"
                  />
                  <button class="btn btn--secondary" type="submit" disabled={!findDeviceOffer(typed())}>
                    {t().devices_confirm_button}
                  </button>
                </form>
              </Show>
            </>
          }
        >
          {(code) => (
            <>
              <p>{t().devices_confirm(offerName(code()) || t().devices_unnamed(0))}</p>
              <div class="settings__row">
                <button class="btn btn--secondary" disabled={busy()} onClick={() => setOffer(null)}>
                  {t().common_cancel}
                </button>
                <button class="btn btn--primary" disabled={busy()} onClick={() => void link()}>
                  {t().devices_confirm_button}
                </button>
              </div>
            </>
          )}
        </Show>
      </div>

      <Show when={scanning()}>
        <QrScanner
          accept={findDeviceOffer}
          notMatching={t().scan_not_device}
          onCode={(code) => {
            setScanning(false);
            setOffer(code);
          }}
          onClose={() => setScanning(false)}
        />
      </Show>
    </>
  );
}

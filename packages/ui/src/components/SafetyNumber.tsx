import { createResource, For, onMount, Show } from "solid-js";
import { api } from "../platform";
import { t } from "../i18n";
import { digitGroups } from "../utils/safetyNumber";

interface Props {
  peerId: string;
  peerName: string;
  onClose: () => void;
}

/** A modal with the safety number of the conversation with `peerId`. */
export default function SafetyNumber(props: Props) {
  let dialog: HTMLDialogElement | undefined;
  const [number] = createResource(
    () => props.peerId,
    (peer) => api.safetyNumber(peer),
  );
  onMount(() => dialog?.showModal());

  return (
    <dialog ref={dialog} class="safety-number" aria-labelledby="safety-number-title" onClose={() => props.onClose()}>
      <h2 id="safety-number-title">
        {t().verify_title} · {props.peerName}
      </h2>
      <Show when={number()} fallback={<p class="safety-error">{String(number.error ?? "")}</p>}>
        {(digits) => (
          <ol class="safety-digits" aria-label={digits()}>
            <For each={digitGroups(digits())}>{(group) => <li>{group}</li>}</For>
          </ol>
        )}
      </Show>
      <p class="safety-hint">{t().verify_hint}</p>
      <button class="btn-primary" onClick={() => dialog?.close()} autofocus>
        {t().verify_close}
      </button>
    </dialog>
  );
}

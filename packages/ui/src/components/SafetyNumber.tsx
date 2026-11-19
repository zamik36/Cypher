import { createResource, For, onMount, Show } from "solid-js";
import "./Overlays.css";
import { api } from "../platform";
import { t } from "../i18n";
import { digitGroups } from "../utils/safetyNumber";
import { reasonText } from "../utils/reasons";

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
    <dialog ref={dialog} class="sheet" aria-labelledby="safety-number-title" onClose={() => props.onClose()}>
      <h2 id="safety-number-title" class="sheet__title">
        {t().verify_title}
      </h2>
      <p class="sheet__subtitle">{props.peerName}</p>
      <Show
        when={number()}
        fallback={
          <Show when={number.error !== undefined} fallback={<span class="spinner" />}>
            <p class="error-text">{reasonText(number.error)}</p>
          </Show>
        }
      >
        {(digits) => (
          <ol class="safety-digits mono" aria-label={digits()}>
            <For each={digitGroups(digits())}>{(group) => <li>{group}</li>}</For>
          </ol>
        )}
      </Show>
      <p class="hint">{t().verify_hint}</p>
      <button class="btn btn--primary btn--block" onClick={() => dialog?.close()} autofocus>
        {t().verify_close}
      </button>
    </dialog>
  );
}

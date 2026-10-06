import { onCleanup, onMount } from "solid-js";
import { Portal } from "solid-js/web";
import "./ImageViewer.css";
import Icon from "./Icon";
import { t } from "../i18n";

/** A picture over everything; a tap, the close button or Escape closes it. */
export default function ImageViewer(props: { src: string; alt: string; onClose: () => void }) {
  let close: HTMLButtonElement | undefined;
  onMount(() => {
    close?.focus();
    const key = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      // Ours, not the screen's: Escape would otherwise also go back.
      e.preventDefault();
      e.stopPropagation();
      props.onClose();
    };
    window.addEventListener("keydown", key, true);
    onCleanup(() => window.removeEventListener("keydown", key, true));
  });
  return (
    <Portal>
      <div class="image-viewer" role="dialog" aria-label={props.alt} onClick={() => props.onClose()}>
        <img class="image-viewer__img" src={props.src} alt={props.alt} />
        <button ref={close} class="image-viewer__close" aria-label={t().common_close} onClick={() => props.onClose()}>
          <Icon name="x" size={22} />
        </button>
      </div>
    </Portal>
  );
}

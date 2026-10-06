import { createSignal, onCleanup, onMount, Show } from "solid-js";
import Icon from "@cypher/ui/components/Icon";
import { t } from "@cypher/ui/i18n";

/** How often an open tab asks for a new version. */
const CHECK_MS = 60 * 60 * 1000;

/**
 * Offers a new version once its service worker is installed and waiting;
 * Update hands control to it and reloads.
 */
export default function UpdatePrompt() {
  const [waiting, setWaiting] = createSignal<ServiceWorker | null>(null);
  let updating = false;
  let timer: ReturnType<typeof setInterval> | undefined;
  onCleanup(() => clearInterval(timer));

  onMount(() => {
    if (!("serviceWorker" in navigator)) return;
    const sw = navigator.serviceWorker;
    // The first install has nothing to replace and takes over by itself.
    const offer = (worker: ServiceWorker | null) => {
      if (worker && sw.controller) setWaiting(worker);
    };
    void sw
      .register("/sw.js")
      .then((reg) => {
        offer(reg.waiting);
        reg.addEventListener("updatefound", () => {
          const next = reg.installing;
          next?.addEventListener("statechange", () => {
            if (next.state === "installed") offer(next);
          });
        });
        timer = setInterval(() => void reg.update().catch(() => undefined), CHECK_MS);
      })
      .catch(() => undefined);
    // Only an update the user asked for reloads the page: the first install
    // also takes control (clients.claim) and must not wipe a half-typed form.
    sw.addEventListener("controllerchange", () => {
      if (!updating) return;
      updating = false;
      location.reload();
    });
  });

  return (
    <Show when={waiting()}>
      {(worker) => (
        <div class="install-prompt" role="status">
          <Icon name="download" size={18} />
          <span class="install-prompt__text">{t().update_text}</span>
          <button
            class="btn btn--primary"
            onClick={() => {
              updating = true;
              worker().postMessage("SKIP_WAITING");
            }}
          >
            {t().update_btn}
          </button>
        </div>
      )}
    </Show>
  );
}

import { createSignal, Show, onMount, onCleanup } from "solid-js";
import { XIcon } from "./Icons";

const DISMISS_KEY = "pwa-install-dismissed";

export default function InstallPrompt() {
  interface BeforeInstallPromptEvent extends Event {
    prompt(): Promise<void>;
    userChoice: Promise<{ outcome: string }>;
  }
  const [deferredPrompt, setDeferredPrompt] = createSignal<BeforeInstallPromptEvent | null>(null);
  const [showIosHint, setShowIosHint] = createSignal(false);
  const [dismissed, setDismissed] = createSignal(
    localStorage.getItem(DISMISS_KEY) === "1"
  );

  function isIos() {
    return /iPad|iPhone|iPod/.test(navigator.userAgent) && !("MSStream" in window);
  }

  function isAndroid() {
    return /Android/.test(navigator.userAgent);
  }

  function isInStandaloneMode() {
    return (
      ("standalone" in navigator && (navigator as Record<string, unknown>).standalone === true) ||
      window.matchMedia("(display-mode: standalone)").matches
    );
  }

  function handleBeforeInstall(e: Event) {
    e.preventDefault();
    setDeferredPrompt(e as BeforeInstallPromptEvent);
  }

  onMount(() => {
    if (isInStandaloneMode()) return;

    window.addEventListener("beforeinstallprompt", handleBeforeInstall);

    // iOS Safari: no beforeinstallprompt, show manual instructions.
    if (isIos() && !isInStandaloneMode()) {
      setShowIosHint(true);
    }
    // Android: if beforeinstallprompt hasn't fired after 3s and not standalone,
    // show manual "Add to Home Screen" hint for browsers that don't fire the event.
    if (isAndroid() && !isInStandaloneMode()) {
      setTimeout(() => {
        if (!deferredPrompt()) setShowIosHint(true);
      }, 3000);
    }
  });

  onCleanup(() => {
    window.removeEventListener("beforeinstallprompt", handleBeforeInstall);
  });

  async function install() {
    const prompt = deferredPrompt();
    if (!prompt) return;
    prompt.prompt();
    const result = await prompt.userChoice;
    if (result.outcome === "accepted") {
      setDeferredPrompt(null);
    }
  }

  function dismiss() {
    setDismissed(true);
    localStorage.setItem(DISMISS_KEY, "1");
  }

  const visible = () =>
    !dismissed() && (deferredPrompt() !== null || showIosHint());

  return (
    <Show when={visible()}>
      <div class="install-prompt">
        <div class="install-prompt-content">
          <Show when={deferredPrompt()}>
            <span>Install Cypher for the best experience</span>
            <button class="btn-primary btn-sm" onClick={install}>
              Install
            </button>
          </Show>
          <Show when={showIosHint() && !deferredPrompt()}>
            <span>
              {isIos() ? (
                <>
                  Tap{" "}
                  <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" style={{ "vertical-align": "middle" }}>
                    <path d="M4 12v8a2 2 0 002 2h12a2 2 0 002-2v-8" />
                    <polyline points="16 6 12 2 8 6" />
                    <line x1="12" y1="2" x2="12" y2="15" />
                  </svg>{" "}
                  Share, then "Add to Home Screen"
                </>
              ) : (
                <>Menu &#8942; then "Add to Home Screen" or "Install app"</>
              )}
            </span>
          </Show>
        </div>
        <button class="install-prompt-close" onClick={dismiss}>
          <XIcon width="16" height="16" />
        </button>
      </div>
    </Show>
  );
}

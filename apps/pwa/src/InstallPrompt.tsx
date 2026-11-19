import { createSignal, Show, onMount, onCleanup } from "solid-js";
import Icon from "@cypher/ui/components/Icon";
import { t } from "@cypher/ui/i18n";

const DISMISS_KEY = "pwa-install-dismissed";

interface BeforeInstallPromptEvent extends Event {
  prompt(): Promise<void>;
  userChoice: Promise<{ outcome: string }>;
}

const isIos = () => /iPad|iPhone|iPod/.test(navigator.userAgent) && !("MSStream" in window);
const isAndroid = () => navigator.userAgent.includes("Android");
const isStandalone = () =>
  ("standalone" in navigator && (navigator as unknown as Record<string, unknown>)["standalone"] === true) ||
  window.matchMedia("(display-mode: standalone)").matches;

export default function InstallPrompt() {
  const [deferredPrompt, setDeferredPrompt] = createSignal<BeforeInstallPromptEvent | null>(null);
  const [showHint, setShowHint] = createSignal(false);
  const [dismissed, setDismissed] = createSignal(localStorage.getItem(DISMISS_KEY) === "1");
  let hintTimer: ReturnType<typeof setTimeout> | undefined;

  function handleBeforeInstall(e: Event) {
    e.preventDefault();
    setDeferredPrompt(e as BeforeInstallPromptEvent);
  }

  onMount(() => {
    if (isStandalone()) return;
    window.addEventListener("beforeinstallprompt", handleBeforeInstall);
    if (isIos()) setShowHint(true);
    if (isAndroid()) {
      hintTimer = setTimeout(() => {
        if (!deferredPrompt()) setShowHint(true);
      }, 3000);
    }
  });

  onCleanup(() => {
    clearTimeout(hintTimer);
    window.removeEventListener("beforeinstallprompt", handleBeforeInstall);
  });

  async function install() {
    const prompt = deferredPrompt();
    if (!prompt) return;
    await prompt.prompt();
    if ((await prompt.userChoice).outcome === "accepted") setDeferredPrompt(null);
  }

  function dismiss() {
    setDismissed(true);
    localStorage.setItem(DISMISS_KEY, "1");
  }

  const visible = () => !dismissed() && (deferredPrompt() !== null || showHint());

  return (
    <Show when={visible()}>
      <div class="install-prompt" role="dialog" aria-label={t().install_text}>
        <span class="install-prompt__text">
          <Show when={deferredPrompt()} fallback={isIos() ? t().install_ios : t().install_android}>
            {t().install_text}
          </Show>
        </span>
        <Show when={deferredPrompt()}>
          <button class="btn btn--primary" onClick={() => void install()}>
            {t().install_btn}
          </button>
        </Show>
        <button class="icon-btn" aria-label={t().common_close} onClick={dismiss}>
          <Icon name="x" size={18} />
        </button>
      </div>
    </Show>
  );
}

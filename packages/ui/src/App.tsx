import "./stores/theme";
import { createEffect, createSignal, onCleanup, Show } from "solid-js";
import IdentityView from "./components/IdentityView";
import AppShell from "./screens/AppShell";
import { startApp } from "./app/wire";
import { api } from "./platform";
import { clearAllMessages } from "./stores/chat";
import { setConnection } from "./stores/connection";
import { loadConversations, totalUnread } from "./stores/contacts";
import { clearAllDrafts } from "./stores/drafts";
import { watchForLock } from "./stores/lock";
import { reset } from "./stores/nav";
import { setNickname } from "./stores/profile";
import { toastError } from "./stores/toasts";
import { closeToTray } from "./stores/window";
import { t } from "./i18n";

export default function App() {
  const [unlocked, setUnlocked] = createSignal(false);
  let stop: (() => void) | undefined;
  let stopWatching: (() => void) | undefined;
  onCleanup(() => {
    stopWatching?.();
    stop?.();
  });

  async function handleUnlocked(peerId: string, nick: string) {
    setNickname(nick);
    setConnection({ peerId });
    setUnlocked(true);
    stop = await startApp();
    stopWatching = watchForLock(() => void lock());
  }

  /** Back to the passphrase: nothing decrypted stays on screen or in memory. */
  async function lock() {
    if (!unlocked()) return;
    stopWatching?.();
    stop?.();
    [stop, stopWatching] = [undefined, undefined];
    setUnlocked(false);
    reset();
    clearAllMessages();
    clearAllDrafts();
    loadConversations([]);
    setNickname(null);
    setConnection({ peerId: null, link: "idle" });
    await api.lock().catch(toastError);
  }

  // The desktop tray speaks the UI's language and counts unread messages.
  createEffect(() => {
    const shell = api.shell;
    if (!shell) return;
    const unread = unlocked() ? totalUnread() : 0;
    const tooltip = unread > 0 ? t().tray_unread(unread) : t().identity_title;
    void shell.setTray(t().tray_open, t().tray_quit, tooltip).catch(() => undefined);
  });
  createEffect(() => {
    void api.shell?.setCloseToTray(closeToTray()).catch(() => undefined);
  });

  return (
    <Show when={unlocked()} fallback={<IdentityView onUnlocked={(id, nick) => void handleUnlocked(id, nick)} />}>
      <AppShell />
    </Show>
  );
}

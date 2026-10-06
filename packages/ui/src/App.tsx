import "./stores/theme";
import { createSignal, onCleanup, Show } from "solid-js";
import IdentityView from "./components/IdentityView";
import AppShell from "./screens/AppShell";
import { startApp } from "./app/wire";
import { api } from "./platform";
import { clearAllMessages } from "./stores/chat";
import { setConnection } from "./stores/connection";
import { loadConversations } from "./stores/contacts";
import { clearAllDrafts } from "./stores/drafts";
import { watchForLock } from "./stores/lock";
import { reset } from "./stores/nav";
import { setNickname } from "./stores/profile";
import { toastError } from "./stores/toasts";

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

  return (
    <Show when={unlocked()} fallback={<IdentityView onUnlocked={(id, nick) => void handleUnlocked(id, nick)} />}>
      <AppShell />
    </Show>
  );
}

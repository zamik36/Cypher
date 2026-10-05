import "./stores/theme";
import { createSignal, onCleanup, Show } from "solid-js";
import IdentityView from "./components/IdentityView";
import AppShell from "./screens/AppShell";
import { startApp } from "./app/wire";
import { setConnection } from "./stores/connection";
import { setNickname } from "./stores/profile";

export default function App() {
  const [unlocked, setUnlocked] = createSignal(false);
  let stop: (() => void) | undefined;
  onCleanup(() => stop?.());

  async function handleUnlocked(peerId: string, nick: string) {
    setNickname(nick);
    setConnection({ peerId });
    setUnlocked(true);
    stop = await startApp();
  }

  return (
    <Show when={unlocked()} fallback={<IdentityView onUnlocked={(id, nick) => void handleUnlocked(id, nick)} />}>
      <AppShell />
    </Show>
  );
}

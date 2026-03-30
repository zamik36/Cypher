import { createSignal, onMount, onCleanup, Show } from "solid-js";
import Sidebar, { type Page } from "./components/Sidebar";
import BottomNav from "./components/BottomNav";
import InstallPrompt from "./components/InstallPrompt";
import HomeView from "./components/HomeView";
import ChatPane from "./components/ChatPane";
import FilesView from "./components/FilesView";
import SettingsView from "./components/SettingsView";
import StatusBar from "./components/StatusBar";
import ToastContainer from "./components/ToastContainer";
import IdentityView from "./components/IdentityView";
import {
  onConnected, onDisconnected, onPeerConnected,
  onMessage, onMessageSent, onFileOffered, onFileProgress, onFileComplete, onError,
  api, setPeerId as apiSetPeerId,
} from "./api";
import { connection, setConnection, addPeer, shortName } from "./stores/connection";
import { addMessage } from "./stores/chat";
import { upsertTransfer } from "./stores/transfers";
import { addToast } from "./stores/toasts";
import type { IdentityData } from "./storage/identity";
import { deriveStorageKey } from "./storage/identity";
import { openMessageStore, saveMessage, saveConversation } from "./storage/messages";

export default function App() {
  const [page, setPage] = createSignal<Page>("home");
  const [theme, setTheme] = createSignal<"dark" | "light">("dark");
  const [unread, setUnread] = createSignal(0);
  const [drawerOpen, setDrawerOpen] = createSignal(false);
  const [unlocked, setUnlocked] = createSignal(false);
  const [identityNickname, setIdentityNickname] = createSignal<string | null>(null);

  let pendingRoom: { code: string; role: "host" | "guest" } | null = null;

  function setPendingRoom(code: string, role: "host" | "guest") {
    pendingRoom = { code, role };
  }

  function toggleTheme() {
    const next = theme() === "dark" ? "light" : "dark";
    setTheme(next);
    document.documentElement.setAttribute("data-theme", next);
  }

  function navigateTo(p: Page) {
    if (p === "chat") setUnread(0);
    setPage(p);
  }

  async function handleIdentityUnlocked(data: IdentityData) {
    // Set the API's peerId to the persistent one.
    apiSetPeerId(data.peerId);
    setIdentityNickname(data.nickname);

    // Open encrypted message store.
    try {
      const sek = await deriveStorageKey(data.seed);
      await openMessageStore(sek);
    } catch (e) {
      console.error("Failed to open message store:", e);
      addToast("Message history unavailable — messages won't persist across restarts", "error");
    }

    setUnlocked(true);
    startApp();
  }

  let cleanupFns: Array<() => void> = [];
  onCleanup(() => cleanupFns.forEach((fn) => fn()));

  async function startApp() {
    const unlisten = await Promise.all([
      onConnected((peerId) => {
        setConnection({ connected: true, peerId, status: "connected", gatewayConnecting: false, gatewayError: null });
      }),
      onDisconnected(() => {
        setConnection({ connected: false, peerId: null, status: "disconnected" });
      }),
      onPeerConnected((remotePeerId) => {
        const room = pendingRoom || { code: "direct", role: "guest" as const };
        addPeer({
          peerId: remotePeerId,
          roomCode: room.code,
          role: room.role,
          displayName: shortName(remotePeerId),
        });
        pendingRoom = null;
        setConnection({ status: "peer connected" });
        addToast("Peer connected!", "success");
        navigateTo("chat");
        // Save conversation to IndexedDB.
        saveConversation(remotePeerId, null).catch(() => {});
      }),
      onMessage((msg) => {
        const peerId = msg.from;
        addMessage(peerId, msg);
        if (page() !== "chat") setUnread((n: number) => n + 1);
        // Persist received message.
        saveMessage(peerId, "received", msg.text, msg.timestamp).catch((e) =>
          console.warn("Failed to persist received message:", e));
      }),
      onFileOffered((info) => {
        api.acceptFile(info.file_id, info.name).catch((e) => console.error("accept_file failed:", e));
        upsertTransfer({
          file_id: info.file_id,
          file_name: info.name,
          total_size: info.size,
          progress: 0,
          direction: "receive",
          status: "active",
        });
        addToast(`Receiving: ${info.name}`, "info");
        navigateTo("files");
      }),
      onFileProgress((info) => {
        upsertTransfer({ file_id: info.file_id, progress: info.progress });
      }),
      onFileComplete((fileId) => {
        upsertTransfer({ file_id: fileId, progress: 1.0, status: "complete" });
        addToast("Transfer complete!", "success");
      }),
      onMessageSent((info) => {
        // Persist sent message to IndexedDB.
        saveMessage(info.to, "sent", info.text, info.timestamp).catch((e) =>
          console.warn("Failed to persist sent message:", e));
      }),
      onError((msg) => {
        addToast(msg, "error");
      }),
    ]);

    cleanupFns = unlisten;

    // Auto-connect to gateway.
    setConnection({ gatewayConnecting: true, gatewayError: null });
    try {
      await api.connectToGateway(connection.gatewayAddr);
      setConnection({ connected: true, gatewayConnecting: false, gatewayError: null, status: "connected" });
    } catch (e) {
      setConnection({ gatewayConnecting: false, gatewayError: String(e) });
    }
  }

  onMount(() => {
    // If no identity exists yet, show identity screen right away.
    // The actual app boot happens after identity unlock.
  });

  return (
    <Show when={unlocked()} fallback={<IdentityView onUnlocked={handleIdentityUnlocked} />}>
      <div class="app">
        <Sidebar
          page={page()}
          setPage={navigateTo}
          theme={theme()}
          toggleTheme={toggleTheme}
          unread={unread()}
          drawerOpen={drawerOpen()}
          setDrawerOpen={setDrawerOpen}
        />

        <main class="content">
          <Show when={page() === "home"}>
            <HomeView onNavigate={navigateTo} onPendingRoom={setPendingRoom} />
          </Show>
          <Show when={page() === "chat"}><ChatPane onNavigate={navigateTo} /></Show>
          <Show when={page() === "files"}><FilesView /></Show>
          <Show when={page() === "settings"}>
            <SettingsView
              theme={theme()}
              setTheme={(t) => { setTheme(t); document.documentElement.setAttribute("data-theme", t); }}
              nickname={identityNickname()}
            />
          </Show>
        </main>

        <StatusBar />
        <BottomNav page={page()} setPage={navigateTo} unread={unread()} />
        <InstallPrompt />
        <ToastContainer />
      </div>
    </Show>
  );
}

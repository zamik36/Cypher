import { createSignal, onCleanup, Show } from "solid-js";
import Sidebar, { type Page } from "./components/Sidebar";
import BottomNav from "./components/BottomNav";
import HomeView from "./components/HomeView";
import ChatPane from "./components/ChatPane";
import FilesView from "./components/FilesView";
import SettingsView from "./components/SettingsView";
import StatusBar from "./components/StatusBar";
import ToastContainer from "./components/ToastContainer";
import IdentityView from "./components/IdentityView";
import {
  onConnected, onDisconnected, onPeerConnected,
  onMessage, onFileOffered, onFileProgress, onFileComplete, onError,
  api,
} from "./api/tauri";
import { connection, setConnection, addPeer, shortName } from "./stores/connection";
import { addMessage } from "./stores/chat";
import { upsertTransfer } from "./stores/transfers";
import { addToast } from "./stores/toasts";
import { t } from "./i18n";

export default function App() {
  const [page, setPage] = createSignal<Page>("home");
  const [theme, setTheme] = createSignal<"dark" | "light">("dark");
  const [unread, setUnread] = createSignal(0);
  const [drawerOpen, setDrawerOpen] = createSignal(false);
  const [nickname, setNickname] = createSignal<string | null>(null);
  const [unlocked, setUnlocked] = createSignal(false);

  async function handleIdentityUnlocked(peerId: string, nick: string) {
    setNickname(nick);
    setConnection({ peerId });

    // Restore saved conversations from SQLite.
    try {
      const conversations = await api.getConversations();
      for (const conv of conversations) {
        addPeer({
          peerId: conv.peer_id,
          roomCode: "saved",
          role: "guest",
          displayName: conv.display_name || shortName(conv.peer_id),
          online: false,
        });
      }
    } catch (e) {
      console.warn("Failed to load saved conversations:", e);
    }

    setUnlocked(true);
    startApp();
  }

  // Track pending room info so we can associate the next PeerConnected with a room
  let pendingRoom: { code: string; role: "host" | "guest" } | null = null;

  /** Called from HomeView when a room is created or joined */
  function setPendingRoom(code: string, role: "host" | "guest") {
    pendingRoom = { code, role };
  }

  function toggleTheme() {
    const next = theme() === "dark" ? "light" : "dark";
    setTheme(next);
    document.documentElement.setAttribute("data-theme", next);
  }

  // Reset unread when entering chat
  function navigateTo(p: Page) {
    if (p === "chat") setUnread(0);
    setPage(p);
  }

  // Store unlisten functions for cleanup
  let cleanupFns: Array<() => void> = [];
  onCleanup(() => cleanupFns.forEach((fn) => fn()));

  async function startApp() {
    // Register event listeners BEFORE connecting, so we don't miss the Connected event
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
          online: true,
        });
        pendingRoom = null;
        setConnection({ status: "peer connected" });
        addToast(t().toast_peer_connected, "success");
        navigateTo("chat");
      }),
      onMessage((msg) => {
        const peerId = msg.from;
        addMessage(peerId, msg);
        if (page() !== "chat") {
          setUnread((n) => n + 1);
        }
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
        addToast(t().toast_receiving(info.name), "info");
        navigateTo("files");
      }),
      onFileProgress((info) => {
        upsertTransfer({
          file_id: info.file_id,
          progress: info.progress,
        });
      }),
      onFileComplete((fileId) => {
        upsertTransfer({
          file_id: fileId,
          progress: 1.0,
          status: "complete",
        });
        addToast(t().toast_transfer_complete, "success");
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
        nickname={nickname()}
      />

      <main class="content">
        <Show when={page() === "home"}>
          <HomeView onNavigate={navigateTo} onPendingRoom={setPendingRoom} />
        </Show>
        <Show when={page() === "chat"}><ChatPane onNavigate={navigateTo} /></Show>
        <Show when={page() === "files"}><FilesView /></Show>
        <Show when={page() === "settings"}>
          <SettingsView theme={theme()} setTheme={(t) => { setTheme(t); document.documentElement.setAttribute("data-theme", t); }} nickname={nickname()} />
        </Show>
      </main>

      <StatusBar />
      <BottomNav page={page()} setPage={navigateTo} unread={unread()} />
      <ToastContainer />
    </div>
    </Show>
  );
}

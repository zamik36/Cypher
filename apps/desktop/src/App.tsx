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
  onConnected, onDisconnected, onPeerConnected, onMessage, onMessageStatus,
  onFileOffered, onFileProgress, onFileComplete, onFileFailed, onError, onAnonymityLevel,
  api,
} from "./api/tauri";
import {
  connection, setConnection, addPeer, shortName, setPeerOnline, markAllPeersOffline,
} from "./stores/connection";
import { addMessage, peerOf, setMessageStatus } from "./stores/chat";
import { upsertTransfer } from "./stores/transfers";
import { addToast } from "./stores/toasts";
import { anonymousSettings, setAnonymityStatus } from "./stores/anonymity";
import { t } from "./i18n";
import { notifyMessage } from "./utils/notifications";

export default function App() {
  const [page, setPage] = createSignal<Page>("home");
  const [theme, setTheme] = createSignal<"dark" | "light">("dark");
  const [unread, setUnread] = createSignal(0);
  const [drawerOpen, setDrawerOpen] = createSignal(false);
  const [nickname, setNickname] = createSignal<string | null>(null);
  const [unlocked, setUnlocked] = createSignal(false);

  function handleIdentityUnlocked(peerId: string, nick: string) {
    setNickname(nick);
    setConnection({ peerId });
    setUnlocked(true);
    void startApp();
  }

  function applyTheme(next: "dark" | "light") {
    setTheme(next);
    document.documentElement.setAttribute("data-theme", next);
  }

  function navigateTo(p: Page) {
    if (p === "chat") setUnread(0);
    setPage(p);
  }

  let cleanupFns: Array<() => void> = [];
  onCleanup(() => cleanupFns.forEach((fn) => fn()));

  async function loadConversations() {
    try {
      for (const conv of await api.getConversations()) {
        addPeer({
          peerId: conv.peer_id,
          roomCode: "saved",
          role: "guest",
          displayName: conv.display_name || shortName(conv.peer_id),
          online: false,
        });
      }
    } catch (e) {
      console.warn("Failed to load conversations:", e);
    }
  }

  async function startApp() {
    cleanupFns = await Promise.all([
      onConnected(() => {
        setConnection({ connected: true, status: "connected", gatewayConnecting: false, gatewayError: null });
      }),
      onDisconnected(() => {
        setConnection({ connected: false, status: "disconnected" });
        markAllPeersOffline();
      }),
      onPeerConnected((peerId) => {
        addPeer({ peerId, roomCode: "direct", role: "guest", displayName: shortName(peerId), online: true });
        setConnection({ status: "peer connected" });
        addToast(t().toast_peer_connected, "success");
        navigateTo("chat");
      }),
      onMessage((msg) => {
        addPeer({ peerId: msg.from, roomCode: "direct", role: "guest", displayName: shortName(msg.from), online: true });
        addMessage(msg.from, msg);
        void notifyMessage(shortName(msg.from), msg.text);
        if (page() !== "chat") setUnread((n) => n + 1);
      }),
      onMessageStatus(({ msg_id, status }) => {
        setMessageStatus(msg_id, status);
        const peer = peerOf(msg_id);
        if (peer && (status === "sent" || status === "queued")) {
          setPeerOnline(peer, status === "sent");
        }
      }),
      onFileOffered((info) => {
        upsertTransfer({
          file_id: info.file_id,
          file_name: info.name,
          total_size: info.size,
          progress: 0,
          direction: "receive",
          status: "offered",
        });
        addToast(t().toast_receiving(info.name), "info");
      }),
      onFileProgress((info) => upsertTransfer({ file_id: info.file_id, progress: info.progress })),
      onFileComplete((fileId) => {
        upsertTransfer({ file_id: fileId, progress: 1, status: "complete" });
        addToast(t().toast_transfer_complete, "success");
      }),
      onFileFailed(({ file_id, reason }) => {
        upsertTransfer({ file_id, status: "error" });
        addToast(reason, "error");
      }),
      onError((msg) => addToast(msg, "error")),
      onAnonymityLevel((payload) => {
        setAnonymityStatus({ supported: true, label: payload.label, description: payload.description });
      }),
    ]);

    setConnection({ gatewayConnecting: true, gatewayError: null });
    try {
      await api.connectToGateway(connection.gatewayAddr, anonymousSettings.enabled, anonymousSettings.bridgeLines);
      await loadConversations();
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
          toggleTheme={() => applyTheme(theme() === "dark" ? "light" : "dark")}
          unread={unread()}
          drawerOpen={drawerOpen()}
          setDrawerOpen={setDrawerOpen}
          nickname={nickname()}
        />

        <main class="content">
          <Show when={page() === "home"}>
            <HomeView onNavigate={navigateTo} />
          </Show>
          <Show when={page() === "chat"}><ChatPane onNavigate={navigateTo} /></Show>
          <Show when={page() === "files"}><FilesView /></Show>
          <Show when={page() === "settings"}>
            <SettingsView theme={theme()} setTheme={applyTheme} nickname={nickname()} />
          </Show>
        </main>

        <StatusBar />
        <BottomNav page={page()} setPage={navigateTo} unread={unread()} />
        <ToastContainer />
      </div>
    </Show>
  );
}

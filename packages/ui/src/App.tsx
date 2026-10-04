import "./stores/theme";
import { createSignal, onCleanup, Show } from "solid-js";
import { reasonText } from "./utils/reasons";
import Sidebar, { type Page } from "./components/Sidebar";
import BottomNav from "./components/BottomNav";
import HomeView from "./components/HomeView";
import ChatPane, { previewText } from "./components/ChatPane";
import FilesView from "./components/FilesView";
import SettingsView from "./components/SettingsView";
import StatusBar from "./components/StatusBar";
import ConnectionBanner from "./components/ConnectionBanner";
import ToastContainer from "./components/ToastContainer";
import IdentityView from "./components/IdentityView";
import {
  onConnected,
  onDisconnected,
  onSuperseded,
  onUpdateRequired,
  onPeerConnected,
  onMessage,
  onMessageStatus,
  onFileOffered,
  onFileProgress,
  onFileComplete,
  onFileFailed,
  onError,
  onAnonymityLevel,
  api,
} from "./platform";
import {
  connection,
  setConnection,
  addPeer,
  shortName,
  setPeerOnline,
  markAllPeersOffline,
  linkEvent,
  connectGateway,
} from "./stores/connection";
import { addMessage, peerOf, setMessageStatus } from "./stores/chat";
import { hasTransfer, upsertTransfer } from "./stores/transfers";
import { setMediaProgress, trackMedia } from "./stores/media";
import { addToast } from "./stores/toasts";
import { anonymousSettings, setOnionUp } from "./stores/anonymity";
import { t } from "./i18n";
import { notifyMessage } from "./utils/notifications";

export default function App() {
  const [page, setPage] = createSignal<Page>("home");
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

  function navigateTo(p: Page) {
    if (p === "chat") setUnread(0);
    setPage(p);
  }

  let cleanupFns: (() => void)[] = [];
  onCleanup(() => cleanupFns.forEach((fn) => fn()));

  async function loadConversations() {
    try {
      for (const conv of await api.getConversations()) {
        addPeer({
          peerId: conv.peer_id,
          roomCode: "saved",
          role: "guest",
          displayName: conv.alias ?? shortName(conv.peer_id),
          online: false,
        });
      }
    } catch (e) {
      console.warn("Failed to load conversations:", e);
    }
  }

  async function startApp() {
    cleanupFns = await Promise.all([
      onConnected(() => linkEvent("connected")),
      onDisconnected(() => {
        linkEvent("disconnected");
        markAllPeersOffline();
      }),
      onSuperseded(() => {
        linkEvent("superseded");
        markAllPeersOffline();
      }),
      onUpdateRequired(() => {
        linkEvent("update_required");
        markAllPeersOffline();
      }),
      onPeerConnected((peerId) => {
        addPeer({ peerId, roomCode: "direct", role: "guest", displayName: shortName(peerId), online: true });
        addToast(t().toast_peer_connected, "success");
        navigateTo("chat");
      }),
      // eslint-disable-next-line solid/reactivity -- an event callback: it samples page() per message on purpose.
      onMessage((msg) => {
        addPeer({
          peerId: msg.from,
          roomCode: "direct",
          role: "guest",
          displayName: shortName(msg.from),
          online: true,
        });
        if (msg.file && msg.file.kind !== "file") trackMedia(msg.file.file_id);
        const text = previewText(msg.text, msg.file);
        addMessage(msg.from, { ...msg, text });
        void notifyMessage(shortName(msg.from), text);
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
      onFileProgress((info) => {
        setMediaProgress(info.file_id, info.progress);
        if (hasTransfer(info.file_id)) upsertTransfer({ file_id: info.file_id, progress: info.progress });
      }),
      onFileComplete((fileId) => {
        setMediaProgress(fileId, 1);
        if (!hasTransfer(fileId)) return;
        upsertTransfer({ file_id: fileId, progress: 1, status: "complete" });
        addToast(t().toast_transfer_complete, "success");
      }),
      onFileFailed(({ file_id, reason }) => {
        if (hasTransfer(file_id)) upsertTransfer({ file_id, status: "error" });
        addToast(reasonText(reason), "error");
      }),
      onError((msg) => addToast(reasonText(msg), "error")),
      onAnonymityLevel((payload) => {
        setOnionUp(payload.level > 0);
      }),
    ]);

    await connectGateway(() =>
      api.connectToGateway(connection.gatewayAddr, anonymousSettings.enabled, anonymousSettings.bridgeLines),
    );
    await loadConversations();
  }

  return (
    <Show when={unlocked()} fallback={<IdentityView onUnlocked={handleIdentityUnlocked} />}>
      <div class="app">
        <Sidebar
          page={page()}
          setPage={navigateTo}
          unread={unread()}
          drawerOpen={drawerOpen()}
          setDrawerOpen={setDrawerOpen}
          nickname={nickname()}
        />

        <main class="content">
          <ConnectionBanner />
          <Show when={page() === "home"}>
            <HomeView onNavigate={navigateTo} />
          </Show>
          <Show when={page() === "chat"}>
            <ChatPane onNavigate={navigateTo} />
          </Show>
          <Show when={page() === "files"}>
            <FilesView />
          </Show>
          <Show when={page() === "settings"}>
            <SettingsView nickname={nickname()} />
          </Show>
        </main>

        <StatusBar />
        <BottomNav page={page()} setPage={navigateTo} unread={unread()} />
        <ToastContainer />
      </div>
    </Show>
  );
}

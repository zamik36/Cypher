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
import { openMessageStore, saveMessage, saveConversation, listConversations } from "./storage/messages";
import { hexEncode } from "./api/proto";
import { notifyMessage } from "./utils/notifications";
import { t } from "./i18n";

export default function App() {
  const [page, setPage] = createSignal<Page>("home");
  const [theme, setTheme] = createSignal<"dark" | "light">("dark");
  const [unread, setUnread] = createSignal(0);
  const [drawerOpen, setDrawerOpen] = createSignal(false);
  const [unlocked, setUnlocked] = createSignal(false);
  const [identityNickname, setIdentityNickname] = createSignal<string | null>(null);

  const SESSION_SEK_KEY = "cypher-session-sek";
  const SESSION_PEERID_KEY = "cypher-session-peerId";
  const SESSION_NICKNAME_KEY = "cypher-session-nickname";

  function bytesToBase64(bytes: Uint8Array): string {
    let binary = "";
    for (const b of bytes) binary += String.fromCharCode(b);
    return btoa(binary);
  }

  function base64ToBytes(b64: string): Uint8Array {
    const binary = atob(b64);
    const bytes = new Uint8Array(binary.length);
    for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
    return bytes;
  }

  function hexToBytes(hex: string): Uint8Array {
    const bytes = new Uint8Array(hex.length / 2);
    for (let i = 0; i < bytes.length; i++) bytes[i] = parseInt(hex.substring(i * 2, i * 2 + 2), 16);
    return bytes;
  }

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
      // Cache session data for page refresh (not tab close).
      sessionStorage.setItem(SESSION_SEK_KEY, bytesToBase64(sek));
      sessionStorage.setItem(SESSION_PEERID_KEY, hexEncode(data.peerId));
      sessionStorage.setItem(SESSION_NICKNAME_KEY, data.nickname);
    } catch (e) {
      console.error("Failed to open message store:", e);
      addToast(t().toast_msg_unavailable, "error");
    }

    // Restore saved conversations from IndexedDB.
    try {
      const conversations = await listConversations();
      for (const conv of conversations) {
        addPeer({
          peerId: conv.peerId,
          roomCode: "saved",
          role: "guest",
          displayName: conv.nickname || shortName(conv.peerId),
          online: false,
        });
      }
    } catch (e) {
      console.warn("Failed to load saved conversations:", e);
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
          online: true,
        });
        pendingRoom = null;
        setConnection({ status: "peer connected" });
        addToast(t().toast_peer_connected, "success");
        navigateTo("chat");
        // Save conversation to IndexedDB.
        saveConversation(remotePeerId, null).catch(() => {});
      }),
      onMessage((msg) => {
        const peerId = msg.from;
        addMessage(peerId, msg);
        if (page() !== "chat") setUnread((n: number) => n + 1);
        // Show system notification when app is not focused.
        notifyMessage(shortName(peerId), msg.text);
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
        addToast(t().toast_receiving(info.name), "info");
        navigateTo("files");
      }),
      onFileProgress((info) => {
        upsertTransfer({ file_id: info.file_id, progress: info.progress });
      }),
      onFileComplete((fileId) => {
        upsertTransfer({ file_id: fileId, progress: 1.0, status: "complete" });
        addToast(t().toast_transfer_complete, "success");
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

  onMount(async () => {
    // Try to restore session from sessionStorage (survives page refresh, not tab close).
    const sekB64 = sessionStorage.getItem(SESSION_SEK_KEY);
    const peerIdHex = sessionStorage.getItem(SESSION_PEERID_KEY);
    const nickname = sessionStorage.getItem(SESSION_NICKNAME_KEY);

    if (sekB64 && peerIdHex && nickname) {
      try {
        const sek = base64ToBytes(sekB64);
        const peerId = hexToBytes(peerIdHex);

        // Validate restored data integrity.
        if (sek.length !== 32 || peerId.length !== 32 || peerId.some((b) => Number.isNaN(b))) {
          throw new Error("Corrupted session cache");
        }

        apiSetPeerId(peerId);
        setIdentityNickname(nickname);
        await openMessageStore(sek);

        // Restore saved conversations.
        try {
          const conversations = await listConversations();
          for (const conv of conversations) {
            addPeer({
              peerId: conv.peerId,
              roomCode: "saved",
              role: "guest",
              displayName: conv.nickname || shortName(conv.peerId),
              online: false,
            });
          }
        } catch (e) {
          console.warn("Failed to load saved conversations:", e);
        }

        setUnlocked(true);
        startApp();
        return;
      } catch (e) {
        console.warn("Session restore failed, clearing cache:", e);
        sessionStorage.removeItem(SESSION_SEK_KEY);
        sessionStorage.removeItem(SESSION_PEERID_KEY);
        sessionStorage.removeItem(SESSION_NICKNAME_KEY);
      }
    }
    // Otherwise show identity/unlock screen (handled by Show fallback).
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

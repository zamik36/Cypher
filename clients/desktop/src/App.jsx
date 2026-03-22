import { createSignal, onMount, onCleanup, Show } from "solid-js";
import Sidebar from "./components/Sidebar";
import HomeView from "./components/HomeView";
import ChatPane from "./components/ChatPane";
import FilesView from "./components/FilesView";
import SettingsView from "./components/SettingsView";
import StatusBar from "./components/StatusBar";
import ToastContainer from "./components/ToastContainer";
import { onConnected, onDisconnected, onPeerConnected, onMessage, onFileOffered, onFileProgress, onFileComplete, onError, api, } from "./api/tauri";
import { connection, setConnection, addPeer, shortName } from "./stores/connection";
import { addMessage } from "./stores/chat";
import { upsertTransfer } from "./stores/transfers";
import { addToast } from "./stores/toasts";
export default function App() {
    const [page, setPage] = createSignal("home");
    const [theme, setTheme] = createSignal("dark");
    const [unread, setUnread] = createSignal(0);
    const [drawerOpen, setDrawerOpen] = createSignal(false);
    // Track pending room info so we can associate the next PeerConnected with a room
    let pendingRoom = null;
    /** Called from HomeView when a room is created or joined */
    function setPendingRoom(code, role) {
        pendingRoom = { code, role };
    }
    function toggleTheme() {
        const next = theme() === "dark" ? "light" : "dark";
        setTheme(next);
        document.documentElement.setAttribute("data-theme", next);
    }
    // Reset unread when entering chat
    function navigateTo(p) {
        if (p === "chat")
            setUnread(0);
        setPage(p);
    }
    // Store unlisten functions for cleanup
    let cleanupFns = [];
    onCleanup(() => cleanupFns.forEach((fn) => fn()));
    onMount(async () => {
        // Register event listeners BEFORE connecting, so we don't miss the Connected event
        const unlisten = await Promise.all([
            onConnected((peerId) => {
                console.log("[P2P] event: connected, peerId:", peerId);
                setConnection({ connected: true, peerId, status: "connected", gatewayConnecting: false, gatewayError: null });
            }),
            onDisconnected(() => {
                setConnection({ connected: false, peerId: null, status: "disconnected" });
            }),
            onPeerConnected((remotePeerId) => {
                console.log("[P2P] event: peer_connected", remotePeerId);
                const room = pendingRoom || { code: "direct", role: "guest" };
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
            }),
            onMessage((msg) => {
                // msg.from is the peer's hex id
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
                addToast(`Receiving: ${info.name}`, "info");
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
                addToast("Transfer complete!", "success");
            }),
            onError((msg) => {
                addToast(msg, "error");
                console.error("p2p error:", msg);
            }),
        ]);
        cleanupFns = unlisten;
        // Now auto-connect to gateway (listeners are already in place)
        setConnection({ gatewayConnecting: true, gatewayError: null });
        try {
            await api.connectToGateway(connection.gatewayAddr);
            console.log("[P2P] connectToGateway resolved OK");
            setConnection({ connected: true, gatewayConnecting: false, gatewayError: null, status: "connected" });
        }
        catch (e) {
            console.error("[P2P] connectToGateway error:", e);
            setConnection({ gatewayConnecting: false, gatewayError: String(e) });
        }
    });
    return (<div class="app">
      <Sidebar page={page()} setPage={navigateTo} theme={theme()} toggleTheme={toggleTheme} unread={unread()} drawerOpen={drawerOpen()} setDrawerOpen={setDrawerOpen}/>

      <main class="content">
        <Show when={page() === "home"}>
          <HomeView onNavigate={navigateTo} onPendingRoom={setPendingRoom}/>
        </Show>
        <Show when={page() === "chat"}><ChatPane onNavigate={navigateTo}/></Show>
        <Show when={page() === "files"}><FilesView /></Show>
        <Show when={page() === "settings"}>
          <SettingsView theme={theme()} setTheme={(t) => { setTheme(t); document.documentElement.setAttribute("data-theme", t); }}/>
        </Show>
      </main>

      <StatusBar />
      <ToastContainer />
    </div>);
}

import { createSignal, Show } from "solid-js";
import { api } from "../api/tauri";
import { connection, setConnection, addPeer, shortName } from "../stores/connection";
import Spinner from "./Spinner";
import { UsersIcon, LinkIcon, CopyIcon, CheckIcon } from "./Icons";
import type { Page } from "./Sidebar";
import { t } from "../i18n";

interface HomeViewProps {
  onNavigate: (p: Page) => void;
  onPendingRoom: (code: string, role: "host" | "guest") => void;
}

export default function HomeView(props: HomeViewProps) {
  const [joinCode, setJoinCode] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [copied, setCopied] = createSignal(false);
  const [qrDataUri, setQrDataUri] = createSignal("");
  const [showAdvanced, setShowAdvanced] = createSignal(false);
  const [advancedAddr, setAdvancedAddr] = createSignal(connection.gatewayAddr);
  const [error, setError] = createSignal<string | null>(null);
  const [pendingCode, setPendingCode] = createSignal<string | null>(null);

  async function handleCreate() {
    if (busy()) return;
    setBusy(true);
    setError(null);
    try {
      console.log("[P2P] creating link...");
      const result = await api.createLink();
      console.log("[P2P] link created:", result);
      setPendingCode(result.link_id);
      props.onPendingRoom(result.link_id, "host");
      try {
        const qr = await api.generateQr(result.link_id);
        setQrDataUri(qr);
      } catch { /* QR is optional */ }
    } catch (e) {
      console.error("[P2P] createLink error:", e);
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function handleJoin() {
    if (busy()) return;
    const code = joinCode().trim();
    if (!code) return;
    setBusy(true);
    setError(null);
    try {
      console.log("[P2P] joining link:", code);
      props.onPendingRoom(code, "guest");
      const remotePeerId = await api.joinLink(code);
      console.log("[P2P] joined, remotePeer:", remotePeerId);
      addPeer({
        peerId: remotePeerId,
        roomCode: code,
        role: "guest",
        displayName: shortName(remotePeerId),
        online: true,
      });
      setConnection({ status: "peer connected" });
      setJoinCode("");
      props.onNavigate("chat");
    } catch (e) {
      console.error("[P2P] joinLink error:", e);
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function handleRetry() {
    setConnection({ gatewayConnecting: true, gatewayError: null });
    try {
      await api.connectToGateway(advancedAddr());
      setConnection({ connected: true, gatewayConnecting: false, gatewayError: null, status: "connected", gatewayAddr: advancedAddr() });
    } catch (e) {
      setConnection({ gatewayConnecting: false, gatewayError: String(e) });
    }
  }

  function copyCode() {
    const code = pendingCode();
    if (!code) return;
    navigator.clipboard.writeText(code);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  }

  function handleNewRoom() {
    setPendingCode(null);
    setQrDataUri("");
    setError(null);
  }

  // State A: Connecting
  const isConnecting = () => connection.gatewayConnecting && !connection.connected;
  // State D: Error
  const isError = () => !connection.connected && !connection.gatewayConnecting && connection.gatewayError;
  // Connected (show create/join)
  const isReady = () => connection.connected;

  return (
    <div class="home-view">
      <Show when={isConnecting()}>
        <div class="home-connecting">
          <Spinner size={48} />
          <p>{t().home_connecting}</p>
        </div>
      </Show>

      <Show when={isError()}>
        <div class="home-error">
          <p class="error-msg">{connection.gatewayError}</p>
          <button class="btn-primary" onClick={handleRetry}>{t().home_retry}</button>
          <button class="advanced-toggle" onClick={() => setShowAdvanced(!showAdvanced())}>
            {showAdvanced() ? t().home_advanced_hide : t().home_advanced_show}
          </button>
          <Show when={showAdvanced()}>
            <div class="advanced-panel">
              <input
                type="text"
                value={advancedAddr()}
                onInput={(e) => setAdvancedAddr(e.currentTarget.value)}
                placeholder={t().home_host_port}
              />
              <button class="btn-secondary" onClick={handleRetry}>{t().home_connect}</button>
            </div>
          </Show>
        </div>
      </Show>

      <Show when={isReady()}>
        {/* Show active peers summary */}
        <Show when={connection.peers.length > 0}>
          <div class="active-peers-bar">
            <span>{t().status_active_chats(connection.peers.length)}</span>
            <button class="btn-secondary btn-sm" onClick={() => props.onNavigate("chat")}>{t().home_open_chats}</button>
          </div>
        </Show>

        {/* Show pending room waiting for peer */}
        <Show when={pendingCode()}>
          <div class="room-active">
            <h3>{t().home_room_code}</h3>
            <div class="room-code">{pendingCode()}</div>
            <div class="room-actions">
              <button class="btn-secondary" onClick={copyCode}>
                <Show when={copied()} fallback={<><CopyIcon width="16" height="16" /> {t().home_copy_code}</>}>
                  <CheckIcon width="16" height="16" /> {t().home_copied}
                </Show>
              </button>
              <button class="btn-secondary" onClick={handleNewRoom}>{t().home_new_room}</button>
            </div>
            <Show when={qrDataUri()}>
              <div class="qr-code">
                <img src={qrDataUri()} alt="QR code" width="180" height="180" />
              </div>
            </Show>
            <p class="room-waiting">{t().home_waiting}</p>
          </div>
        </Show>

        {/* Create / Join cards — always visible */}
        <Show when={!pendingCode()}>
          <div class="home-cards">
            <div class="card">
              <div class="card-icon">
                <LinkIcon width="24" height="24" />
              </div>
              <h3>{t().home_create_title}</h3>
              <p>{t().home_create_desc}</p>
              <button class="btn-primary" onClick={handleCreate} disabled={busy()}>
                {busy() ? t().home_creating : t().home_create_btn}
              </button>
            </div>

            <div class="card">
              <div class="card-icon">
                <UsersIcon width="24" height="24" />
              </div>
              <h3>{t().home_join_title}</h3>
              <p>{t().home_join_desc}</p>
              <div class="join-form">
                <input
                  type="text"
                  placeholder={t().home_join_placeholder}
                  value={joinCode()}
                  onInput={(e) => setJoinCode(e.currentTarget.value)}
                  onKeyDown={(e) => e.key === "Enter" && handleJoin()}
                />
                <button class="btn-primary" onClick={handleJoin} disabled={busy() || !joinCode().trim()}>
                  {t().home_join_btn}
                </button>
              </div>
            </div>
          </div>
        </Show>

        <Show when={error()}>
          <p class="error" style={{ "margin-top": "16px", "text-align": "center" }}>{error()}</p>
        </Show>
      </Show>
    </div>
  );
}

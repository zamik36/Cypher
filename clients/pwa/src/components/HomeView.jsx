import { createSignal, Show } from "solid-js";
import { api } from "../api";
import { connection, setConnection, addPeer, shortName } from "../stores/connection";
import Spinner from "./Spinner";
import { UsersIcon, LinkIcon, CopyIcon, CheckIcon } from "./Icons";
export default function HomeView(props) {
    const [joinCode, setJoinCode] = createSignal("");
    const [busy, setBusy] = createSignal(false);
    const [copied, setCopied] = createSignal(false);
    const [showAdvanced, setShowAdvanced] = createSignal(false);
    const [advancedAddr, setAdvancedAddr] = createSignal(connection.gatewayAddr);
    const [error, setError] = createSignal(null);
    const [pendingCode, setPendingCode] = createSignal(null);
    async function handleCreate() {
        setBusy(true);
        setError(null);
        try {
            const result = await api.createLink();
            setPendingCode(result.link_id);
            props.onPendingRoom(result.link_id, "host");
        }
        catch (e) {
            setError(String(e));
        }
        finally {
            setBusy(false);
        }
    }
    async function handleJoin() {
        const code = joinCode().trim();
        if (!code)
            return;
        setBusy(true);
        setError(null);
        try {
            props.onPendingRoom(code, "guest");
            const remotePeerId = await api.joinLink(code);
            addPeer({
                peerId: remotePeerId,
                roomCode: code,
                role: "guest",
                displayName: shortName(remotePeerId),
            });
            setConnection({ status: "peer connected" });
            setJoinCode("");
            props.onNavigate("chat");
        }
        catch (e) {
            setError(String(e));
        }
        finally {
            setBusy(false);
        }
    }
    async function handleRetry() {
        setConnection({ gatewayConnecting: true, gatewayError: null });
        try {
            await api.connectToGateway(advancedAddr());
        }
        catch (e) {
            setConnection({ gatewayConnecting: false, gatewayError: String(e) });
        }
    }
    function copyCode() {
        const code = pendingCode();
        if (!code)
            return;
        navigator.clipboard.writeText(code);
        setCopied(true);
        setTimeout(() => setCopied(false), 2000);
    }
    function handleNewRoom() {
        setPendingCode(null);
        setError(null);
    }
    const isConnecting = () => connection.gatewayConnecting && !connection.connected;
    const isError = () => !connection.connected && !connection.gatewayConnecting && connection.gatewayError;
    const isReady = () => connection.connected;
    return (<div class="home-view">
      <Show when={isConnecting()}>
        <div class="home-connecting">
          <Spinner size={48}/>
          <p>Connecting to network...</p>
        </div>
      </Show>

      <Show when={isError()}>
        <div class="home-error">
          <p class="error-msg">{connection.gatewayError}</p>
          <button class="btn-primary" onClick={handleRetry}>Retry</button>
          <button class="advanced-toggle" onClick={() => setShowAdvanced(!showAdvanced())}>
            {showAdvanced() ? "Hide advanced" : "Advanced options"}
          </button>
          <Show when={showAdvanced()}>
            <div class="advanced-panel">
              <input type="text" value={advancedAddr()} onInput={(e) => setAdvancedAddr(e.currentTarget.value)} placeholder="host:port"/>
              <button class="btn-secondary" onClick={handleRetry}>Connect</button>
            </div>
          </Show>
        </div>
      </Show>

      <Show when={isReady()}>
        <Show when={connection.peers.length > 0}>
          <div class="active-peers-bar">
            <span>{connection.peers.length} active chat{connection.peers.length > 1 ? "s" : ""}</span>
            <button class="btn-secondary btn-sm" onClick={() => props.onNavigate("chat")}>Open Chats</button>
          </div>
        </Show>

        <Show when={pendingCode()}>
          <div class="room-active">
            <h3>Your Room Code</h3>
            <div class="room-code">{pendingCode()}</div>
            <div class="room-actions">
              <button class="btn-secondary" onClick={copyCode}>
                <Show when={copied()} fallback={<><CopyIcon width="16" height="16"/> Copy Code</>}>
                  <CheckIcon width="16" height="16"/> Copied!
                </Show>
              </button>
              <button class="btn-secondary" onClick={handleNewRoom}>New Room</button>
            </div>
            <p class="room-waiting">Waiting for someone to join...</p>
          </div>
        </Show>

        <Show when={!pendingCode()}>
          <div class="home-cards">
            <div class="card">
              <div class="card-icon">
                <LinkIcon width="24" height="24"/>
              </div>
              <h3>Create Room</h3>
              <p>Start a private room and share the code with someone to connect.</p>
              <button class="btn-primary" onClick={handleCreate} disabled={busy()}>
                {busy() ? "Creating..." : "Create Room"}
              </button>
            </div>

            <div class="card">
              <div class="card-icon">
                <UsersIcon width="24" height="24"/>
              </div>
              <h3>Join Room</h3>
              <p>Enter a room code from someone to establish a secure connection.</p>
              <div class="join-form">
                <input type="text" placeholder="Enter room code" value={joinCode()} onInput={(e) => setJoinCode(e.currentTarget.value)} onKeyDown={(e) => e.key === "Enter" && handleJoin()}/>
                <button class="btn-primary" onClick={handleJoin} disabled={busy() || !joinCode().trim()}>
                  Join
                </button>
              </div>
            </div>
          </div>
        </Show>

        <Show when={error()}>
          <p class="error" style={{ "margin-top": "16px", "text-align": "center" }}>{error()}</p>
        </Show>
      </Show>
    </div>);
}

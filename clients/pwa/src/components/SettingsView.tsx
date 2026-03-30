import { createSignal, onCleanup } from "solid-js";
import { connection, setConnection } from "../stores/connection";
import { api } from "../api";
import { clearAll as clearMessageStore } from "../storage/messages";
import { clearAllMessages } from "../stores/chat";
import { exportSeed } from "../storage/identity";
import { addToast } from "../stores/toasts";

interface SettingsViewProps {
  theme: string;
  setTheme: (t: "dark" | "light") => void;
  nickname: string | null;
}

export default function SettingsView(props: SettingsViewProps) {
  const [addr, setAddr] = createSignal(connection.gatewayAddr);
  const [reconnecting, setReconnecting] = createSignal(false);
  const [confirmClear, setConfirmClear] = createSignal(false);
  const [clearCountdown, setClearCountdown] = createSignal(0);
  const [exportPass, setExportPass] = createSignal("");
  const [seedHex, setSeedHex] = createSignal<string | null>(null);

  async function handleReconnect() {
    setReconnecting(true);
    setConnection({ gatewayAddr: addr(), gatewayConnecting: true, gatewayError: null });
    try {
      await api.connectToGateway(addr());
    } catch (e) {
      setConnection({ gatewayConnecting: false, gatewayError: String(e) });
    } finally {
      setReconnecting(false);
    }
  }

  let clearIntervalId: ReturnType<typeof setInterval> | undefined;
  onCleanup(() => { if (clearIntervalId) clearInterval(clearIntervalId); });

  function startClearConfirmation() {
    setConfirmClear(true);
    setClearCountdown(3);
    clearIntervalId = setInterval(() => {
      setClearCountdown((n: number) => {
        if (n <= 1) { clearInterval(clearIntervalId!); clearIntervalId = undefined; return 0; }
        return n - 1;
      });
    }, 1000);
  }

  async function handleClearHistory() {
    try {
      await clearMessageStore();
      clearAllMessages();
      addToast("Chat history cleared", "success");
    } catch (e) {
      addToast(`Failed to clear: ${e}`, "error");
    }
    setConfirmClear(false);
  }

  async function handleExportSeed() {
    if (!exportPass()) return;
    try {
      const hex = await exportSeed(exportPass());
      setSeedHex(hex);
      // Auto-clear seed from display after 30 seconds for security.
      setTimeout(() => setSeedHex(null), 30_000);
    } catch (e) {
      addToast(String(e), "error");
    }
    setExportPass("");
  }

  return (
    <div class="settings-view">
      <h2>Settings</h2>

      {props.nickname && (
        <div class="settings-group">
          <label>Identity</label>
          <div class="about-info">
            <p>Nickname: <strong>{props.nickname}</strong></p>
            <p style={{ "font-size": "12px", "opacity": "0.7" }}>
              PeerId: {connection.peerId?.slice(0, 12)}...
            </p>
          </div>
        </div>
      )}

      <div class="settings-group">
        <label>Gateway Server</label>
        <div class="settings-row">
          <input
            type="text"
            value={addr()}
            onInput={(e: InputEvent & { currentTarget: HTMLInputElement }) => setAddr(e.currentTarget.value)}
            placeholder="host:port"
          />
          <button class="btn-secondary" onClick={handleReconnect} disabled={reconnecting()}>
            {reconnecting() ? "Connecting..." : "Reconnect"}
          </button>
        </div>
      </div>

      <div class="settings-group">
        <label>Theme</label>
        <div class="theme-options">
          <button
            class={`theme-option ${props.theme === "dark" ? "active" : ""}`}
            onClick={() => props.setTheme("dark")}
          >
            Dark
          </button>
          <button
            class={`theme-option ${props.theme === "light" ? "active" : ""}`}
            onClick={() => props.setTheme("light")}
          >
            Light
          </button>
        </div>
      </div>

      <div class="settings-group">
        <label>Export Seed (Backup)</label>
        <div class="settings-row">
          <input
            type="password"
            value={exportPass()}
            onInput={(e: InputEvent & { currentTarget: HTMLInputElement }) => setExportPass(e.currentTarget.value)}
            placeholder="Enter passphrase to export"
          />
          <button class="btn-secondary" onClick={handleExportSeed} disabled={!exportPass()}>
            Export
          </button>
        </div>
        {seedHex() && (
          <div class="seed-display">
            <code>{seedHex()}</code>
            <button
              class="btn-sm btn-secondary"
              onClick={() => {
                navigator.clipboard.writeText(seedHex()!);
                addToast("Seed copied!", "success");
              }}
            >
              Copy
            </button>
          </div>
        )}
      </div>

      <div class="settings-group">
        <label>Data</label>
        {!confirmClear() ? (
          <button class="btn-danger" onClick={startClearConfirmation}>
            Clear chat history
          </button>
        ) : (
          <div class="clear-confirm">
            <p class="clear-warning">
              All messages and chat history will be permanently deleted.
              Ratchet sessions will be reset — reconnecting to peers will
              require a new key exchange.
            </p>
            <button
              class="btn-danger"
              onClick={handleClearHistory}
              disabled={clearCountdown() > 0}
            >
              {clearCountdown() > 0
                ? `Confirm (${clearCountdown()}s)`
                : "Confirm delete"}
            </button>
            <button class="btn-secondary" onClick={() => setConfirmClear(false)}>
              Cancel
            </button>
          </div>
        )}
      </div>

      <div class="settings-group">
        <label>About</label>
        <div class="about-info">
          <p>Cypher v0.1.1 (PWA)</p>
          <p>Anonymous, end-to-end encrypted messaging.</p>
          <p>No accounts. No tracking. No logs.</p>
        </div>
      </div>
    </div>
  );
}

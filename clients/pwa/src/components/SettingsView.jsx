import { createSignal } from "solid-js";
import { connection, setConnection } from "../stores/connection";
import { api } from "../api";
export default function SettingsView(props) {
    const [addr, setAddr] = createSignal(connection.gatewayAddr);
    const [reconnecting, setReconnecting] = createSignal(false);
    async function handleReconnect() {
        setReconnecting(true);
        setConnection({ gatewayAddr: addr(), gatewayConnecting: true, gatewayError: null });
        try {
            await api.connectToGateway(addr());
        }
        catch (e) {
            setConnection({ gatewayConnecting: false, gatewayError: String(e) });
        }
        finally {
            setReconnecting(false);
        }
    }
    return (<div class="settings-view">
      <h2>Settings</h2>

      <div class="settings-group">
        <label>Gateway Server</label>
        <div class="settings-row">
          <input type="text" value={addr()} onInput={(e) => setAddr(e.currentTarget.value)} placeholder="host:port"/>
          <button class="btn-secondary" onClick={handleReconnect} disabled={reconnecting()}>
            {reconnecting() ? "Connecting..." : "Reconnect"}
          </button>
        </div>
      </div>

      <div class="settings-group">
        <label>Theme</label>
        <div class="theme-options">
          <button class={`theme-option ${props.theme === "dark" ? "active" : ""}`} onClick={() => props.setTheme("dark")}>
            Dark
          </button>
          <button class={`theme-option ${props.theme === "light" ? "active" : ""}`} onClick={() => props.setTheme("light")}>
            Light
          </button>
        </div>
      </div>

      <div class="settings-group">
        <label>About</label>
        <div class="about-info">
          <p>P2P Share v0.1.0 (PWA)</p>
          <p>Anonymous, end-to-end encrypted file sharing.</p>
          <p>No accounts. No tracking. No logs.</p>
          <p style={{ "margin-top": "8px", "font-size": "12px" }}>
            Add to Home Screen for the best experience on iOS.
          </p>
        </div>
      </div>
    </div>);
}

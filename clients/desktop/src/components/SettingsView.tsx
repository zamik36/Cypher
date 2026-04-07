import { createSignal, onCleanup } from "solid-js";
import { connection, setConnection } from "../stores/connection";
import { api } from "../api/tauri";
import { addToast } from "../stores/toasts";
import { t } from "../i18n";
import { locale, setLocale } from "../i18n";

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
      setConnection({ connected: true, gatewayConnecting: false, gatewayError: null, status: "connected" });
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
      await api.clearChatHistory();
      addToast(t().toast_history_cleared, "success");
    } catch (e) {
      addToast(t().toast_clear_failed(String(e)), "error");
    }
    setConfirmClear(false);
  }

  async function handleExportSeed() {
    if (!exportPass()) return;
    try {
      const hex = await api.exportMnemonic(exportPass());
      setSeedHex(hex);
      setTimeout(() => setSeedHex(null), 30_000);
    } catch (e) {
      addToast(String(e), "error");
    }
    setExportPass("");
  }

  return (
    <div class="settings-view">
      <h2>{t().settings_title}</h2>

      {props.nickname && (
        <div class="settings-group">
          <label>{t().settings_identity}</label>
          <div class="about-info">
            <p>{t().settings_nickname} <strong>{props.nickname}</strong></p>
            <p style={{ "font-size": "12px", "opacity": "0.7" }}>
              {t().settings_peerid} {connection.peerId?.slice(0, 12)}...
            </p>
          </div>
        </div>
      )}

      <div class="settings-group">
        <label>{t().settings_gateway}</label>
        <div class="settings-row">
          <input
            type="text"
            value={addr()}
            onInput={(e) => setAddr(e.currentTarget.value)}
            placeholder={t().home_host_port}
          />
          <button class="btn-secondary" onClick={handleReconnect} disabled={reconnecting()}>
            {reconnecting() ? t().settings_reconnecting : t().settings_reconnect}
          </button>
        </div>
      </div>

      <div class="settings-group">
        <label>{t().settings_theme}</label>
        <div class="theme-options">
          <button
            class={`theme-option ${props.theme === "dark" ? "active" : ""}`}
            onClick={() => props.setTheme("dark")}
          >
            {t().settings_dark}
          </button>
          <button
            class={`theme-option ${props.theme === "light" ? "active" : ""}`}
            onClick={() => props.setTheme("light")}
          >
            {t().settings_light}
          </button>
        </div>
      </div>

      <div class="settings-group">
        <label>{t().settings_language}</label>
        <div class="theme-options">
          <button
            class={`theme-option ${locale() === "en" ? "active" : ""}`}
            onClick={() => setLocale("en")}
          >
            English
          </button>
          <button
            class={`theme-option ${locale() === "ru" ? "active" : ""}`}
            onClick={() => setLocale("ru")}
          >
            Русский
          </button>
        </div>
      </div>

      {props.nickname && (
        <div class="settings-group">
          <label>{t().settings_export}</label>
          <div class="settings-row">
            <input
              type="password"
              value={exportPass()}
              onInput={(e: InputEvent & { currentTarget: HTMLInputElement }) => setExportPass(e.currentTarget.value)}
              placeholder={t().settings_export_placeholder}
            />
            <button class="btn-secondary" onClick={handleExportSeed} disabled={!exportPass()}>
              {t().settings_export_btn}
            </button>
          </div>
          {seedHex() && (
            <div class="seed-display">
              <code>{seedHex()}</code>
              <button
                class="btn-sm btn-secondary"
                onClick={() => {
                  navigator.clipboard.writeText(seedHex()!);
                  addToast(t().toast_seed_copied, "success");
                }}
              >
                {t().settings_copy}
              </button>
            </div>
          )}
        </div>
      )}

      {props.nickname && (
        <div class="settings-group">
          <label>{t().settings_data}</label>
          {!confirmClear() ? (
            <button class="btn-danger" onClick={startClearConfirmation}>
              {t().settings_clear}
            </button>
          ) : (
            <div class="clear-confirm">
              <p class="clear-warning">
                {t().settings_clear_warning}
              </p>
              <button
                class="btn-danger"
                onClick={handleClearHistory}
                disabled={clearCountdown() > 0}
              >
                {t().settings_clear_confirm(clearCountdown())}
              </button>
              <button class="btn-secondary" onClick={() => setConfirmClear(false)}>
                {t().settings_cancel}
              </button>
            </div>
          )}
        </div>
      )}

      <div class="settings-group">
        <label>{t().settings_about}</label>
        <div class="about-info">
          <p>{t().settings_version}</p>
          <p>{t().settings_about_desc}</p>
          <p>{t().settings_about_motto}</p>
        </div>
      </div>
    </div>
  );
}

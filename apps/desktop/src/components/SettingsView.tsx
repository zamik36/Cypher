import { createSignal, onCleanup, onMount, Show } from "solid-js";
import { connection, setConnection, setGatewayAddr } from "../stores/connection";
import { api } from "../api/tauri";
import { addToast } from "../stores/toasts";
import { t } from "../i18n";
import { locale, setLocale } from "../i18n";
import { anonymousSettings, anonymityStatus, setAnonymousSettings } from "../stores/anonymity";
import {
  notificationPermissionState,
  notificationsEnabled,
  notificationsSupported,
  previewEnabled,
  requestNotificationAccess,
  setNotificationsEnabled,
  setPreviewEnabled,
} from "../utils/notifications";

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
  const [anonymousEnabled, setAnonymousEnabled] = createSignal(anonymousSettings.enabled);
  const [bridgeLines, setBridgeLines] = createSignal(anonymousSettings.bridgeLines.join("\n"));
  const [savingAnonymous, setSavingAnonymous] = createSignal(false);
  const [notifEnabled, setNotifEnabled] = createSignal(notificationsEnabled());
  const [prevEnabled, setPrevEnabled] = createSignal(previewEnabled());
  const [notificationPermission, setNotificationPermission] =
    createSignal<NotificationPermission>("default");

  onMount(() => {
    void notificationPermissionState().then(setNotificationPermission);
  });

  async function handleToggleNotifications() {
    if (!notifEnabled()) {
      const permission = await requestNotificationAccess();
      setNotificationPermission(permission);
      if (permission === "granted") {
        setNotificationsEnabled(true);
        setNotifEnabled(true);
        addToast(t().toast_notif_enabled, "success");
      } else {
        addToast(t().toast_notif_denied, "error");
      }
      return;
    }

    setNotificationsEnabled(false);
    setNotifEnabled(false);
    addToast(t().toast_notif_disabled, "success");
  }

  async function handleReconnect() {
    const normalizedAddr = setGatewayAddr(addr());
    setAddr(normalizedAddr);
    setReconnecting(true);
    setConnection({ gatewayConnecting: true, gatewayError: null });
    try {
      await api.connectToGateway(normalizedAddr);
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

  async function handleSaveAnonymousSettings() {
    const nextSettings = {
      enabled: anonymousEnabled(),
      bridgeLines: bridgeLines()
        .split(/\r?\n/)
        .map((line) => line.trim())
        .filter(Boolean),
    };

    setSavingAnonymous(true);
    try {
      await api.applyAnonymousSettings(nextSettings.enabled, nextSettings.bridgeLines);
      setAnonymousSettings(nextSettings);
      addToast(t().toast_anonymous_saved, "success");
    } catch (e) {
      addToast(t().toast_anonymous_save_failed(String(e)), "error");
    } finally {
      setSavingAnonymous(false);
    }
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

      <Show when={notificationsSupported()}>
        <div class="settings-group">
          <label>{t().settings_notifications}</label>
          <div class="settings-row">
            <span style={{ flex: 1, "font-size": "13px" }}>
              {notificationPermission() === "denied"
                ? t().settings_notif_blocked
                : notifEnabled()
                  ? t().settings_notif_on
                  : t().settings_notif_off}
            </span>
            <button
              class={notifEnabled() ? "btn-secondary" : "btn-primary"}
              onClick={handleToggleNotifications}
              disabled={notificationPermission() === "denied"}
            >
              {notifEnabled() ? t().settings_notif_disable : t().settings_notif_enable}
            </button>
          </div>
          <Show when={notifEnabled()}>
            <div class="settings-row" style={{ "margin-top": "8px" }}>
              <span style={{ flex: 1, "font-size": "13px" }}>
                {prevEnabled()
                  ? t().settings_preview_shown
                  : t().settings_preview_hidden}
              </span>
              <button
                class="btn-secondary"
                onClick={() => {
                  const next = !prevEnabled();
                  setPreviewEnabled(next);
                  setPrevEnabled(next);
                }}
              >
                {prevEnabled() ? t().settings_preview_hide : t().settings_preview_show}
              </button>
            </div>
          </Show>
        </div>
      </Show>

      <div class="settings-group">
        <label>{t().settings_anonymous_title}</label>
        <div class="anonymous-status-card">
          <div>
            <strong>{anonymityStatus.label}</strong>
            <p>{anonymityStatus.description}</p>
          </div>
          <span class={`status-chip ${anonymousEnabled() ? "enabled" : "disabled"}`}>
            {anonymousEnabled() ? t().settings_anonymous_enabled : t().settings_anonymous_disabled}
          </span>
        </div>
        <div class="theme-options" style={{ "margin-top": "12px" }}>
          <button
            class={`theme-option ${anonymousEnabled() ? "active" : ""}`}
            onClick={() => setAnonymousEnabled(true)}
          >
            {t().settings_anonymous_enabled}
          </button>
          <button
            class={`theme-option ${!anonymousEnabled() ? "active" : ""}`}
            onClick={() => setAnonymousEnabled(false)}
          >
            {t().settings_anonymous_disabled}
          </button>
        </div>
        <p class="settings-help">{t().settings_anonymous_help}</p>
        <label class="settings-subtitle">{t().settings_bridges_label}</label>
        <textarea
          value={bridgeLines()}
          onInput={(e) => setBridgeLines(e.currentTarget.value)}
          placeholder={t().settings_bridges_placeholder}
        />
        <p class="settings-help">{t().settings_bridges_help}</p>
        <button
          class="btn-secondary"
          style={{ "margin-top": "10px" }}
          onClick={handleSaveAnonymousSettings}
          disabled={savingAnonymous()}
        >
          {savingAnonymous() ? t().settings_anonymous_saving : t().settings_anonymous_apply}
        </button>
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

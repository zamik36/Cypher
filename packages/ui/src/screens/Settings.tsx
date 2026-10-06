import { createMemo, createSignal, For, Match, onCleanup, onMount, Show, Switch, type JSX } from "solid-js";
import "./Settings.css";
import Avatar from "../components/Avatar";
import Icon, { type IconName } from "../components/Icon";
import TopBar from "../components/TopBar";
import { api } from "../platform";
import { connection, connectGateway, setGatewayAddr } from "../stores/connection";
import { anonymousSettings, onionUp, setAnonymousSettings } from "../stores/anonymity";
import { clearAllMessages } from "../stores/chat";
import { blockedContacts, displayName, loadConversations, setContactBlocked } from "../stores/contacts";
import { isWide } from "../stores/layout";
import { back, push, type SettingsSection } from "../stores/nav";
import { nickname } from "../stores/profile";
import { setThemePref, themePref, type ThemePref } from "../stores/theme";
import { closeToTray, setCloseToTray } from "../stores/window";
import { addToast, toastError } from "../stores/toasts";
import { checkBridges } from "../utils/bridges";
import { copyText } from "../utils/clipboard";
import {
  notificationPermissionState,
  notificationsEnabled,
  notificationsSupported,
  previewEnabled,
  requestNotificationAccess,
  setNotificationsEnabled,
  setPreviewEnabled,
} from "../utils/notifications";
import { reasonText } from "../utils/reasons";
import { AUTO_LOCK_CHOICES, autoLock, lockNow, lockOnHide, setAutoLock, setLockOnHide } from "../stores/lock";
import { APP_VERSION } from "../version";
import { locale, setLocale, t, type Locale } from "../i18n";

const SECTIONS: readonly { id: SettingsSection; icon: IconName }[] = [
  { id: "appearance", icon: "palette" },
  { id: "notifications", icon: "bell" },
  { id: "privacy", icon: "lock" },
  { id: "storage", icon: "database" },
];

function sectionTitle(id: SettingsSection): string {
  const tr = t();
  return {
    profile: tr.settings_profile,
    appearance: tr.settings_appearance,
    notifications: tr.settings_notifications,
    privacy: tr.settings_privacy,
    storage: tr.settings_storage,
  }[id];
}

/** A switch row: label, optional hint, and the switch itself. */
function SwitchRow(props: {
  label: string;
  hint?: string;
  checked: boolean;
  onChange: () => void;
  disabled?: boolean;
}) {
  return (
    <div class="list-row">
      <span class="list-row__text">
        <span class="list-row__title">{props.label}</span>
        <Show when={props.hint}>
          <span class="list-row__subtitle">{props.hint}</span>
        </Show>
      </span>
      <button
        class="switch"
        role="switch"
        aria-checked={props.checked}
        aria-label={props.label}
        disabled={props.disabled}
        onClick={() => props.onChange()}
      />
    </div>
  );
}

function Root() {
  const ownId = () => connection.peerId ?? "";
  return (
    <>
      <button
        class="card settings__profile"
        aria-label={`${t().settings_profile}: ${nickname() ?? ""}`}
        onClick={() => push({ name: "settings-section", section: "profile" })}
      >
        <Avatar peerId={ownId()} name={nickname() ?? "?"} size={56} />
        <span class="list-row__text">
          <span class="settings__nick">{nickname()}</span>
          <span class="list-row__subtitle mono">{ownId().slice(0, 16)}…</span>
          <span class="list-row__subtitle">{t().settings_profile_hint}</span>
        </span>
        <Icon name="chevron-right" class="list-row__chevron" />
      </button>

      <div class="list settings__list">
        <For each={SECTIONS}>
          {(section) => (
            <button
              class="list-row"
              onClick={() => push({ name: "settings-section", section: section.id })}
              aria-label={sectionTitle(section.id)}
            >
              <span class="list-row__icon">
                <Icon name={section.icon} size={18} />
              </span>
              <span class="list-row__text">
                <span class="list-row__title">{sectionTitle(section.id)}</span>
              </span>
              <Icon name="chevron-right" class="list-row__chevron" />
            </button>
          )}
        </For>
      </div>

      <div class="settings__about">
        <p>{t().settings_version(APP_VERSION)}</p>
        <p class="muted">{t().settings_about_desc}</p>
        <p class="muted">{t().settings_about_motto}</p>
      </div>
    </>
  );
}

function Profile() {
  const [passphrase, setPassphrase] = createSignal("");
  const [phrase, setPhrase] = createSignal<string | null>(null);
  let hide: ReturnType<typeof setTimeout> | undefined;
  onCleanup(() => clearTimeout(hide));

  async function exportPhrase() {
    try {
      setPhrase(await api.exportMnemonic(passphrase()));
      clearTimeout(hide);
      hide = setTimeout(() => setPhrase(null), 60_000);
    } catch (e) {
      toastError(e);
    }
    setPassphrase("");
  }

  return (
    <>
      <h3 class="group-title">{t().profile_nickname}</h3>
      <div class="card settings__card">
        <p class="settings__nick">{nickname()}</p>
        <p class="hint">{t().profile_nickname_hint}</p>
      </div>

      <h3 class="group-title">{t().profile_id}</h3>
      <div class="card settings__card">
        <p class="mono settings__id" data-testid="own-peer-id">
          {connection.peerId}
        </p>
        <button class="btn btn--secondary" onClick={() => void copyText(connection.peerId ?? "")}>
          <Icon name="copy" size={18} /> {t().common_copy}
        </button>
      </div>

      <h3 class="group-title">{t().profile_backup}</h3>
      <div class="card settings__card">
        <p class="hint">{t().profile_backup_hint}</p>
        <Show
          when={phrase()}
          fallback={
            <form
              class="settings__row"
              onSubmit={(e) => {
                e.preventDefault();
                if (passphrase()) void exportPhrase();
              }}
            >
              <input
                class="field"
                type="password"
                placeholder={t().profile_backup_placeholder}
                aria-label={t().profile_backup_placeholder}
                value={passphrase()}
                onInput={(e) => setPassphrase(e.currentTarget.value)}
                autocomplete="current-password"
              />
              <button class="btn btn--primary" type="submit" disabled={!passphrase()}>
                {t().profile_backup_show}
              </button>
            </form>
          }
        >
          {(words) => (
            <>
              <ol class="recovery-words" data-testid="recovery-phrase" aria-label={words()}>
                <For each={words().split(" ")}>{(word) => <li>{word}</li>}</For>
              </ol>
              <div class="settings__row">
                <button
                  class="btn btn--secondary"
                  onClick={() => void copyText(words()).then((ok) => ok && addToast(t().toast_seed_copied, "success"))}
                >
                  <Icon name="copy" size={18} /> {t().common_copy}
                </button>
                <button class="btn btn--ghost" onClick={() => setPhrase(null)}>
                  {t().profile_backup_hide}
                </button>
              </div>
            </>
          )}
        </Show>
      </div>
    </>
  );
}

function Appearance() {
  const themes: readonly ThemePref[] = ["system", "dark", "light"];
  const themeName = (choice: ThemePref) =>
    ({ system: t().settings_system, dark: t().settings_dark, light: t().settings_light })[choice];
  const languages: readonly { id: Locale; name: string }[] = [
    { id: "ru", name: "Русский" },
    { id: "en", name: "English" },
  ];
  return (
    <>
      <h3 class="group-title">{t().settings_theme}</h3>
      <div class="segmented">
        <For each={themes}>
          {(choice) => (
            <button aria-pressed={themePref() === choice} onClick={() => setThemePref(choice)}>
              {themeName(choice)}
            </button>
          )}
        </For>
      </div>
      <h3 class="group-title">{t().settings_language}</h3>
      <div class="segmented">
        <For each={languages}>
          {(language) => (
            <button aria-pressed={locale() === language.id} onClick={() => setLocale(language.id)}>
              {language.name}
            </button>
          )}
        </For>
      </div>
      <Show when={api.shell}>
        <h3 class="group-title">{t().settings_window}</h3>
        <div class="list">
          <SwitchRow
            label={t().settings_close_to_tray}
            hint={t().settings_close_to_tray_hint}
            checked={closeToTray()}
            onChange={() => setCloseToTray(!closeToTray())}
          />
        </div>
      </Show>
    </>
  );
}

function Notifications() {
  const [enabled, setEnabled] = createSignal(notificationsEnabled());
  const [preview, setPreview] = createSignal(previewEnabled());
  const [permission, setPermission] = createSignal<NotificationPermission>("default");
  onMount(() => void notificationPermissionState().then(setPermission));

  async function toggle() {
    if (enabled()) {
      setNotificationsEnabled(false);
      setEnabled(false);
      return;
    }
    const granted = await requestNotificationAccess();
    setPermission(granted);
    if (granted === "granted") {
      setNotificationsEnabled(true);
      setEnabled(true);
    } else {
      addToast(t().toast_notif_denied, "error");
    }
  }

  return (
    <Show when={notificationsSupported()} fallback={<p class="hint">{t().notif_unsupported}</p>}>
      <div class="list">
        <SwitchRow label={t().notif_messages} checked={enabled()} onChange={() => void toggle()} />
        <SwitchRow
          label={t().notif_preview}
          hint={t().notif_preview_hint}
          checked={preview()}
          disabled={!enabled()}
          onChange={() => {
            setPreviewEnabled(!preview());
            setPreview(!preview());
          }}
        />
      </div>
      <Show when={permission() === "denied"}>
        <p class="error-text settings__note">{t().settings_notif_blocked}</p>
      </Show>
    </Show>
  );
}

/** "Never", "1 min", "1 h"... */
function lockLabel(minutes: number): string {
  if (minutes === 0) return t().lock_never;
  return minutes < 60 ? t().lock_minutes(minutes) : t().lock_hours(minutes / 60);
}

function Privacy() {
  const caps = api.capabilities;
  const [anonymous, setAnonymous] = createSignal(anonymousSettings.enabled);
  const [bridges, setBridges] = createSignal(anonymousSettings.bridgeLines.join("\n"));
  const [address, setAddress] = createSignal(connection.gatewayAddr);
  const [advanced, setAdvanced] = createSignal(false);
  const [applying, setApplying] = createSignal(false);
  const bridgeCheck = createMemo(() => checkBridges(caps.tor ? bridges() : ""));
  const bridgeLines = () => bridgeCheck().lines;
  const bridgeProblem = () => {
    const problem = bridgeCheck().problem;
    if (!problem) return null;
    return problem.reason === "transport"
      ? t().privacy_bridges_transport(problem.line)
      : t().privacy_bridges_format(problem.line);
  };
  const changed = () =>
    anonymous() !== anonymousSettings.enabled || bridgeLines().join("\n") !== anonymousSettings.bridgeLines.join("\n");
  const statusTitle = () =>
    onionUp() === null ? t().anon_status_unknown : onionUp() ? t().anon_status_onion : t().anon_status_direct;
  const statusText = () =>
    onionUp() === null ? t().anon_desc_unknown : onionUp() ? t().anon_desc_onion : t().anon_desc_direct;

  async function apply() {
    const next = { enabled: anonymous(), bridgeLines: bridgeLines() };
    setApplying(true);
    try {
      await api.applyAnonymousSettings(next.enabled, next.bridgeLines);
      setAnonymousSettings(next);
      addToast(t().toast_anonymous_saved, "success");
    } catch (e) {
      addToast(t().toast_anonymous_save_failed(reasonText(e)), "error");
    } finally {
      setApplying(false);
    }
  }

  async function unblock(peerId: string) {
    try {
      await api.setBlocked(peerId, false);
      setContactBlocked(peerId, false);
      addToast(t().toast_unblocked, "success");
    } catch (e) {
      toastError(e);
    }
  }

  async function reconnect() {
    const normalized = setGatewayAddr(address());
    setAddress(normalized);
    await connectGateway(() =>
      api.connectToGateway(normalized, anonymousSettings.enabled, anonymousSettings.bridgeLines),
    );
  }

  return (
    <>
      <h3 class="group-title">{t().lock_title}</h3>
      <div class="segmented" role="radiogroup" aria-label={t().lock_auto}>
        <For each={AUTO_LOCK_CHOICES}>
          {(minutes) => (
            <button aria-pressed={autoLock() === minutes} onClick={() => setAutoLock(minutes)}>
              {lockLabel(minutes)}
            </button>
          )}
        </For>
      </div>
      <div class="list settings__list">
        <SwitchRow
          label={t().lock_on_hide}
          hint={t().lock_on_hide_hint}
          checked={lockOnHide()}
          onChange={() => setLockOnHide(!lockOnHide())}
        />
        <button class="list-row" onClick={lockNow}>
          <span class="list-row__icon">
            <Icon name="lock" size={18} />
          </span>
          <span class="list-row__text">
            <span class="list-row__title">{t().lock_now}</span>
            <span class="list-row__subtitle">{t().lock_hint}</span>
          </span>
        </button>
      </div>

      <h3 class="group-title">{t().privacy_route}</h3>
      <div class="card settings__card settings__status">
        <span class="settings__dot" data-on={onionUp() === true} />
        <span class="list-row__text">
          <span class="list-row__title">{statusTitle()}</span>
          <span class="list-row__subtitle">{statusText()}</span>
        </span>
      </div>

      <div class="list settings__list">
        <SwitchRow
          label={t().privacy_onion_only}
          hint={caps.tor ? t().privacy_onion_only_native : t().privacy_onion_only_web}
          checked={anonymous()}
          onChange={() => setAnonymous(!anonymous())}
        />
      </div>

      <Show when={caps.tor}>
        <h3 class="group-title">{t().privacy_bridges}</h3>
        <textarea
          class="field mono settings__bridges"
          rows={3}
          placeholder={t().privacy_bridges_placeholder}
          aria-label={t().privacy_bridges}
          value={bridges()}
          onInput={(e) => setBridges(e.currentTarget.value)}
          spellcheck={false}
        />
        <Show when={bridgeProblem()} fallback={<p class="hint settings__note">{t().privacy_bridges_hint}</p>}>
          {(problem) => (
            <p class="error-text settings__note" role="alert">
              {problem()}
            </p>
          )}
        </Show>
      </Show>

      <Show when={changed()}>
        <button
          class="btn btn--primary btn--block settings__apply"
          disabled={applying() || bridgeProblem() !== null}
          onClick={() => void apply()}
        >
          {applying() ? t().common_applying : t().common_apply}
        </button>
      </Show>

      <h3 class="group-title">{t().privacy_blocked}</h3>
      <div class="list">
        <For each={blockedContacts()} fallback={<p class="hint settings__note">{t().privacy_blocked_none}</p>}>
          {(contact) => (
            <div class="list-row">
              <span class="list-row__text">
                <span class="list-row__title">{displayName(contact.peerId)}</span>
              </span>
              <button class="btn btn--secondary" onClick={() => void unblock(contact.peerId)}>
                {t().contact_unblock}
              </button>
            </div>
          )}
        </For>
      </div>

      <Show when={caps.gatewayConfig}>
        <button class="btn btn--ghost settings__advanced" onClick={() => setAdvanced(!advanced())}>
          {t().privacy_advanced}
        </button>
        <Show when={advanced()}>
          <div class="card settings__card">
            <label class="label" for="gateway-address">
              {t().privacy_server}
            </label>
            <div class="settings__row">
              <input
                id="gateway-address"
                class="field mono"
                type="text"
                value={address()}
                onInput={(e) => setAddress(e.currentTarget.value)}
                autocapitalize="off"
                autocomplete="off"
                spellcheck={false}
              />
              <button class="btn btn--secondary" onClick={() => void reconnect()}>
                {t().privacy_reconnect}
              </button>
            </div>
            <p class="hint">{t().privacy_server_hint}</p>
          </div>
        </Show>
      </Show>
    </>
  );
}

function Storage() {
  const [confirming, setConfirming] = createSignal(false);
  const [countdown, setCountdown] = createSignal(0);
  let timer: ReturnType<typeof setInterval> | undefined;
  onCleanup(() => clearInterval(timer));

  function startConfirm() {
    setConfirming(true);
    setCountdown(3);
    clearInterval(timer);
    timer = setInterval(() => {
      setCountdown((n) => Math.max(0, n - 1));
      if (countdown() === 0) clearInterval(timer);
    }, 1000);
  }

  async function clear() {
    try {
      await api.clearChatHistory();
      clearAllMessages();
      loadConversations(await api.getConversations());
      addToast(t().toast_history_cleared, "success");
    } catch (e) {
      addToast(t().toast_clear_failed(reasonText(e)), "error");
    }
    setConfirming(false);
  }

  return (
    <>
      <h3 class="group-title">{t().storage_files}</h3>
      <div class="card settings__card">
        <p class="hint">
          {api.kind === "web"
            ? t().storage_files_web
            : api.capabilities.revealFile
              ? t().storage_files_desktop
              : t().storage_files_android}
        </p>
      </div>

      <h3 class="group-title">{t().storage_history}</h3>
      <div class="card settings__card">
        <p class="hint">{t().storage_clear_hint}</p>
        <Show
          when={confirming()}
          fallback={
            <button class="btn btn--danger" onClick={startConfirm}>
              <Icon name="trash" size={18} /> {t().storage_clear}
            </button>
          }
        >
          <div class="settings__row">
            <button class="btn btn--secondary" onClick={() => setConfirming(false)}>
              {t().common_cancel}
            </button>
            <button class="btn btn--danger" disabled={countdown() > 0} onClick={() => void clear()}>
              {t().storage_clear_confirm(countdown())}
            </button>
          </div>
        </Show>
      </div>
    </>
  );
}

/** Settings: the root list, or one of its sections. */
export default function Settings(props: { section?: SettingsSection }) {
  const body = (): JSX.Element => (
    <Switch fallback={<Root />}>
      <Match when={props.section === "profile"}>
        <Profile />
      </Match>
      <Match when={props.section === "appearance"}>
        <Appearance />
      </Match>
      <Match when={props.section === "notifications"}>
        <Notifications />
      </Match>
      <Match when={props.section === "privacy"}>
        <Privacy />
      </Match>
      <Match when={props.section === "storage"}>
        <Storage />
      </Match>
    </Switch>
  );
  return (
    <section class="screen settings">
      <TopBar
        title={props.section ? sectionTitle(props.section) : t().settings_title}
        onBack={props.section || !isWide() ? back : undefined}
      />
      <div class="screen__body">
        <div class="content">{body()}</div>
      </div>
    </section>
  );
}

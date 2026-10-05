import { createEffect, createSignal, Match, on, Show, Switch, untrack } from "solid-js";
import "./NewChat.css";
import Icon from "../components/Icon";
import QrScanner from "../components/QrScanner";
import TopBar from "../components/TopBar";
import { api } from "../platform";
import { isOnline } from "../stores/connection";
import { ensureContact } from "../stores/contacts";
import { back, replace } from "../stores/nav";
import { copyText } from "../utils/clipboard";
import { findInvite } from "../utils/invite";
import { reasonText } from "../utils/reasons";
import { t } from "../i18n";

type Tab = "invite" | "join";

/** Shows a fresh invite code with its QR code until someone uses it. */
function InviteTab() {
  const [code, setCode] = createSignal<string | null>(null);
  const [qr, setQr] = createSignal("");
  const [error, setError] = createSignal<string | null>(null);
  const [copied, setCopied] = createSignal(false);
  let creating = false;

  async function create() {
    if (creating) return;
    creating = true;
    setError(null);
    setCode(null);
    setQr("");
    try {
      const { link_id } = await api.createLink();
      setCode(link_id);
      setQr(await api.generateQr(link_id).catch(() => ""));
    } catch (e) {
      setError(reasonText(e));
    } finally {
      creating = false;
    }
  }

  // A code needs the server: make one as soon as the app is online.
  createEffect(
    on(isOnline, (online) => {
      if (online && !code()) void create();
    }),
  );

  async function copy() {
    const value = code();
    if (!value || !(await copyText(value))) return;
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  }

  const share = () => {
    const value = code();
    if (value) void navigator.share({ text: t().invite_share_text(value) }).catch(() => undefined);
  };

  return (
    <div class="new-chat__panel">
      <p class="hint">{t().invite_hint}</p>
      <Switch>
        <Match when={code()}>
          {(value) => (
            <>
              <div class="invite-qr">
                <Show when={qr()} fallback={<span class="spinner" />}>
                  <img src={qr()} alt={t().invite_qr_alt} />
                </Show>
              </div>
              <p class="invite-code mono" data-testid="invite-code">
                {value()}
              </p>
              <div class="new-chat__actions">
                <button class="btn btn--primary" onClick={() => void copy()}>
                  <Icon name={copied() ? "check" : "copy"} /> {copied() ? t().invite_copied : t().invite_copy}
                </button>
                <Show when={typeof navigator.share === "function"}>
                  <button class="btn btn--secondary" onClick={share}>
                    <Icon name="share" /> {t().invite_share}
                  </button>
                </Show>
              </div>
              <p class="invite-waiting muted">
                <span class="spinner" /> {t().invite_waiting}
              </p>
              <button class="btn btn--ghost" onClick={() => void create()}>
                {t().invite_new}
              </button>
            </>
          )}
        </Match>
        <Match when={error()}>
          {(message) => (
            <>
              <p class="error-text" role="alert">
                {message()}
              </p>
              <button class="btn btn--secondary" onClick={() => void create()}>
                {t().common_retry}
              </button>
            </>
          )}
        </Match>
        <Match when={!isOnline()}>
          <p class="muted">{t().invite_offline}</p>
        </Match>
        <Match when={true}>
          <span class="spinner" />
        </Match>
      </Switch>
    </div>
  );
}

/** Joins someone's chat with the invite code they sent. */
function JoinTab() {
  const [input, setInput] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [scanning, setScanning] = createSignal(false);
  const canScan = Boolean(api.scanQr) || "mediaDevices" in navigator;

  async function join() {
    const code = findInvite(input());
    if (!code) {
      setError(t().join_invalid_link);
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const peerId = await api.joinLink(code);
      ensureContact(peerId, true);
      replace({ name: "chat", peerId });
    } catch (e) {
      setError(reasonText(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <form
      class="new-chat__panel"
      onSubmit={(e) => {
        e.preventDefault();
        void join();
      }}
    >
      <Show when={canScan}>
        <button class="btn btn--secondary btn--block" type="button" onClick={() => setScanning(true)}>
          <Icon name="scan" size={18} /> {t().scan_button}
        </button>
      </Show>
      <Show when={scanning()}>
        <QrScanner
          onCode={(code) => {
            setScanning(false);
            setInput(code);
            void join();
          }}
          onClose={() => setScanning(false)}
        />
      </Show>
      <p class="hint">{t().join_hint}</p>
      <input
        class="field mono"
        type="text"
        placeholder={t().join_placeholder}
        aria-label={t().join_placeholder}
        value={input()}
        onInput={(e) => setInput(e.currentTarget.value)}
        autocomplete="off"
        autocapitalize="off"
        spellcheck={false}
      />
      <Show when={error()}>
        {(message) => (
          <p class="error-text" role="alert">
            {message()}
          </p>
        )}
      </Show>
      <button class="btn btn--primary btn--block" type="submit" disabled={busy() || input().trim() === ""}>
        <Show when={busy()} fallback={t().join_button}>
          {t().join_joining}
        </Show>
      </button>
    </form>
  );
}

export default function NewChat(props: { tab: Tab }) {
  const [tab, setTab] = createSignal<Tab>(untrack(() => props.tab));
  return (
    <section class="screen">
      <TopBar title={t().chats_new} onBack={back} />
      <div class="screen__body">
        <div class="content new-chat">
          <div class="segmented" role="tablist">
            <button role="tab" aria-selected={tab() === "invite"} onClick={() => setTab("invite")}>
              {t().new_tab_invite}
            </button>
            <button role="tab" aria-selected={tab() === "join"} onClick={() => setTab("join")}>
              {t().new_tab_join}
            </button>
          </div>
          <Show when={tab() === "invite"} fallback={<JoinTab />}>
            <InviteTab />
          </Show>
        </div>
      </div>
    </section>
  );
}

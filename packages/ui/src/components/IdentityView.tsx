import { createSignal, For, Show, createMemo, onCleanup, onMount, type JSX } from "solid-js";
import "./Onboarding.css";
import Icon from "./Icon";
import { api } from "../platform";
import { t } from "../i18n";
import { connection } from "../stores/connection";
import { copyText } from "../utils/clipboard";
import { reasonText } from "../utils/reasons";
import { answersMatch, phraseWords, pickPositions } from "../utils/recovery";

/** The core's minimum (`identity_file::MIN_PASSPHRASE_CHARS`), counted in characters. */
const MIN_PASSPHRASE_CHARS = 12;

interface IdentityViewProps {
  onUnlocked: (peerId: string, nickname: string) => void;
}

/**
 * `forgot` explains a lost passphrase and offers to erase the device;
 * `link` makes this a new device of a profile used elsewhere.
 */
type Mode = "unlock" | "create" | "import" | "backup" | "forgot" | "link";

/** A new identity, held back until its recovery phrase has been written down. */
interface Pending {
  peerId: string;
  nickname: string;
}

/** Length in Unicode code points, as the core counts it (Rust `chars()`). */
const chars = (s: string) => Array.from(s).length;

export default function IdentityView(props: IdentityViewProps) {
  const [hasId, setHasId] = createSignal<boolean | null>(null);
  const [mode, setMode] = createSignal<Mode>("unlock");
  const [nickname, setNickname] = createSignal("");
  const [passphrase, setPassphrase] = createSignal("");
  const [repeat, setRepeat] = createSignal("");
  const [mnemonic, setMnemonic] = createSignal("");
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);

  const [pending, setPending] = createSignal<Pending | null>(null);
  const [words, setWords] = createSignal<string[]>([]);
  const [checking, setChecking] = createSignal(false);
  const [positions, setPositions] = createSignal<number[]>([]);
  const [answers, setAnswers] = createSignal<string[]>([]);
  const [eraseIn, setEraseIn] = createSignal(0);
  const [deviceName, setDeviceName] = createSignal("");
  /** The offer this device shows while it waits to be linked, and its QR. */
  const [offer, setOffer] = createSignal<string | null>(null);
  const [offerQr, setOfferQr] = createSignal("");
  let eraseTimer: ReturnType<typeof setInterval> | undefined;
  onCleanup(() => clearInterval(eraseTimer));

  onMount(() => {
    api
      .hasIdentity()
      .then((exists) => {
        setHasId(exists);
        if (!exists) setMode("create");
      })
      .catch((e: unknown) => {
        setHasId(false);
        setMode("create");
        setError(reasonText(e));
      });
  });

  function switchMode(next: Mode) {
    setPassphrase("");
    setRepeat("");
    setError("");
    if (offer()) {
      setOffer(null);
      void api.devices.cancelLink().catch(() => undefined);
    }
    if (next === "link" && !deviceName()) {
      void api.devices
        .defaultName()
        .then((name) => setDeviceName((typed) => typed || name))
        .catch(() => undefined);
    }
    setMode(next);
    clearInterval(eraseTimer);
    if (next === "forgot") {
      // A pause before the button works: this cannot be undone.
      setEraseIn(3);
      eraseTimer = setInterval(() => {
        setEraseIn((n) => Math.max(0, n - 1));
        if (eraseIn() === 0) clearInterval(eraseTimer);
      }, 1000);
    }
  }

  async function handleErase() {
    begin();
    try {
      await api.eraseDevice();
      setHasId(false);
      switchMode("create");
    } catch (e) {
      fail(e);
    } finally {
      setBusy(false);
    }
  }

  function begin() {
    setBusy(true);
    setError("");
  }

  /** Shows a failure in the user's language. */
  function fail(e: unknown) {
    setError(reasonText(e));
  }

  const passphraseValid = () => chars(passphrase()) >= MIN_PASSPHRASE_CHARS && passphrase() === repeat();

  async function handleUnlock() {
    if (!passphrase()) return;
    begin();
    try {
      const [peerId, nick] = await api.unlockIdentity(passphrase());
      setPassphrase("");
      props.onUnlocked(peerId, nick);
    } catch (e) {
      fail(e);
    } finally {
      setBusy(false);
    }
  }

  async function handleCreate() {
    if (!nickname() || !passphraseValid()) return;
    begin();
    try {
      const nick = nickname();
      const peerId = await api.createIdentity(nick, passphrase());
      const phrase = await api.exportMnemonic(passphrase()).catch(() => null);
      setPassphrase("");
      setRepeat("");
      if (phrase === null) {
        // Nothing to confirm without the phrase; Settings can still export it.
        props.onUnlocked(peerId, nick);
        return;
      }
      const list = phraseWords(phrase);
      setPending({ peerId, nickname: nick });
      setWords(list);
      setPositions(pickPositions(list.length));
      setAnswers([]);
      setChecking(false);
      setMode("backup");
    } catch (e) {
      fail(e);
    } finally {
      setBusy(false);
    }
  }

  async function handleImport() {
    if (!mnemonic() || !nickname() || !passphraseValid()) return;
    begin();
    try {
      const peerId = await api.importMnemonic(mnemonic(), nickname(), passphrase());
      setPassphrase("");
      setRepeat("");
      setMnemonic("");
      props.onUnlocked(peerId, nickname());
    } catch (e) {
      fail(e);
    } finally {
      setBusy(false);
    }
  }

  /** Shows the offer, then waits until a device of the profile links this one. */
  async function handleLink() {
    if (!deviceName().trim() || !passphraseValid()) return;
    begin();
    try {
      const code = await api.devices.startLink(connection.gatewayAddr, deviceName().trim());
      setOffer(code);
      setOfferQr(await api.generateQr(code).catch(() => ""));
      setBusy(false);
      const [peerId, nick] = await api.devices.finishLink(passphrase());
      setPassphrase("");
      setRepeat("");
      setOffer(null);
      props.onUnlocked(peerId, nick);
    } catch (e) {
      // Leaving this screen cancels the wait; that is no failure to show.
      if (mode() === "link") {
        setOffer(null);
        fail(e);
      }
    } finally {
      setBusy(false);
    }
  }

  function finishBackup() {
    const done = pending();
    if (!done || !answersMatch(words(), positions(), answers())) return;
    setWords([]);
    setAnswers([]);
    setPending(null);
    props.onUnlocked(done.peerId, done.nickname);
  }

  const strengthLevel = createMemo(() => {
    const len = chars(passphrase());
    if (len >= 32) return 4;
    if (len >= 24) return 3;
    if (len >= 16) return 2;
    if (len >= MIN_PASSPHRASE_CHARS) return 1;
    return 0;
  });

  const title = () => {
    if (mode() === "forgot") return t().identity_forgot_title;
    if (mode() === "link") return t().identity_link_title;
    if (mode() !== "backup") return t().identity_title;
    return checking() ? t().backup_check_title : t().backup_title;
  };

  const subtitle = () => {
    const tr = t();
    switch (mode()) {
      case "unlock":
        return tr.identity_subtitle_unlock;
      case "create":
        return tr.identity_subtitle_create;
      case "import":
        return tr.identity_subtitle_import;
      case "backup":
        return checking() ? tr.backup_check_hint : tr.backup_hint;
      case "forgot":
        return tr.identity_forgot_text;
      case "link":
        return offer() ? tr.identity_link_scan : tr.identity_subtitle_link;
    }
  };

  /** A form whose Enter key and submit button both run `action`. */
  const form = (action: () => unknown, children: JSX.Element) => (
    <form
      class="onboard__form"
      onSubmit={(e) => {
        e.preventDefault();
        if (!busy()) void action();
      }}
    >
      {children}
    </form>
  );

  /** A new passphrase, typed twice, with its strength. */
  const passphrasePair = () => (
    <>
      <input
        class="field"
        type="password"
        placeholder={t().identity_passphrase_min}
        aria-label={t().identity_passphrase_min}
        value={passphrase()}
        onInput={(e) => setPassphrase(e.currentTarget.value)}
        autocomplete="new-password"
      />
      <Show when={passphrase().length > 0}>
        <div class="onboard__strength" data-level={strengthLevel()} aria-hidden="true">
          <span />
          <span />
          <span />
          <span />
        </div>
      </Show>
      <input
        class="field"
        type="password"
        placeholder={t().identity_passphrase_repeat}
        aria-label={t().identity_passphrase_repeat}
        value={repeat()}
        onInput={(e) => setRepeat(e.currentTarget.value)}
        autocomplete="new-password"
      />
      <Show when={repeat().length > 0 && repeat() !== passphrase()}>
        <p class="error-text">{t().identity_passphrase_mismatch}</p>
      </Show>
    </>
  );

  const nicknameField = (autofocus: boolean) => (
    <input
      class="field"
      type="text"
      placeholder={t().identity_nickname}
      aria-label={t().identity_nickname}
      value={nickname()}
      onInput={(e) => setNickname(e.currentTarget.value)}
      autocomplete="nickname"
      autofocus={autofocus}
    />
  );

  return (
    <main class="onboard">
      <Show when={hasId() !== null} fallback={<span class="spinner" />}>
        <div class="onboard__card">
          <div class="onboard__logo">
            <Icon name="shield" size={32} />
          </div>
          <h1 class="onboard__title">{title()}</h1>
          <p class="onboard__subtitle">{subtitle()}</p>

          <Show when={mode() === "unlock"}>
            {form(
              handleUnlock,
              <>
                <input
                  class="field"
                  type="password"
                  placeholder={t().identity_passphrase}
                  aria-label={t().identity_passphrase}
                  value={passphrase()}
                  onInput={(e) => setPassphrase(e.currentTarget.value)}
                  autocomplete="current-password"
                  autofocus
                />
                <button class="btn btn--primary btn--block" type="submit" disabled={busy() || !passphrase()}>
                  {busy() ? t().identity_unlocking : t().identity_unlock}
                </button>
              </>,
            )}
            <div class="onboard__links">
              <button class="btn btn--ghost" onClick={() => switchMode("forgot")}>
                {t().identity_forgot}
              </button>
            </div>
          </Show>

          <Show when={mode() === "forgot"}>
            <div class="onboard__form">
              <button
                class="btn btn--danger btn--block"
                disabled={busy() || eraseIn() > 0}
                onClick={() => void handleErase()}
              >
                {t().identity_erase(eraseIn())}
              </button>
            </div>
            <div class="onboard__links">
              <button class="btn btn--ghost" onClick={() => switchMode("unlock")}>
                {t().identity_back_unlock}
              </button>
            </div>
          </Show>

          <Show when={mode() === "create"}>
            {form(
              handleCreate,
              <>
                {nicknameField(true)}
                {passphrasePair()}
                <p class="hint">{t().identity_passphrase_hint}</p>
                <button
                  class="btn btn--primary btn--block"
                  type="submit"
                  disabled={busy() || !nickname() || !passphraseValid()}
                >
                  {busy() ? t().identity_creating : t().identity_create}
                </button>
              </>,
            )}
            <div class="onboard__links">
              <button class="btn btn--ghost" onClick={() => switchMode("import")}>
                {t().identity_import_link}
              </button>
              <button class="btn btn--ghost" onClick={() => switchMode("link")}>
                {t().identity_link_link}
              </button>
            </div>
          </Show>

          <Show when={mode() === "link"}>
            <Show
              when={offer()}
              fallback={form(
                handleLink,
                <>
                  <input
                    class="field"
                    type="text"
                    placeholder={t().devices_name}
                    aria-label={t().devices_name}
                    value={deviceName()}
                    onInput={(e) => setDeviceName(e.currentTarget.value)}
                    autocomplete="off"
                  />
                  {passphrasePair()}
                  <button
                    class="btn btn--primary btn--block"
                    type="submit"
                    disabled={busy() || !deviceName().trim() || !passphraseValid()}
                  >
                    {t().identity_link_show}
                  </button>
                </>,
              )}
            >
              {(code) => (
                <div class="onboard__form onboard__offer">
                  <Show when={offerQr()}>
                    <img class="onboard__qr" src={offerQr()} alt={t().identity_link_title} />
                  </Show>
                  <code class="mono onboard__code" data-testid="device-offer">
                    {code()}
                  </code>
                  <button class="btn btn--secondary btn--block" onClick={() => void copyText(code())}>
                    <Icon name="copy" size={18} /> {t().common_copy}
                  </button>
                  <p class="hint" role="status">
                    <span class="spinner" /> {t().identity_link_waiting}
                  </p>
                </div>
              )}
            </Show>
            <div class="onboard__links">
              <button class="btn btn--ghost" onClick={() => switchMode("create")}>
                {t().common_back}
              </button>
            </div>
          </Show>

          <Show when={mode() === "import"}>
            {form(
              handleImport,
              <>
                <textarea
                  class="field mono"
                  placeholder={t().identity_seed_placeholder}
                  aria-label={t().identity_seed_placeholder}
                  value={mnemonic()}
                  onInput={(e) => setMnemonic(e.currentTarget.value)}
                  rows={4}
                  spellcheck={false}
                  autocomplete="off"
                  autocapitalize="off"
                  autocorrect="off"
                  autofocus
                />
                {nicknameField(false)}
                {passphrasePair()}
                <button
                  class="btn btn--primary btn--block"
                  type="submit"
                  disabled={busy() || !mnemonic() || !nickname() || !passphraseValid()}
                >
                  {busy() ? t().identity_importing : t().identity_import}
                </button>
              </>,
            )}
            <div class="onboard__links">
              <button class="btn btn--ghost" onClick={() => switchMode("create")}>
                {t().common_back}
              </button>
            </div>
          </Show>

          <Show when={mode() === "backup"}>
            <Show
              when={checking()}
              fallback={
                <div class="onboard__form">
                  <ol class="recovery-words" data-testid="recovery-words">
                    <For each={words()}>{(word) => <li>{word}</li>}</For>
                  </ol>
                  <button class="btn btn--primary btn--block" onClick={() => setChecking(true)}>
                    {t().backup_written}
                  </button>
                </div>
              }
            >
              {form(
                finishBackup,
                <>
                  <For each={positions()}>
                    {(position, k) => (
                      <input
                        class="field"
                        type="text"
                        aria-label={t().backup_word(position + 1)}
                        placeholder={t().backup_word(position + 1)}
                        value={answers()[k()] ?? ""}
                        onInput={(e) => {
                          const next = [...answers()];
                          next[k()] = e.currentTarget.value;
                          setAnswers(next);
                        }}
                        spellcheck={false}
                        autocomplete="off"
                        autocapitalize="off"
                      />
                    )}
                  </For>
                  <button
                    class="btn btn--primary btn--block"
                    type="submit"
                    disabled={!answersMatch(words(), positions(), answers())}
                  >
                    {t().backup_continue}
                  </button>
                </>,
              )}
              <div class="onboard__links">
                <button class="btn btn--ghost" onClick={() => setChecking(false)}>
                  {t().backup_show_again}
                </button>
              </div>
            </Show>
          </Show>

          <Show when={error()}>
            <p class="onboard__error" role="alert" data-testid="identity-error">
              <Icon name="alert" size={18} />
              <span>{error()}</span>
            </p>
          </Show>
        </div>
      </Show>
    </main>
  );
}

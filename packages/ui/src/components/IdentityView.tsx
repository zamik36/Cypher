import { createSignal, For, Show, createMemo, onMount } from "solid-js";
import { api } from "../platform";
import { ShieldIcon } from "./Icons";
import { t } from "../i18n";
import { reasonText } from "../utils/reasons";
import { answersMatch, phraseWords, pickPositions } from "../utils/recovery";

/** The core's minimum (`identity_file::MIN_PASSPHRASE_CHARS`), counted in characters. */
const MIN_PASSPHRASE_CHARS = 12;

interface IdentityViewProps {
  onUnlocked: (peerId: string, nickname: string) => void;
}

type Mode = "unlock" | "create" | "import" | "backup";

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
    setMode(next);
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

  function onKeyDown(e: KeyboardEvent, handler: () => Promise<void>) {
    if (e.key === "Enter") void handler();
  }

  /** A new passphrase, typed twice, with its strength. */
  const passphrasePair = (onEnter: () => Promise<void>) => (
    <>
      <input
        type="password"
        placeholder={t().identity_passphrase_min}
        value={passphrase()}
        onInput={(e) => setPassphrase(e.currentTarget.value)}
        autocomplete="new-password"
      />
      <Show when={passphrase().length > 0}>
        <div class="strength-bar">
          <div class={`strength-segment ${strengthLevel() >= 1 ? "weak" : ""}`} />
          <div class={`strength-segment ${strengthLevel() >= 2 ? "fair" : ""}`} />
          <div class={`strength-segment ${strengthLevel() >= 3 ? "good" : ""}`} />
          <div class={`strength-segment ${strengthLevel() >= 4 ? "strong" : ""}`} />
        </div>
      </Show>
      <input
        type="password"
        placeholder={t().identity_passphrase_repeat}
        value={repeat()}
        onInput={(e) => setRepeat(e.currentTarget.value)}
        onKeyDown={(e) => onKeyDown(e, onEnter)}
        autocomplete="new-password"
      />
      <Show when={repeat().length > 0 && repeat() !== passphrase()}>
        <p class="identity-hint">{t().identity_passphrase_mismatch}</p>
      </Show>
    </>
  );

  return (
    <div class="identity-view">
      <Show when={hasId() !== null}>
        <div class="identity-card">
          <div class="identity-logo-icon">
            <ShieldIcon width="48" height="48" />
          </div>
          <h2>{t().identity_title}</h2>
          <p class="identity-subtitle">{t().identity_subtitle}</p>

          <Show when={mode() === "unlock"}>
            <div class="identity-form">
              <input
                type="password"
                placeholder={t().identity_passphrase}
                value={passphrase()}
                onInput={(e) => setPassphrase(e.currentTarget.value)}
                onKeyDown={(e) => onKeyDown(e, handleUnlock)}
                autocomplete="current-password"
                autofocus
              />
              <button class="btn-primary" onClick={() => void handleUnlock()} disabled={busy() || !passphrase()}>
                {busy() ? t().identity_unlocking : t().identity_unlock}
              </button>
              <div class="identity-links">
                <button class="link-btn" onClick={() => switchMode("create")}>
                  {t().identity_new}
                </button>
                <button class="link-btn" onClick={() => switchMode("import")}>
                  {t().identity_import}
                </button>
              </div>
            </div>
          </Show>

          <Show when={mode() === "create"}>
            <div class="identity-form">
              <input
                type="text"
                placeholder={t().identity_nickname}
                value={nickname()}
                onInput={(e) => setNickname(e.currentTarget.value)}
                autofocus
              />
              {passphrasePair(handleCreate)}
              <button
                class="btn-primary"
                onClick={() => void handleCreate()}
                disabled={busy() || !nickname() || !passphraseValid()}
              >
                {busy() ? t().identity_creating : t().identity_create}
              </button>
              <div class="identity-links">
                <Show when={hasId()}>
                  <button class="link-btn" onClick={() => switchMode("unlock")}>
                    {t().identity_back_unlock}
                  </button>
                </Show>
                <button class="link-btn" onClick={() => switchMode("import")}>
                  {t().identity_import}
                </button>
              </div>
            </div>
          </Show>

          <Show when={mode() === "import"}>
            <div class="identity-form">
              <textarea
                placeholder={t().identity_seed_placeholder}
                value={mnemonic()}
                onInput={(e) => setMnemonic(e.currentTarget.value)}
                rows={3}
                spellcheck={false}
                autocomplete="off"
                autocapitalize="off"
                autocorrect="off"
                inputmode="text"
              />
              <input
                type="text"
                placeholder={t().identity_nickname}
                value={nickname()}
                onInput={(e) => setNickname(e.currentTarget.value)}
              />
              {passphrasePair(handleImport)}
              <button
                class="btn-primary"
                onClick={() => void handleImport()}
                disabled={busy() || !mnemonic() || !nickname() || !passphraseValid()}
              >
                {busy() ? t().identity_importing : t().identity_import}
              </button>
              <button class="link-btn" onClick={() => switchMode(hasId() ? "unlock" : "create")}>
                {t().identity_back}
              </button>
            </div>
          </Show>

          <Show when={mode() === "backup"}>
            <div class="identity-form">
              <h3>{checking() ? t().backup_check_title : t().backup_title}</h3>
              <Show
                when={checking()}
                fallback={
                  <>
                    <p class="identity-hint">{t().backup_hint}</p>
                    <ol class="recovery-words">
                      <For each={words()}>{(word) => <li>{word}</li>}</For>
                    </ol>
                    <button class="btn-primary" onClick={() => setChecking(true)}>
                      {t().backup_written}
                    </button>
                  </>
                }
              >
                <p class="identity-hint">{t().backup_check_hint}</p>
                <For each={positions()}>
                  {(position, k) => (
                    <input
                      type="text"
                      aria-label={t().backup_word(position + 1)}
                      placeholder={t().backup_word(position + 1)}
                      value={answers()[k()] ?? ""}
                      onInput={(e) => {
                        const next = [...answers()];
                        next[k()] = e.currentTarget.value;
                        setAnswers(next);
                      }}
                      onKeyDown={(e) => {
                        if (e.key === "Enter") finishBackup();
                      }}
                      spellcheck={false}
                      autocomplete="off"
                      autocapitalize="off"
                    />
                  )}
                </For>
                <button
                  class="btn-primary"
                  onClick={finishBackup}
                  disabled={!answersMatch(words(), positions(), answers())}
                >
                  {t().backup_continue}
                </button>
                <button class="link-btn" onClick={() => setChecking(false)}>
                  {t().backup_show_again}
                </button>
              </Show>
            </div>
          </Show>

          <Show when={error()}>
            <div class="identity-error" role="alert">
              {error()}
            </div>
          </Show>
        </div>
      </Show>
    </div>
  );
}

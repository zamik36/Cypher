import { createSignal, Show, createMemo, onMount } from "solid-js";
import { api } from "../api/tauri";
import { ShieldIcon } from "./Icons";

interface IdentityViewProps {
  onUnlocked: (peerId: string, nickname: string) => void;
}

export default function IdentityView(props: IdentityViewProps) {
  const [hasId, setHasId] = createSignal<boolean | null>(null);
  const [mode, setMode] = createSignal<"unlock" | "create" | "import">("unlock");
  const [nickname, setNickname] = createSignal("");
  const [passphrase, setPassphrase] = createSignal("");
  const [mnemonic, setMnemonic] = createSignal("");
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);

  onMount(() => {
    api.hasIdentity()
      .then((exists) => {
        setHasId(exists);
        if (!exists) setMode("create");
      })
      .catch((e) => {
        setHasId(false);
        setMode("create");
        setError(String(e));
      });
  });

  async function handleUnlock() {
    if (!passphrase()) return;
    setBusy(true);
    setError("");
    try {
      const [peerId, nick] = await api.unlockIdentity(passphrase());
      setPassphrase("");
      props.onUnlocked(peerId, nick);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function handleCreate() {
    if (!nickname() || passphrase().length < 12) return;
    setBusy(true);
    setError("");
    try {
      const peerId = await api.createIdentity(nickname(), passphrase());
      setPassphrase("");
      props.onUnlocked(peerId, nickname());
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function handleImport() {
    if (!mnemonic() || !nickname() || !passphrase()) return;
    setBusy(true);
    setError("");
    try {
      const peerId = await api.importMnemonic(mnemonic(), nickname(), passphrase());
      setPassphrase("");
      setMnemonic("");
      props.onUnlocked(peerId, nickname());
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  const strengthLevel = createMemo(() => {
    const len = passphrase().length;
    if (len >= 32) return 4;
    if (len >= 24) return 3;
    if (len >= 16) return 2;
    if (len >= 12) return 1;
    return 0;
  });

  function onKeyDown(e: KeyboardEvent, handler: () => void) {
    if (e.key === "Enter") handler();
  }

  return (
    <div class="identity-view">
      <Show when={hasId() !== null}>
        <div class="identity-card">
          <div class="identity-logo-icon">
            <ShieldIcon width="48" height="48" />
          </div>
          <h2>Cypher</h2>
          <p class="identity-subtitle">Anonymous encrypted messenger</p>

          <Show when={mode() === "unlock"}>
            <div class="identity-form">
              <input
                type="password"
                placeholder="Passphrase"
                value={passphrase()}
                onInput={(e) => setPassphrase(e.currentTarget.value)}
                onKeyDown={(e) => onKeyDown(e, handleUnlock)}
                autofocus
              />
              <button class="btn-primary" onClick={handleUnlock} disabled={busy() || !passphrase()}>
                {busy() ? "Unlocking..." : "Unlock"}
              </button>
              <div class="identity-links">
                <button class="link-btn" onClick={() => setMode("create")}>New identity</button>
                <button class="link-btn" onClick={() => setMode("import")}>Import</button>
              </div>
            </div>
          </Show>

          <Show when={mode() === "create"}>
            <div class="identity-form">
              <input
                type="text"
                placeholder="Nickname"
                value={nickname()}
                onInput={(e) => setNickname(e.currentTarget.value)}
                autofocus
              />
              <input
                type="password"
                placeholder="Passphrase (min 12 chars)"
                value={passphrase()}
                onInput={(e) => setPassphrase(e.currentTarget.value)}
                onKeyDown={(e) => onKeyDown(e, handleCreate)}
              />
              <Show when={passphrase().length > 0}>
                <div class="strength-bar">
                  <div class={`strength-segment ${strengthLevel() >= 1 ? "weak" : ""}`} />
                  <div class={`strength-segment ${strengthLevel() >= 2 ? "fair" : ""}`} />
                  <div class={`strength-segment ${strengthLevel() >= 3 ? "good" : ""}`} />
                  <div class={`strength-segment ${strengthLevel() >= 4 ? "strong" : ""}`} />
                </div>
              </Show>
              <button
                class="btn-primary"
                onClick={handleCreate}
                disabled={busy() || !nickname() || passphrase().length < 12}
              >
                {busy() ? "Creating..." : "Create identity"}
              </button>
              <Show when={hasId()}>
                <button class="link-btn" onClick={() => setMode("unlock")}>Back to unlock</button>
              </Show>
            </div>
          </Show>

          <Show when={mode() === "import"}>
            <div class="identity-form">
              <textarea
                placeholder="Mnemonic (24 words)"
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
                placeholder="Nickname"
                value={nickname()}
                onInput={(e) => setNickname(e.currentTarget.value)}
              />
              <input
                type="password"
                placeholder="Passphrase"
                value={passphrase()}
                onInput={(e) => setPassphrase(e.currentTarget.value)}
                onKeyDown={(e) => onKeyDown(e, handleImport)}
              />
              <button
                class="btn-primary"
                onClick={handleImport}
                disabled={busy() || !mnemonic() || !nickname() || !passphrase()}
              >
                {busy() ? "Importing..." : "Import"}
              </button>
              <button class="link-btn" onClick={() => setMode(hasId() ? "unlock" : "create")}>Back</button>
            </div>
          </Show>

          <Show when={error()}>
            <div class="identity-error">{error()}</div>
          </Show>
        </div>
      </Show>
    </div>
  );
}

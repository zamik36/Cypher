import { createSignal, Show } from "solid-js";
import { api } from "../api/tauri";

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

  // Check on first render whether identity exists.
  api.hasIdentity().then((exists) => {
    setHasId(exists);
    if (!exists) setMode("create");
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

  function onKeyDown(e: KeyboardEvent, handler: () => void) {
    if (e.key === "Enter") handler();
  }

  return (
    <div class="identity-view">
      <Show when={hasId() !== null}>
        <div class="identity-card">
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
              <input
                type="text"
                placeholder="Mnemonic (24 words)"
                value={mnemonic()}
                onInput={(e) => setMnemonic(e.currentTarget.value)}
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

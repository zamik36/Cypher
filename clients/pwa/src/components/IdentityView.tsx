import { createSignal, Show } from "solid-js";
import { hasIdentity, createIdentity, unlockIdentity, importSeed } from "../storage/identity";
import type { IdentityData } from "../storage/identity";

interface IdentityViewProps {
  onUnlocked: (data: IdentityData) => void;
}

export default function IdentityView(props: IdentityViewProps) {
  const existing = hasIdentity();
  const [mode, setMode] = createSignal<"unlock" | "create" | "import">(
    existing ? "unlock" : "create",
  );
  const [nickname, setNickname] = createSignal("");
  const [passphrase, setPassphrase] = createSignal("");
  const [seedHex, setSeedHex] = createSignal("");
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);

  async function handleUnlock() {
    if (!passphrase()) return;
    setBusy(true);
    setError("");
    try {
      const data = await unlockIdentity(passphrase());
      setPassphrase("");
      props.onUnlocked(data);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function handleCreate() {
    if (!nickname() || !passphrase()) return;
    setBusy(true);
    setError("");
    try {
      const data = await createIdentity(nickname(), passphrase());
      setPassphrase("");
      props.onUnlocked(data);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function handleImport() {
    if (!seedHex() || !nickname() || !passphrase()) return;
    setBusy(true);
    setError("");
    try {
      const data = await importSeed(seedHex(), nickname(), passphrase());
      setPassphrase("");
      setSeedHex("");
      props.onUnlocked(data);
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
      <div class="identity-card">
        <h2>Cypher</h2>
        <p class="identity-subtitle">Anonymous encrypted messenger</p>

        <Show when={mode() === "unlock"}>
          <div class="identity-form">
            <input
              type="password"
              placeholder="PIN / passphrase"
              value={passphrase()}
              onInput={(e) => setPassphrase(e.currentTarget.value)}
              onKeyDown={(e) => onKeyDown(e, handleUnlock)}
              autofocus
            />
            <button class="btn-primary" onClick={handleUnlock} disabled={busy() || !passphrase()}>
              {busy() ? "Unlocking..." : "Unlock"}
            </button>
            <div class="identity-links">
              <button class="link-btn" onClick={() => setMode("create")}>
                New identity
              </button>
              <button class="link-btn" onClick={() => setMode("import")}>
                Import
              </button>
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
            <Show when={existing}>
              <button class="link-btn" onClick={() => setMode("unlock")}>
                Back to unlock
              </button>
            </Show>
          </div>
        </Show>

        <Show when={mode() === "import"}>
          <div class="identity-form">
            <input
              type="text"
              placeholder="Seed (64 hex characters)"
              value={seedHex()}
              onInput={(e) => setSeedHex(e.currentTarget.value)}
            />
            <input
              type="text"
              placeholder="Nickname"
              value={nickname()}
              onInput={(e) => setNickname(e.currentTarget.value)}
            />
            <input
              type="password"
              placeholder="PIN / passphrase"
              value={passphrase()}
              onInput={(e) => setPassphrase(e.currentTarget.value)}
              onKeyDown={(e) => onKeyDown(e, handleImport)}
            />
            <button
              class="btn-primary"
              onClick={handleImport}
              disabled={busy() || !seedHex() || !nickname() || !passphrase()}
            >
              {busy() ? "Importing..." : "Import"}
            </button>
            <button class="link-btn" onClick={() => setMode(existing ? "unlock" : "create")}>
              Back
            </button>
          </div>
        </Show>

        <Show when={error()}>
          <div class="identity-error">{error()}</div>
        </Show>
      </div>
    </div>
  );
}

import { createSignal, Show, createMemo } from "solid-js";
import { hasIdentity, createIdentity, unlockIdentity, importSeed } from "../storage/identity";
import type { IdentityData } from "../storage/identity";
import { ShieldIcon } from "./Icons";
import { t } from "../i18n";

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

  const strengthLevel = createMemo(() => {
    const len = passphrase().length;
    if (len >= 32) return 4;
    if (len >= 24) return 3;
    if (len >= 16) return 2;
    if (len >= 12) return 1;
    return 0;
  });

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
    if (!nickname() || passphrase().length < 12) return;
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
              autofocus
            />
            <button class="btn-primary" onClick={handleUnlock} disabled={busy() || !passphrase()}>
              {busy() ? t().identity_unlocking : t().identity_unlock}
            </button>
            <div class="identity-links">
              <button class="link-btn" onClick={() => setMode("create")}>
                {t().identity_new}
              </button>
              <button class="link-btn" onClick={() => setMode("import")}>
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
              <input
                type="password"
                placeholder={t().identity_passphrase_min}
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
              {busy() ? t().identity_creating : t().identity_create}
            </button>
            <Show when={existing}>
              <button class="link-btn" onClick={() => setMode("unlock")}>
                {t().identity_back_unlock}
              </button>
            </Show>
          </div>
        </Show>

        <Show when={mode() === "import"}>
          <div class="identity-form">
            <textarea
              placeholder={t().identity_seed_placeholder}
              value={seedHex()}
              onInput={(e) => setSeedHex(e.currentTarget.value)}
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
              <input
                type="password"
                placeholder={t().identity_passphrase}
                value={passphrase()}
                onInput={(e) => setPassphrase(e.currentTarget.value)}
                onKeyDown={(e) => onKeyDown(e, handleImport)}
            />
            <button
              class="btn-primary"
              onClick={handleImport}
              disabled={busy() || !seedHex() || !nickname() || !passphrase()}
            >
              {busy() ? t().identity_importing : t().identity_import}
            </button>
            <button class="link-btn" onClick={() => setMode(existing ? "unlock" : "create")}>
              {t().identity_back}
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

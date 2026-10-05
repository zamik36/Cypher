import { createSignal, Show, untrack } from "solid-js";
import "./ContactScreen.css";
import Avatar from "../components/Avatar";
import Icon from "../components/Icon";
import SafetyNumber from "../components/SafetyNumber";
import TopBar from "../components/TopBar";
import { api } from "../platform";
import { avatarName, contacts, displayName, removeContact, setAlias } from "../stores/contacts";
import { removeChat } from "../stores/chat";
import { back, depth } from "../stores/nav";
import { addToast, toastError } from "../stores/toasts";
import { copyText } from "../utils/clipboard";
import { t } from "../i18n";

/** Longest contact name the core keeps. */
const MAX_NAME = 64;

export default function ContactScreen(props: { peerId: string }) {
  const [name, setName] = createSignal(untrack(() => contacts[props.peerId]?.alias ?? ""));
  const [saving, setSaving] = createSignal(false);
  const [verifying, setVerifying] = createSignal(false);
  const [confirmDelete, setConfirmDelete] = createSignal(false);
  const [deleting, setDeleting] = createSignal(false);
  const changed = () => name().trim() !== (contacts[props.peerId]?.alias ?? "");

  async function save() {
    const alias = name().trim() || null;
    setSaving(true);
    try {
      await api.renamePeer(props.peerId, alias);
      setAlias(props.peerId, alias);
      addToast(t().contact_saved, "success");
    } catch (e) {
      toastError(e);
    } finally {
      setSaving(false);
    }
  }

  async function remove() {
    setDeleting(true);
    try {
      await api.deleteConversation(props.peerId);
      removeChat(props.peerId);
      removeContact(props.peerId);
      addToast(t().contact_deleted, "success");
      // Back past the chat too: it no longer exists.
      history.go(-depth());
    } catch (e) {
      toastError(e);
      setDeleting(false);
    }
  }

  return (
    <section class="screen">
      <TopBar title={t().contact_title} onBack={back} />
      <div class="screen__body">
        <div class="content contact">
          <div class="contact__hero">
            <Avatar peerId={props.peerId} name={avatarName(props.peerId)} size={88} />
            <h2>{displayName(props.peerId)}</h2>
          </div>

          <form
            class="contact__name"
            onSubmit={(e) => {
              e.preventDefault();
              if (changed()) void save();
            }}
          >
            <label class="label" for="contact-name">
              {t().contact_name}
            </label>
            <div class="contact__row">
              <input
                id="contact-name"
                class="field"
                type="text"
                maxLength={MAX_NAME}
                placeholder={t().contact_name_placeholder}
                value={name()}
                onInput={(e) => setName(e.currentTarget.value)}
                autocomplete="off"
              />
              <button class="btn btn--primary" type="submit" disabled={!changed() || saving()}>
                {t().common_save}
              </button>
            </div>
            <p class="hint">{t().contact_name_hint}</p>
          </form>

          <h3 class="group-title">{t().contact_security}</h3>
          <div class="list">
            <button class="list-row" onClick={() => setVerifying(true)}>
              <span class="list-row__icon">
                <Icon name="shield" size={18} />
              </span>
              <span class="list-row__text">
                <span class="list-row__title">{t().verify_open}</span>
                <span class="list-row__subtitle">{t().contact_verify_hint}</span>
              </span>
              <Icon name="chevron-right" class="list-row__chevron" />
            </button>
            <button class="list-row" onClick={() => void copyText(props.peerId)}>
              <span class="list-row__icon">
                <Icon name="copy" size={18} />
              </span>
              <span class="list-row__text">
                <span class="list-row__title">{t().contact_id}</span>
                <span class="list-row__subtitle mono contact__id">{props.peerId}</span>
              </span>
            </button>
          </div>

          <div class="contact__danger">
            <Show
              when={confirmDelete()}
              fallback={
                <button class="btn btn--danger btn--block" onClick={() => setConfirmDelete(true)}>
                  <Icon name="trash" size={18} /> {t().contact_delete}
                </button>
              }
            >
              <div class="card contact__confirm" role="alertdialog" aria-labelledby="delete-title">
                <p id="delete-title">{t().contact_delete_confirm}</p>
                <div class="contact__row">
                  <button class="btn btn--secondary" onClick={() => setConfirmDelete(false)} disabled={deleting()}>
                    {t().common_cancel}
                  </button>
                  <button class="btn btn--danger" onClick={() => void remove()} disabled={deleting()}>
                    {t().contact_delete}
                  </button>
                </div>
              </div>
            </Show>
          </div>
        </div>
      </div>
      <Show when={verifying()}>
        <SafetyNumber peerId={props.peerId} peerName={displayName(props.peerId)} onClose={() => setVerifying(false)} />
      </Show>
    </section>
  );
}

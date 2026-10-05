import { createSignal, For, Show } from "solid-js";
import "./ChatList.css";
import Avatar from "../components/Avatar";
import Icon from "../components/Icon";
import TopBar from "../components/TopBar";
import StatusTick from "../components/StatusTick";
import { avatarName, contacts, displayName, sortedContacts, type Contact } from "../stores/contacts";
import { connection, type LinkState } from "../stores/connection";
import { isWide } from "../stores/layout";
import { push, replace, top } from "../stores/nav";
import { formatListTime } from "../utils/format";
import { isMine, previewOf } from "../utils/messages";
import { locale, t } from "../i18n";

/** What the header says about the connection; nothing while online. */
function linkLabel(state: LinkState): string | undefined {
  const tr = t();
  const labels: Partial<Record<LinkState, string>> = {
    idle: tr.status_offline,
    connecting: tr.status_connecting,
    reconnecting: tr.status_reconnecting,
    failed: tr.status_offline,
    superseded: tr.status_superseded,
    update_required: tr.status_update_required,
  };
  return labels[state];
}

/** Opens a chat: beside the list on a wide window, on top of it on a phone. */
export function openChat(peerId: string): void {
  const screen = top();
  if (isWide() && screen.name !== "chats") replace({ name: "chat", peerId });
  else push({ name: "chat", peerId });
}

function ChatRow(props: { contact: Contact }) {
  const name = () => displayName(props.contact.peerId);
  const selected = () => {
    const screen = top();
    return screen.name === "chat" && screen.peerId === props.contact.peerId;
  };
  const preview = () => {
    const last = props.contact.last;
    return last ? previewOf(last) : { text: t().chats_no_messages };
  };
  return (
    <button
      class="chat-row"
      classList={{ "chat-row--selected": selected() && isWide() }}
      data-testid="chat-row"
      aria-current={selected() ? "true" : undefined}
      onClick={() => openChat(props.contact.peerId)}
    >
      <Avatar peerId={props.contact.peerId} name={avatarName(props.contact.peerId)} size={48} />
      <span class="chat-row__body">
        <span class="chat-row__line">
          <span class="chat-row__name">{name()}</span>
          <Show when={props.contact.lastAt > 0 && props.contact.last}>
            <span class="chat-row__time">{formatListTime(props.contact.lastAt, Date.now(), locale())}</span>
          </Show>
        </span>
        <span class="chat-row__line">
          <span class="chat-row__preview">
            <Show when={props.contact.last && isMine(props.contact.last)}>
              <span class="chat-row__you">{t().preview_you}</span>
            </Show>
            <Show when={preview().icon}>{(icon) => <Icon name={icon()} size={14} />}</Show>
            <span class="chat-row__text">{preview().text}</span>
          </span>
          <Show
            when={props.contact.unread > 0}
            fallback={
              <Show when={props.contact.last && isMine(props.contact.last) ? props.contact.last.status : undefined}>
                {(status) => <StatusTick status={status()} />}
              </Show>
            }
          >
            <span class="badge" aria-label={t().chats_unread(props.contact.unread)}>
              {props.contact.unread > 99 ? "99+" : props.contact.unread}
            </span>
          </Show>
        </span>
      </span>
    </button>
  );
}

export default function ChatList() {
  const [query, setQuery] = createSignal("");
  const all = () => sortedContacts();
  const shown = () => sortedContacts(query());
  const newChat = (tab: "invite" | "join") => push({ name: "new-chat", tab });

  return (
    <section class="screen chat-list" data-testid="chat-list">
      <TopBar
        actions={
          <>
            <Show when={isWide()}>
              <button class="icon-btn" onClick={() => newChat("invite")} aria-label={t().chats_new}>
                <Icon name="plus" />
              </button>
            </Show>
            <button class="icon-btn" onClick={() => push({ name: "settings" })} aria-label={t().settings_title}>
              <Icon name="settings" />
            </button>
          </>
        }
      >
        <h1>{t().chats_title}</h1>
        <span
          class="topbar__subtitle"
          classList={{ "visually-hidden": connection.link === "online" }}
          data-testid="link-status"
          data-state={connection.link}
          role="status"
        >
          {linkLabel(connection.link) ?? t().status_connected}
        </span>
      </TopBar>

      <div class="screen__body">
        <Show
          when={all().length > 0}
          fallback={
            <div class="empty">
              <span class="empty__icon">
                <Icon name="message-circle" size={28} />
              </span>
              <h2>{t().chats_empty_title}</h2>
              <p>{t().chats_empty_text}</p>
              <div class="chat-list__empty-actions">
                <button class="btn btn--primary" onClick={() => newChat("invite")}>
                  <Icon name="qr" /> {t().chats_empty_invite}
                </button>
                <button class="btn btn--secondary" onClick={() => newChat("join")}>
                  {t().chats_empty_join}
                </button>
              </div>
            </div>
          }
        >
          <div class="chat-list__search">
            <Icon name="search" size={18} />
            <input
              class="field"
              type="search"
              placeholder={t().chats_search}
              aria-label={t().chats_search}
              value={query()}
              onInput={(e) => setQuery(e.currentTarget.value)}
            />
          </div>
          <div class="chat-list__rows">
            <For each={shown()} fallback={<p class="chat-list__nothing muted">{t().chats_no_results}</p>}>
              {(contact) => <ChatRow contact={contacts[contact.peerId] ?? contact} />}
            </For>
          </div>
        </Show>
      </div>

      <Show when={!isWide() && all().length > 0}>
        <button class="chat-list__fab" onClick={() => newChat("invite")} aria-label={t().chats_new}>
          <Icon name="plus" size={24} />
        </button>
      </Show>
    </section>
  );
}

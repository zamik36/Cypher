import { createEffect, createSignal, For, Match, on, onCleanup, onMount, Show, Switch, untrack } from "solid-js";
import "./Chat.css";
import ActionMenu, { type MenuAction } from "../components/ActionMenu";
import Avatar from "../components/Avatar";
import Icon from "../components/Icon";
import SafetyNumber from "../components/SafetyNumber";
import StatusTick from "../components/StatusTick";
import TopBar from "../components/TopBar";
import FileCard from "../components/FileCard";
import RecordButton from "../components/media/RecordButton";
import RoundVideoBubble from "../components/media/RoundVideoBubble";
import VoiceBubble from "../components/media/VoiceBubble";
import { api, type ChatMessage, type MediaSent, type TransferInfo, type UiFile } from "../platform";
import {
  addMessage,
  findMessage,
  getMessages,
  hasOlder,
  historyLoaded,
  mergeHistory,
  prependHistory,
  removeMessage,
  setHasOlder,
} from "../stores/chat";
import {
  avatarName,
  contacts,
  displayName,
  markConversationRead,
  noteMessage,
  setContactBlocked,
  setContactRequest,
  setLastMessage,
} from "../stores/contacts";
import { clearDraft, draftOf, setDraftText, setReplyTo } from "../stores/drafts";
import { isWide } from "../stores/layout";
import { back, push } from "../stores/nav";
import { takeShare } from "../stores/share";
import { windowActive } from "../stores/presence";
import { upsertTransfer } from "../stores/transfers";
import { trackMedia } from "../stores/media";
import { addToast, toastError } from "../stores/toasts";
import { copyText } from "../utils/clipboard";
import { dayLabel, formatTime } from "../utils/format";
import { isMine, ME, noteOf, previewOf, toChatMessage } from "../utils/messages";
import { buildTimeline } from "../utils/timeline";
import { locale, t } from "../i18n";

/** Messages fetched per page of history. */
const HISTORY_PAGE = 50;
/** Closer than this to the bottom, new messages keep the view scrolled down. */
const STICK_PX = 120;
/** The composer grows up to this height, then scrolls. */
const COMPOSER_MAX_PX = 144;
/** A touch held this long opens a message's menu. */
const LONG_PRESS_MS = 450;
/** Pages of older history searched for a quoted message before giving up. */
const QUOTE_SEARCH_PAGES = 10;

/** One line naming a message: who wrote it, and what it says or carries. */
function Quote(props: { peerId: string; msg: ChatMessage | undefined }) {
  const author = () => {
    const msg = props.msg;
    if (!msg) return "";
    return isMine(msg) ? t().reply_you : displayName(props.peerId);
  };
  return (
    <>
      <span class="quote__author">{author()}</span>
      <span class="quote__text">
        <Show when={props.msg} fallback={t().reply_unavailable}>
          {(msg) => {
            const preview = () => previewOf(msg());
            return (
              <>
                <Show when={preview().icon}>{(icon) => <Icon name={icon()} size={14} />}</Show> {preview().text}
              </>
            );
          }}
        </Show>
      </span>
    </>
  );
}

function MessageBubble(props: {
  peerId: string;
  msg: ChatMessage;
  first: boolean;
  last: boolean;
  onMenu: (msg: ChatMessage, anchor: HTMLElement) => void;
  onQuote: (msgId: string) => void;
}) {
  let row: HTMLDivElement | undefined;
  let press: ReturnType<typeof setTimeout> | undefined;
  const mine = () => isMine(props.msg);
  const note = () => noteOf(props.msg);
  const menu = () => row && props.onMenu(props.msg, row.querySelector<HTMLElement>(".bubble, .msg__round") ?? row);
  const quoted = () => props.msg.reply_to;
  onCleanup(() => clearTimeout(press));
  const meta = () => (
    <span class="msg__meta">
      <span>{formatTime(props.msg.timestamp, locale())}</span>
      <Show when={mine() && props.msg.status}>{(status) => <StatusTick status={status()} />}</Show>
    </span>
  );
  return (
    <div
      ref={row}
      class="msg"
      classList={{ "msg--out": mine(), "msg--in": !mine(), "msg--first": props.first, "msg--last": props.last }}
      data-testid="message"
      data-dir={mine() ? "out" : "in"}
      data-msg-id={props.msg.msg_id}
      onContextMenu={(e) => {
        e.preventDefault();
        menu();
      }}
      onPointerDown={(e) => {
        if (e.pointerType !== "touch") return;
        clearTimeout(press);
        press = setTimeout(menu, LONG_PRESS_MS);
      }}
      onPointerUp={() => clearTimeout(press)}
      onPointerCancel={() => clearTimeout(press)}
      onPointerMove={() => clearTimeout(press)}
    >
      <button class="msg__more" aria-label={t().msg_actions} onClick={menu}>
        <Icon name="more" size={18} />
      </button>
      <Switch
        fallback={
          <div class="bubble">
            <Show when={quoted()}>
              {(id) => (
                <button class="quote" onClick={() => props.onQuote(id())}>
                  <Quote peerId={props.peerId} msg={findMessage(props.peerId, id())} />
                </button>
              )}
            </Show>
            <span class="bubble__text">{props.msg.text}</span>
            {meta()}
          </div>
        }
      >
        <Match when={note()?.kind === "video_note" && note()}>
          {(file) => (
            <div class="msg__round">
              <RoundVideoBubble file={file()} />
              {meta()}
            </div>
          )}
        </Match>
        <Match when={note()}>
          {(file) => (
            <div class="bubble bubble--media">
              <VoiceBubble file={file()} meta={meta()} />
            </div>
          )}
        </Match>
        <Match when={props.msg.file}>
          {(file) => (
            <div class="bubble bubble--file">
              <FileCard file={file()} outgoing={mine()} />
              {meta()}
            </div>
          )}
        </Match>
      </Switch>
    </div>
  );
}

export default function ChatScreen(props: { peerId: string }) {
  const draft = () => draftOf(props.peerId).text;
  const setDraft = (text: string) => setDraftText(props.peerId, text);
  const replyTo = () => draftOf(props.peerId).replyTo;
  const [menuFor, setMenuFor] = createSignal<{ msg: ChatMessage; anchor: HTMLElement } | null>(null);
  const [loading, setLoading] = createSignal(untrack(() => !historyLoaded(props.peerId)));
  const [loadingOlder, setLoadingOlder] = createSignal(false);
  const [verifying, setVerifying] = createSignal(false);
  const [awayFromBottom, setAwayFromBottom] = createSignal(false);
  let scroller: HTMLDivElement | undefined;
  let sentinel: HTMLDivElement | undefined;
  let composer: HTMLTextAreaElement | undefined;

  const messages = () => getMessages(props.peerId);
  const timeline = () => buildTimeline(messages(), isMine);
  const name = () => displayName(props.peerId);
  const nearBottom = () => !scroller || scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight < STICK_PX;
  const toBottom = () => queueMicrotask(() => scroller && (scroller.scrollTop = scroller.scrollHeight));

  // The latest page first; older pages as the user scrolls up.
  onMount(() => {
    const peerId = props.peerId;
    if (historyLoaded(peerId)) {
      setLoading(false);
      toBottom();
      return;
    }
    api
      .getHistory(peerId, HISTORY_PAGE)
      .then((page) => {
        mergeHistory(
          peerId,
          page.reverse().map((m) => toChatMessage(peerId, m)),
        );
        setHasOlder(peerId, page.length === HISTORY_PAGE);
      })
      .catch(toastError)
      .finally(() => {
        setLoading(false);
        toBottom();
      });
  });

  async function loadOlder() {
    const oldest = messages()[0];
    if (!oldest || loadingOlder() || !hasOlder(props.peerId)) return;
    setLoadingOlder(true);
    try {
      // `before` excludes its own millisecond; ask one later and drop repeats.
      const page = await api.getHistory(props.peerId, HISTORY_PAGE, oldest.timestamp + 1);
      const fromBottom = scroller ? scroller.scrollHeight - scroller.scrollTop : 0;
      const added = prependHistory(
        props.peerId,
        page.reverse().map((m) => toChatMessage(props.peerId, m)),
      );
      setHasOlder(props.peerId, page.length === HISTORY_PAGE && added > 0);
      queueMicrotask(() => scroller && (scroller.scrollTop = scroller.scrollHeight - fromBottom));
    } catch (e) {
      toastError(e);
    } finally {
      setLoadingOlder(false);
    }
  }

  onMount(() => {
    if (!sentinel || typeof IntersectionObserver !== "function") return;
    const observer = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting)) void loadOlder();
      },
      { root: scroller ?? null, rootMargin: "400px 0px 0px 0px" },
    );
    observer.observe(sentinel);
    onCleanup(() => observer.disconnect());
  });

  // New messages keep the view at the bottom unless the user scrolled up to read.
  createEffect(
    on(
      () => messages().length,
      (count, previous) => {
        if (previous === undefined || count <= previous) return;
        const last = messages()[count - 1];
        if ((last && isMine(last)) || !awayFromBottom()) toBottom();
      },
    ),
  );

  // Messages count as read only while the user can see them.
  createEffect(() => {
    if (!windowActive() || loading()) return;
    markConversationRead(props.peerId);
    const unread = messages().flatMap((m) => (!isMine(m) && m.msg_id && m.status !== "read" ? [m.msg_id] : []));
    if (unread.length > 0) api.markRead(props.peerId, unread).catch(() => undefined);
  });

  function remember(msg: ChatMessage) {
    addMessage(props.peerId, msg);
    noteMessage(props.peerId, msg, true);
  }

  function resizeComposer() {
    if (!composer) return;
    composer.style.height = "auto";
    composer.style.height = `${Math.min(composer.scrollHeight, COMPOSER_MAX_PX)}px`;
  }

  async function sendText(text: string, reply: string | null) {
    const msgId = await api.sendMessage(props.peerId, text, reply ?? undefined);
    remember({ msg_id: msgId, from: ME, text, timestamp: Date.now(), status: "pending", reply_to: reply });
  }

  async function send() {
    const text = draft().trim();
    if (!text) return;
    const reply = replyTo();
    clearDraft(props.peerId);
    queueMicrotask(resizeComposer);
    try {
      await sendText(text, reply);
    } catch (e) {
      setDraft(text);
      setReplyTo(props.peerId, reply);
      toastError(e);
    }
  }

  async function acceptRequest() {
    const peerId = props.peerId;
    try {
      await api.acceptContact(peerId);
      setContactRequest(peerId, false);
    } catch (e) {
      toastError(e);
    }
  }

  async function setBlocked(blocked: boolean) {
    const peerId = props.peerId;
    try {
      await api.setBlocked(peerId, blocked);
      setContactBlocked(peerId, blocked);
      addToast(blocked ? t().toast_blocked : t().toast_unblocked, "success");
    } catch (e) {
      toastError(e);
    }
  }

  /** Removes a message here; the list shows the one before it. */
  async function deleteHere(msg: ChatMessage) {
    if (!msg.msg_id) return;
    await api.deleteMessage(props.peerId, msg.msg_id, msg.timestamp);
    removeMessage(props.peerId, msg.msg_id);
    if (contacts[props.peerId]?.last?.msg_id === msg.msg_id) setLastMessage(props.peerId, messages().at(-1) ?? null);
    if (replyTo() === msg.msg_id) setReplyTo(props.peerId, null);
  }

  async function deleteAndSay(msg: ChatMessage) {
    await deleteHere(msg);
    addToast(t().msg_deleted, "success");
  }

  /** Sends a failed text again as a new message, then drops the failed one. */
  async function retry(msg: ChatMessage, text: string, reply: string | null) {
    await sendText(text, reply);
    await deleteHere(msg);
  }

  function actionsFor(msg: ChatMessage): MenuAction[] {
    const tr = t();
    const id = msg.msg_id;
    const text = msg.file ? "" : msg.text;
    const actions: MenuAction[] = [];
    if (id) {
      actions.push({
        label: tr.msg_reply,
        icon: "reply",
        run: () => {
          setReplyTo(props.peerId, id);
          composer?.focus();
        },
      });
    }
    if (text) {
      actions.push({
        label: tr.msg_copy,
        icon: "copy",
        run: () => void copyText(text).then((ok) => ok && addToast(tr.msg_copied, "success")),
      });
    }
    if (id && text && isMine(msg) && msg.status === "failed") {
      const reply = msg.reply_to ?? null;
      actions.push({
        label: tr.msg_retry,
        icon: "retry",
        run: () => void retry(msg, text, reply).catch(toastError),
      });
    }
    if (id) {
      actions.push({
        label: tr.msg_delete,
        icon: "trash",
        danger: true,
        run: () => void deleteAndSay(msg).catch(toastError),
      });
    }
    return actions;
  }

  /** Scrolls to a quoted message, loading older pages until it shows up. */
  async function showQuoted(msgId: string) {
    for (
      let page = 0;
      !findMessage(props.peerId, msgId) && hasOlder(props.peerId) && page < QUOTE_SEARCH_PAGES;
      page++
    ) {
      await loadOlder();
    }
    const el = scroller?.querySelector<HTMLElement>(`[data-msg-id="${CSS.escape(msgId)}"]`);
    if (!el) {
      addToast(t().reply_unavailable, "info");
      return;
    }
    el.scrollIntoView({ block: "center", behavior: "smooth" });
    el.classList.add("msg--flash");
    setTimeout(() => el.classList.remove("msg--flash"), 1200);
  }

  /** Shows files just offered as messages with their transfers. */
  function rememberSent(sent: readonly TransferInfo[]) {
    for (const info of sent) {
      upsertTransfer({ ...info, direction: "send", status: "active" });
      remember({
        ...(info.msg_id && { msg_id: info.msg_id }),
        from: ME,
        text: info.file_name,
        timestamp: Date.now(),
        status: "pending",
        file: {
          file_id: info.file_id,
          name: info.file_name,
          size: info.total_size,
          mime: "",
          kind: "file",
          duration_ms: null,
        },
      });
    }
  }

  async function offer(send: () => Promise<TransferInfo[]>) {
    try {
      rememberSent(await send());
    } catch (e) {
      toastError(e);
    }
  }

  // What another app shared and the user chose this chat for.
  onMount(() => {
    const peerId = props.peerId;
    const share = takeShare(peerId);
    if (!share) return;
    if (share.text) setDraft(draft() ? `${draft()}\n${share.text}` : share.text);
    if (share.files.length > 0) void offer(() => api.sendFiles(peerId, share.files));
  });

  function attach() {
    const peerId = props.peerId;
    void offer(() => api.pickAndSend(peerId));
  }

  // Files dropped on the chat (a browser hands them over; the desktop app
  // catches the drop natively and sends the paths it got).
  const [dragging, setDragging] = createSignal(false);
  let dragDepth = 0;
  const holdsFiles = (e: DragEvent) => e.dataTransfer?.types.includes("Files") === true;
  function onDragEnter(e: DragEvent) {
    if (!holdsFiles(e)) return;
    e.preventDefault();
    dragDepth++;
    setDragging(true);
  }
  function onDragLeave() {
    dragDepth = Math.max(0, dragDepth - 1);
    if (dragDepth === 0) setDragging(false);
  }
  function onDrop(e: DragEvent) {
    if (!holdsFiles(e)) return;
    e.preventDefault();
    dragDepth = 0;
    setDragging(false);
    const files = Array.from(e.dataTransfer?.files ?? []);
    const peerId = props.peerId;
    if (files.length > 0) void offer(() => api.sendFiles(peerId, files));
  }
  function onDropped(drop: { id: number }) {
    const [send, peerId] = [api.sendDropped, props.peerId];
    if (send) void offer(() => send(peerId, drop.id));
  }
  onMount(() => {
    const unlisten = [api.on("files_dragging", setDragging), api.on("files_dropped", onDropped)];
    onCleanup(() => void Promise.all(unlisten).then((offs) => offs.forEach((off) => off())));
  });

  function onPaste(e: ClipboardEvent) {
    const files = Array.from(e.clipboardData?.files ?? []);
    if (files.length === 0) return;
    e.preventDefault();
    const peerId = props.peerId;
    void offer(() => api.sendFiles(peerId, files));
  }

  function mediaSent(sent: MediaSent, file: UiFile) {
    trackMedia(sent.file_id);
    remember({ msg_id: sent.msg_id, from: ME, text: "", timestamp: Date.now(), status: "pending", file });
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape" && replyTo()) {
      // Cancels the answer before Escape would leave the chat.
      e.preventDefault();
      e.stopPropagation();
      setReplyTo(props.peerId, null);
      return;
    }
    // On a phone Enter is a new line; the send button sends.
    if (e.key === "Enter" && !e.shiftKey && !e.isComposing && matchMedia("(pointer: fine)").matches) {
      e.preventDefault();
      void send();
    }
  }

  return (
    <section
      class="screen chat"
      onDragEnter={onDragEnter}
      onDragOver={(e) => holdsFiles(e) && e.preventDefault()}
      onDragLeave={onDragLeave}
      onDrop={onDrop}
    >
      <Show when={dragging()}>
        <div class="chat__drop" aria-hidden="true">
          <Icon name="paperclip" size={28} />
          <span>{t().chat_drop}</span>
        </div>
      </Show>
      <TopBar
        onBack={isWide() ? undefined : back}
        testId="chat-header"
        actions={
          <button class="icon-btn" onClick={() => setVerifying(true)} aria-label={t().verify_open}>
            <Icon name="shield" />
          </button>
        }
      >
        <button
          class="chat__who"
          onClick={() => push({ name: "contact", peerId: props.peerId })}
          aria-label={t().chat_contact_info}
        >
          <Avatar peerId={props.peerId} name={avatarName(props.peerId)} size={36} />
          <span class="chat__who-text">
            <span class="chat__name">{name()}</span>
            <Show when={contacts[props.peerId]?.online}>
              <span class="topbar__subtitle">{t().chat_online}</span>
            </Show>
          </span>
        </button>
      </TopBar>

      <Show when={verifying()}>
        <SafetyNumber peerId={props.peerId} peerName={name()} onClose={() => setVerifying(false)} />
      </Show>

      <div class="chat__scroller screen__body" ref={scroller} onScroll={() => setAwayFromBottom(!nearBottom())}>
        <div ref={sentinel} class="chat__sentinel" />
        <Show when={loadingOlder()}>
          <div class="chat__loading">
            <span class="spinner" />
          </div>
        </Show>
        <Show
          when={!loading() && messages().length === 0}
          fallback={
            <div class="chat__timeline">
              <For each={timeline()}>
                {(item) =>
                  item.kind === "day" ? (
                    <div class="day" data-testid="day-separator">
                      <span>
                        {dayLabel(item.timestamp, Date.now(), locale(), {
                          today: t().day_today,
                          yesterday: t().day_yesterday,
                        })}
                      </span>
                    </div>
                  ) : (
                    <MessageBubble
                      peerId={props.peerId}
                      msg={item.message}
                      first={item.first}
                      last={item.last}
                      onMenu={(msg, anchor) => setMenuFor({ msg, anchor })}
                      onQuote={(id) => void showQuoted(id)}
                    />
                  )
                }
              </For>
            </div>
          }
        >
          <div class="empty">
            <span class="empty__icon">
              <Icon name="lock" size={28} />
            </span>
            <p>{t().chat_say_hello}</p>
            <p class="hint">{t().chat_encrypted}</p>
          </div>
        </Show>
      </div>

      <Show when={awayFromBottom()}>
        <button class="chat__jump" onClick={toBottom} aria-label={t().chat_to_latest}>
          <Icon name="arrow-left" style={{ transform: "rotate(-90deg)" }} />
        </button>
      </Show>

      <Show when={menuFor()}>
        {(open) => (
          <ActionMenu
            anchor={open().anchor}
            align={isMine(open().msg) ? "end" : "start"}
            actions={actionsFor(open().msg)}
            label={t().msg_actions}
            onClose={() => setMenuFor(null)}
          />
        )}
      </Show>

      <Show when={contacts[props.peerId]?.request && !contacts[props.peerId]?.blocked}>
        <div class="chat__notice" data-testid="request-notice">
          <p>{t().request_text}</p>
          <div class="chat__notice-actions">
            <button class="btn btn--primary" onClick={() => void acceptRequest()}>
              {t().request_accept}
            </button>
            <button class="btn btn--danger" onClick={() => void setBlocked(true)}>
              {t().contact_block}
            </button>
          </div>
        </div>
      </Show>

      <Show when={replyTo()}>
        {(id) => (
          <div class="reply-bar">
            <Icon name="reply" size={18} />
            <span class="reply-bar__quote quote">
              <Quote peerId={props.peerId} msg={findMessage(props.peerId, id())} />
            </span>
            <button class="icon-btn" aria-label={t().reply_cancel} onClick={() => setReplyTo(props.peerId, null)}>
              <Icon name="x" size={18} />
            </button>
          </div>
        )}
      </Show>

      <Show when={contacts[props.peerId]?.blocked}>
        <div class="chat__notice" data-testid="blocked-notice">
          <p>{t().blocked_text}</p>
          <div class="chat__notice-actions">
            <button class="btn btn--secondary" onClick={() => void setBlocked(false)}>
              {t().contact_unblock}
            </button>
          </div>
        </div>
      </Show>

      <footer class="composer" classList={{ "composer--hidden": contacts[props.peerId]?.blocked === true }}>
        <button class="icon-btn" onClick={attach} aria-label={t().composer_attach}>
          <Icon name="paperclip" />
        </button>
        <textarea
          ref={composer}
          class="composer__input"
          rows={1}
          placeholder={t().composer_placeholder}
          aria-label={t().composer_placeholder}
          value={draft()}
          onInput={(e) => {
            setDraft(e.currentTarget.value);
            resizeComposer();
          }}
          onKeyDown={onKey}
          onPaste={onPaste}
        />
        <Show when={draft().trim()} fallback={<RecordButton peer={props.peerId} onSent={mediaSent} />}>
          <button class="icon-btn icon-btn--accent" onClick={() => void send()} aria-label={t().composer_send}>
            <Icon name="send" />
          </button>
        </Show>
      </footer>
    </section>
  );
}

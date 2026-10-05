import { createEffect, createSignal, For, Match, on, onCleanup, onMount, Show, Switch, untrack } from "solid-js";
import "./Chat.css";
import Avatar from "../components/Avatar";
import Icon from "../components/Icon";
import SafetyNumber from "../components/SafetyNumber";
import StatusTick from "../components/StatusTick";
import TopBar from "../components/TopBar";
import FileCard from "../components/FileCard";
import RecordButton from "../components/media/RecordButton";
import RoundVideoBubble from "../components/media/RoundVideoBubble";
import VoiceBubble from "../components/media/VoiceBubble";
import { api, type ChatMessage, type MediaSent, type UiFile } from "../platform";
import {
  addMessage,
  getMessages,
  hasOlder,
  historyLoaded,
  mergeHistory,
  prependHistory,
  setHasOlder,
} from "../stores/chat";
import { avatarName, contacts, displayName, markConversationRead, noteMessage } from "../stores/contacts";
import { isWide } from "../stores/layout";
import { back, push } from "../stores/nav";
import { windowActive } from "../stores/presence";
import { upsertTransfer } from "../stores/transfers";
import { trackMedia } from "../stores/media";
import { toastError } from "../stores/toasts";
import { dayLabel, formatTime } from "../utils/format";
import { isMine, ME, noteOf, toChatMessage } from "../utils/messages";
import { buildTimeline } from "../utils/timeline";
import { locale, t } from "../i18n";

/** Messages fetched per page of history. */
const HISTORY_PAGE = 50;
/** Closer than this to the bottom, new messages keep the view scrolled down. */
const STICK_PX = 120;
/** The composer grows up to this height, then scrolls. */
const COMPOSER_MAX_PX = 144;

function MessageBubble(props: { msg: ChatMessage; first: boolean; last: boolean }) {
  const mine = () => isMine(props.msg);
  const note = () => noteOf(props.msg);
  const meta = () => (
    <span class="msg__meta">
      <span>{formatTime(props.msg.timestamp, locale())}</span>
      <Show when={mine() && props.msg.status}>{(status) => <StatusTick status={status()} />}</Show>
    </span>
  );
  return (
    <div
      class="msg"
      classList={{ "msg--out": mine(), "msg--in": !mine(), "msg--first": props.first, "msg--last": props.last }}
      data-testid="message"
      data-dir={mine() ? "out" : "in"}
    >
      <Switch
        fallback={
          <div class="bubble">
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
              <VoiceBubble file={file()} />
              {meta()}
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
  const [draft, setDraft] = createSignal("");
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

  async function send() {
    const text = draft().trim();
    if (!text) return;
    setDraft("");
    queueMicrotask(resizeComposer);
    try {
      const msgId = await api.sendMessage(props.peerId, text);
      remember({ msg_id: msgId, from: ME, text, timestamp: Date.now(), status: "pending" });
    } catch (e) {
      setDraft(text);
      toastError(e);
    }
  }

  async function attach() {
    try {
      for (const sent of await api.pickAndSend(props.peerId)) {
        upsertTransfer({ ...sent, direction: "send", status: "active" });
        remember({
          ...(sent.msg_id && { msg_id: sent.msg_id }),
          from: ME,
          text: sent.file_name,
          timestamp: Date.now(),
          status: "pending",
          file: {
            file_id: sent.file_id,
            name: sent.file_name,
            size: sent.total_size,
            mime: "",
            kind: "file",
            duration_ms: null,
          },
        });
      }
    } catch (e) {
      toastError(e);
    }
  }

  function mediaSent(sent: MediaSent, file: UiFile) {
    trackMedia(sent.file_id);
    remember({ msg_id: sent.msg_id, from: ME, text: "", timestamp: Date.now(), status: "pending", file });
  }

  function onKey(e: KeyboardEvent) {
    // On a phone Enter is a new line; the send button sends.
    if (e.key === "Enter" && !e.shiftKey && !e.isComposing && matchMedia("(pointer: fine)").matches) {
      e.preventDefault();
      void send();
    }
  }

  return (
    <section class="screen chat">
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
                    <MessageBubble msg={item.message} first={item.first} last={item.last} />
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

      <footer class="composer">
        <button class="icon-btn" onClick={() => void attach()} aria-label={t().composer_attach}>
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

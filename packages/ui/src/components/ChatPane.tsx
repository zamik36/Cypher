import { createSignal, createEffect, on, onMount, onCleanup, For, Show } from "solid-js";
import { api, type ChatMessage, type MediaSent, type MessageStatus, type UiFile, type UiMessage } from "../platform";
import { chatsByPeer, addMessage, getMessages, setMessages } from "../stores/chat";
import { connection, setActivePeer, shortName } from "../stores/connection";
import { upsertTransfer } from "../stores/transfers";
import { trackMedia } from "../stores/media";
import VoiceBubble from "./media/VoiceBubble";
import RoundVideoBubble from "./media/RoundVideoBubble";
import RecordButton from "./media/RecordButton";
import { addToast } from "../stores/toasts";
import { SendIcon, ChatIcon, UploadIcon } from "./Icons";
import type { Page } from "./Sidebar";
import { t } from "../i18n";

interface ChatPaneProps {
  onNavigate: (p: Page) => void;
}

const HISTORY_PAGE = 200;

function formatTime(ts: number): string {
  return new Date(ts).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

const STATUS_MARK: Record<MessageStatus, string> = {
  pending: "⏲",
  sent: "✓",
  queued: "✓",
  delivered: "✓✓",
  read: "✓✓",
  failed: "!",
};

/** Chat-list preview of a message. */
export function previewText(text: string, file?: UiFile | null): string {
  if (!file) return text;
  if (file.kind === "voice") return `🎤 ${t().media_voice}`;
  if (file.kind === "video_note") return `⏺ ${t().media_video}`;
  return `📎 ${text}`;
}

/** The note a message carries, if it renders as a player rather than text. */
const noteOf = (m: ChatMessage) => (m.file && m.file.kind !== "file" ? m.file : undefined);

function fromHistory(peer: string, m: UiMessage): ChatMessage {
  return {
    msg_id: m.msg_id,
    from: m.outgoing ? "me" : peer,
    text: previewText(m.text, m.file),
    timestamp: m.timestamp,
    status: m.status,
    file: m.file,
  };
}

export default function ChatPane(props: ChatPaneProps) {
  const [draft, setDraft] = createSignal("");
  const [loadingHistory, setLoadingHistory] = createSignal(false);
  let messagesRef: HTMLDivElement | undefined;
  let chatAreaRef: HTMLDivElement | undefined;

  const activePeer = () => connection.activePeerId;
  const activeMessages = () => {
    const peer = activePeer();
    return peer ? getMessages(peer) : [];
  };
  const activePeerInfo = () => connection.peers.find((p) => p.peerId === activePeer());

  let loadingForPeer: string | null = null;
  createEffect(() => {
    const peer = activePeer();
    if (!peer || getMessages(peer).length > 0) return;
    loadingForPeer = peer;
    setLoadingHistory(true);
    api
      .getHistory(peer, HISTORY_PAGE)
      .then((history) => {
        if (loadingForPeer === peer && history.length > 0) {
          setMessages(
            peer,
            history.reverse().map((m) => fromHistory(peer, m)),
          );
        }
      })
      .catch((e: unknown) => console.warn("Failed to load history:", e))
      .finally(() => {
        if (loadingForPeer === peer) setLoadingHistory(false);
      });
  });

  createEffect(() => {
    const peer = activePeer();
    if (!peer) return;
    const unread = getMessages(peer).flatMap((m) =>
      m.from !== "me" && m.msg_id && m.status !== "read" ? [m.msg_id] : [],
    );
    if (unread.length > 0) {
      // Best effort: the next visit retries whatever stayed unread.
      api.markRead(peer, unread).catch(() => undefined);
    }
  });

  createEffect(
    on(
      () => activeMessages().length,
      () =>
        queueMicrotask(() => {
          if (messagesRef) messagesRef.scrollTop = messagesRef.scrollHeight;
        }),
    ),
  );

  onMount(() => {
    const vv = window.visualViewport;
    if (!vv) return;
    const onResize = () => {
      if (!chatAreaRef) return;
      const offset = window.innerHeight - vv.height;
      chatAreaRef.style.paddingBottom = offset > 0 ? `${offset}px` : "";
    };
    vv.addEventListener("resize", onResize);
    onCleanup(() => vv.removeEventListener("resize", onResize));
  });

  async function send() {
    const text = draft().trim();
    const peer = activePeer();
    if (!text || !peer) return;
    try {
      const msgId = await api.sendMessage(peer, text);
      addMessage(peer, { msg_id: msgId, from: "me", text, timestamp: Date.now(), status: "pending" });
      setDraft("");
    } catch (e) {
      addToast(String(e), "error");
    }
  }

  async function attach() {
    const peer = activePeer();
    if (!peer) return;
    try {
      for (const tr of await api.pickAndSend(peer)) {
        upsertTransfer(tr);
        addMessage(peer, {
          ...(tr.msg_id && { msg_id: tr.msg_id }),
          from: "me",
          text: `📎 ${tr.file_name}`,
          timestamp: Date.now(),
          status: "pending",
        });
      }
    } catch (e) {
      addToast(String(e), "error");
    }
  }

  function mediaSent(peer: string, sent: MediaSent, file: UiFile) {
    trackMedia(sent.file_id);
    addMessage(peer, {
      msg_id: sent.msg_id,
      from: "me",
      text: previewText("", file),
      timestamp: Date.now(),
      status: "pending",
      file,
    });
  }

  return (
    <div class="chat-pane">
      <Show when={connection.peers.length === 0}>
        <div class="empty-state">
          <ChatIcon width="48" height="48" />
          <p>{t().chat_empty}</p>
          <button class="btn-primary" onClick={() => props.onNavigate("home")}>
            {t().chat_go_home}
          </button>
        </div>
      </Show>

      <Show when={connection.peers.length > 0}>
        <div class="chat-layout">
          <div class="peer-list">
            <div class="peer-list-header">{t().chat_header}</div>
            <For each={connection.peers}>
              {(peer) => {
                const lastMsg = () => {
                  const msgs = chatsByPeer[peer.peerId] ?? [];
                  return msgs[msgs.length - 1];
                };
                return (
                  <button
                    class={`peer-item ${activePeer() === peer.peerId ? "active" : ""}`}
                    onClick={() => setActivePeer(peer.peerId)}
                  >
                    <div class="peer-avatar">
                      {peer.displayName.slice(0, 2).toUpperCase()}
                      <span class={`online-dot ${peer.online ? "online" : "offline"}`} />
                    </div>
                    <div class="peer-info">
                      <span class="peer-name">{peer.displayName}</span>
                      <span class="peer-last-msg">{lastMsg()?.text.slice(0, 30) || t().chat_no_messages}</span>
                    </div>
                  </button>
                );
              }}
            </For>
          </div>

          <div class="chat-area" ref={chatAreaRef}>
            <Show
              when={activePeer()}
              fallback={
                <div class="empty-state">
                  <ChatIcon width="48" height="48" />
                  <p>{t().chat_select}</p>
                </div>
              }
            >
              {(peer) => (
                <>
                  <div class="chat-header">
                    <div class="peer-avatar small">
                      {shortName(peer()).slice(0, 2).toUpperCase()}
                      <span class={`online-dot ${activePeerInfo()?.online ? "online" : "offline"}`} />
                    </div>
                    <span>{shortName(peer())}</span>
                  </div>

                  <Show when={loadingHistory()}>
                    <div class="empty-state">
                      <p>{t().chat_loading}</p>
                    </div>
                  </Show>
                  <Show when={!loadingHistory() && activeMessages().length === 0}>
                    <div class="empty-state">
                      <ChatIcon width="48" height="48" />
                      <p>{t().chat_say_hello}</p>
                    </div>
                  </Show>

                  <div class="messages" ref={messagesRef}>
                    <For each={activeMessages()}>
                      {(msg) => {
                        const isMine = msg.from === "me";
                        return (
                          <div class={`message-group ${isMine ? "mine" : "theirs"}`}>
                            <div class={`avatar ${isMine ? "me" : "peer"}`}>{isMine ? t().chat_me : t().chat_peer}</div>
                            <div class="message-content">
                              <Show when={noteOf(msg)} fallback={<div class="bubble">{msg.text}</div>}>
                                {(note) => (
                                  <Show when={note().kind === "voice"} fallback={<RoundVideoBubble file={note()} />}>
                                    <div class="bubble media">
                                      <VoiceBubble file={note()} />
                                    </div>
                                  </Show>
                                )}
                              </Show>
                              <span class="message-time">
                                {formatTime(msg.timestamp)}
                                <Show when={isMine && msg.status}>
                                  {(status) => (
                                    <>
                                      {" "}
                                      <span class={`message-status ${status()}`}>{STATUS_MARK[status()]}</span>
                                    </>
                                  )}
                                </Show>
                              </span>
                            </div>
                          </div>
                        );
                      }}
                    </For>
                  </div>

                  <div class="input-row">
                    <button class="btn-icon" onClick={attach} title={t().files_choose}>
                      <UploadIcon />
                    </button>
                    <input
                      type="text"
                      value={draft()}
                      onInput={(e) => setDraft(e.currentTarget.value)}
                      onKeyDown={(e) => e.key === "Enter" && send()}
                      placeholder={t().chat_placeholder}
                    />
                    <Show
                      when={draft().trim()}
                      fallback={<RecordButton peer={peer()} onSent={(sent, file) => mediaSent(peer(), sent, file)} />}
                    >
                      <button class="btn-icon" onClick={send}>
                        <SendIcon />
                      </button>
                    </Show>
                  </div>
                </>
              )}
            </Show>
          </div>
        </div>
      </Show>
    </div>
  );
}

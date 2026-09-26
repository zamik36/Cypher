import { createSignal, createEffect, onMount, onCleanup, For, Show } from "solid-js";
import { api, type ChatMessage, type MessageStatus, type UiMessage } from "../api/tauri";
import { chatsByPeer, addMessage, getMessages, setMessages } from "../stores/chat";
import { connection, setActivePeer, shortName } from "../stores/connection";
import { upsertTransfer } from "../stores/transfers";
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

function fromHistory(peer: string, m: UiMessage): ChatMessage {
  return {
    msg_id: m.msg_id,
    from: m.outgoing ? "me" : peer,
    text: m.file ? `📎 ${m.text}` : m.text,
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
  const activeMessages = () => (activePeer() ? getMessages(activePeer()!) : []);
  const activePeerInfo = () => connection.peers.find((p) => p.peerId === activePeer());

  let loadingForPeer: string | null = null;
  createEffect(() => {
    const peer = activePeer();
    if (!peer || getMessages(peer).length > 0) return;
    loadingForPeer = peer;
    setLoadingHistory(true);
    api.getHistory(peer, HISTORY_PAGE)
      .then((history) => {
        if (loadingForPeer === peer && history.length > 0) {
          setMessages(peer, history.reverse().map((m) => fromHistory(peer, m)));
        }
      })
      .catch((e) => console.warn("Failed to load history:", e))
      .finally(() => { if (loadingForPeer === peer) setLoadingHistory(false); });
  });

  createEffect(() => {
    const peer = activePeer();
    if (!peer) return;
    const unread = getMessages(peer)
      .filter((m) => m.from !== "me" && m.msg_id && m.status !== "read")
      .map((m) => m.msg_id!);
    if (unread.length > 0) {
      void api.markRead(peer, unread).catch(() => {});
    }
  });

  createEffect(() => {
    const peer = activePeer();
    if (peer) void chatsByPeer[peer]?.length;
    queueMicrotask(() => { if (messagesRef) messagesRef.scrollTop = messagesRef.scrollHeight; });
  });

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
      for (const tr of await api.browseAndSend(peer)) {
        upsertTransfer(tr);
        addMessage(peer, {
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

  return (
    <div class="chat-pane">
      <Show when={connection.peers.length === 0}>
        <div class="empty-state">
          <ChatIcon width="48" height="48" />
          <p>{t().chat_empty}</p>
          <button class="btn-primary" onClick={() => props.onNavigate("home")}>{t().chat_go_home}</button>
        </div>
      </Show>

      <Show when={connection.peers.length > 0}>
        <div class="chat-layout">
          <div class="peer-list">
            <div class="peer-list-header">{t().chat_header}</div>
            <For each={connection.peers}>
              {(peer) => {
                const lastMsg = () => {
                  const msgs = chatsByPeer[peer.peerId] || [];
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
                      <span class="peer-last-msg">{lastMsg()?.text?.slice(0, 30) || t().chat_no_messages}</span>
                    </div>
                  </button>
                );
              }}
            </For>
          </div>

          <div class="chat-area" ref={chatAreaRef}>
            <Show when={activePeer()} fallback={
              <div class="empty-state">
                <ChatIcon width="48" height="48" />
                <p>{t().chat_select}</p>
              </div>
            }>
              <div class="chat-header">
                <div class="peer-avatar small">
                  {shortName(activePeer()!).slice(0, 2).toUpperCase()}
                  <span class={`online-dot ${activePeerInfo()?.online ? "online" : "offline"}`} />
                </div>
                <span>{shortName(activePeer()!)}</span>
              </div>

              <Show when={loadingHistory()}>
                <div class="empty-state"><p>{t().chat_loading}</p></div>
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
                        <div class={`avatar ${isMine ? "me" : "peer"}`}>
                          {isMine ? t().chat_me : t().chat_peer}
                        </div>
                        <div class="message-content">
                          <div class="bubble">{msg.text}</div>
                          <span class="message-time">
                            {formatTime(msg.timestamp)}
                            <Show when={isMine && msg.status}>
                              {" "}<span class={`message-status ${msg.status}`}>{STATUS_MARK[msg.status!]}</span>
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
                <button class="btn-icon" onClick={send} disabled={!draft().trim()}>
                  <SendIcon />
                </button>
              </div>
            </Show>
          </div>
        </div>
      </Show>
    </div>
  );
}

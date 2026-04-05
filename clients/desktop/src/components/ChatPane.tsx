import { createSignal, createEffect, onMount, onCleanup, For, Show } from "solid-js";
import { api } from "../api/tauri";
import { chatsByPeer, addMessage, getMessages, setMessages } from "../stores/chat";
import { connection, setActivePeer, shortName } from "../stores/connection";
import { SendIcon, ChatIcon } from "./Icons";
import type { Page } from "./Sidebar";
import { t } from "../i18n";

interface ChatPaneProps {
  onNavigate: (p: Page) => void;
}

function formatTime(ts: number): string {
  return new Date(ts).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

export default function ChatPane(props: ChatPaneProps) {
  const [draft, setDraft] = createSignal("");
  let messagesRef: HTMLDivElement | undefined;
  let chatAreaRef: HTMLDivElement | undefined;

  const [loadingHistory, setLoadingHistory] = createSignal(false);
  const activePeer = () => connection.activePeerId;
  const activeMessages = () => activePeer() ? getMessages(activePeer()!) : [];
  const activePeerInfo = () => connection.peers.find((p) => p.peerId === activePeer());

  // Load message history from SQLite when selecting a peer with no in-memory messages.
  let loadingForPeer: string | null = null;
  createEffect(() => {
    const peer = activePeer();
    if (!peer) return;
    if (getMessages(peer).length > 0) return;
    loadingForPeer = peer;
    setLoadingHistory(true);
    api.getHistory(peer, 200).then((history) => {
      if (loadingForPeer !== peer) return;
      if (history.length > 0) {
        const msgs = history.reverse().map((m) => ({
          from: m.direction === "sent" ? "me" : peer,
          text: m.text,
          timestamp: m.timestamp,
        }));
        setMessages(peer, msgs);
      }
    }).catch((e) => console.warn("Failed to load history:", e))
      .finally(() => { if (loadingForPeer === peer) setLoadingHistory(false); });
  });

  // Auto-scroll to bottom when new messages arrive
  createEffect(() => {
    const peer = activePeer();
    if (peer) void (chatsByPeer[peer]?.length);
    if (messagesRef) {
      setTimeout(() => messagesRef!.scrollTop = messagesRef!.scrollHeight, 10);
    }
  });

  // Handle virtual keyboard resize (keeps input visible above keyboard)
  onMount(() => {
    if (window.visualViewport) {
      const vv = window.visualViewport;
      const onResize = () => {
        if (chatAreaRef) {
          const offset = window.innerHeight - vv.height;
          chatAreaRef.style.paddingBottom = offset > 0 ? `${offset}px` : "";
        }
      };
      vv.addEventListener("resize", onResize);
      onCleanup(() => vv.removeEventListener("resize", onResize));
    }
  });

  async function send() {
    const text = draft().trim();
    const peer = activePeer();
    if (!text || !peer) return;
    try {
      await api.sendMessage(peer, text);
      addMessage(peer, { from: "me", text, timestamp: Date.now() });
      setDraft("");
    } catch (e) {
      console.error("send_message failed:", e);
    }
  }

  // No peers at all
  const noPeers = () => connection.peers.length === 0;

  return (
    <div class="chat-pane">
      <Show when={noPeers()}>
        <div class="empty-state">
          <ChatIcon width="48" height="48" />
          <p>{t().chat_empty}</p>
          <button class="btn-primary" onClick={() => props.onNavigate("home")}>{t().chat_go_home}</button>
        </div>
      </Show>

      <Show when={!noPeers()}>
        <div class="chat-layout">
          <div class="peer-list">
            <div class="peer-list-header">{t().chat_header}</div>
            <For each={connection.peers}>
              {(peer) => {
                const isActive = () => activePeer() === peer.peerId;
                const peerMessages = () => chatsByPeer[peer.peerId] || [];
                const lastMsg = () => {
                  const msgs = peerMessages();
                  return msgs.length > 0 ? msgs[msgs.length - 1] : null;
                };
                return (
                  <button
                    class={`peer-item ${isActive() ? "active" : ""}`}
                    onClick={() => setActivePeer(peer.peerId)}
                  >
                    <div class="peer-avatar">
                      {peer.displayName.slice(0, 2).toUpperCase()}
                      <span class={`online-dot ${peer.online ? "online" : "offline"}`} />
                    </div>
                    <div class="peer-info">
                      <span class="peer-name">{peer.displayName}</span>
                      <span class="peer-last-msg">
                        {lastMsg()?.text?.slice(0, 30) || t().chat_no_messages}
                      </span>
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
                <Show when={activePeerInfo() && !activePeerInfo()!.online}>
                  <span class="offline-badge">{t().chat_offline_badge}</span>
                </Show>
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
                        <div class={`avatar ${isMine ? "me" : "peer"}`}>
                          {isMine ? t().chat_me : t().chat_peer}
                        </div>
                        <div class="message-content">
                          <div class="bubble">{msg.text}</div>
                          <span class="message-time">{formatTime(msg.timestamp)}</span>
                        </div>
                      </div>
                    );
                  }}
                </For>
              </div>

              <Show when={activePeerInfo() && !activePeerInfo()!.online}>
                <div class="offline-banner">{t().chat_offline_hint}</div>
              </Show>
              <div class="input-row">
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

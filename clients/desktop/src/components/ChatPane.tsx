import { createSignal, createEffect, For, Show } from "solid-js";
import { api } from "../api/tauri";
import { chatsByPeer, addMessage, getMessages } from "../stores/chat";
import { connection, setActivePeer, shortName } from "../stores/connection";
import { SendIcon, ChatIcon } from "./Icons";
import type { Page } from "./Sidebar";

interface ChatPaneProps {
  onNavigate: (p: Page) => void;
}

function formatTime(ts: number): string {
  return new Date(ts).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

export default function ChatPane(props: ChatPaneProps) {
  const [draft, setDraft] = createSignal("");
  let messagesRef: HTMLDivElement | undefined;

  const activePeer = () => connection.activePeerId;
  const activeMessages = () => activePeer() ? getMessages(activePeer()!) : [];

  // Auto-scroll to bottom when new messages arrive
  createEffect(() => {
    const peer = activePeer();
    if (peer) void (chatsByPeer[peer]?.length);
    if (messagesRef) {
      setTimeout(() => messagesRef!.scrollTop = messagesRef!.scrollHeight, 10);
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
          <p>Connect to a peer first to start chatting.</p>
          <button class="btn-primary" onClick={() => props.onNavigate("home")}>Go to Home</button>
        </div>
      </Show>

      <Show when={!noPeers()}>
        <div class="chat-layout">
          {/* Peer list */}
          <div class="peer-list">
            <div class="peer-list-header">Chats</div>
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
                    <div class="peer-avatar">{peer.displayName.slice(0, 2).toUpperCase()}</div>
                    <div class="peer-info">
                      <span class="peer-name">{peer.displayName}</span>
                      <span class="peer-last-msg">
                        {lastMsg()?.text?.slice(0, 30) || "No messages yet"}
                      </span>
                    </div>
                  </button>
                );
              }}
            </For>
          </div>

          {/* Active chat */}
          <div class="chat-area">
            <Show when={activePeer()} fallback={
              <div class="empty-state">
                <ChatIcon width="48" height="48" />
                <p>Select a chat from the list</p>
              </div>
            }>
              <div class="chat-header">
                <div class="peer-avatar small">
                  {shortName(activePeer()!).slice(0, 2).toUpperCase()}
                </div>
                <span>{shortName(activePeer()!)}</span>
              </div>

              <Show when={activeMessages().length === 0}>
                <div class="empty-state">
                  <ChatIcon width="48" height="48" />
                  <p>No messages yet. Say hello!</p>
                </div>
              </Show>

              <div class="messages" ref={messagesRef}>
                <For each={activeMessages()}>
                  {(msg) => {
                    const isMine = msg.from === "me";
                    return (
                      <div class={`message-group ${isMine ? "mine" : "theirs"}`}>
                        <div class={`avatar ${isMine ? "me" : "peer"}`}>
                          {isMine ? "Me" : "P"}
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

              <div class="input-row">
                <input
                  type="text"
                  value={draft()}
                  onInput={(e) => setDraft(e.currentTarget.value)}
                  onKeyDown={(e) => e.key === "Enter" && send()}
                  placeholder="Type a message..."
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

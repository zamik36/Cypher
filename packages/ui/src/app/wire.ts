import {
  api,
  onAnonymityLevel,
  onConnected,
  onDisconnected,
  onError,
  onFileComplete,
  onFileFailed,
  onFileOffered,
  onFileProgress,
  onMessage,
  onMessageStatus,
  onPeerConnected,
  onPeerProfile,
  onContactRequest,
  onSuperseded,
  onUpdateRequired,
} from "../platform";
import { connection, connectGateway, linkEvent } from "../stores/connection";
import {
  displayName,
  ensureContact,
  loadConversations,
  noteMessage,
  setAllOffline,
  setContactName,
  setContactRequest,
  contacts,
  setContactOnline,
  setLastStatus,
} from "../stores/contacts";
import { addMessage, peerOf, setMessageStatus } from "../stores/chat";
import { hasTransfer, upsertTransfer } from "../stores/transfers";
import { setMediaProgress, trackMedia } from "../stores/media";
import { setInvitesReady } from "../stores/invite";
import { onPushKey, onPushState, startPush } from "../stores/push";
import { setSharesReady } from "../stores/share";
import { addToast, toastError } from "../stores/toasts";
import { anonymousSettings, setOnionUp } from "../stores/anonymity";
import { windowActive } from "../stores/presence";
import { installNavigation, replace, top } from "../stores/nav";
import { notifyMessage } from "../utils/notifications";
import { previewOf, toChatMessage } from "../utils/messages";
import { reasonText } from "../utils/reasons";
import { t } from "../i18n";

/** Whether the user is looking at the conversation with `peerId` right now. */
export function viewing(peerId: string): boolean {
  const screen = top();
  return windowActive() && screen.name === "chat" && screen.peerId === peerId;
}

/** Reloads the chat list from the client: names, last messages, unread counts. */
export async function refreshConversations(): Promise<void> {
  try {
    loadConversations(await api.getConversations());
  } catch (e) {
    console.warn("Failed to load conversations:", e);
  }
}

/**
 * Connects the client's events to the stores, then connects to the server.
 * Returns the cleanup.
 */
export async function startApp(): Promise<() => void> {
  const stopNavigation = installNavigation();
  setInvitesReady(true);
  setSharesReady(true);
  const unsubscribe = await Promise.all([
    onConnected(() => {
      linkEvent("connected");
      // Every session asks again: a restarted client (new network settings)
      // starts without it, and registering renews the server's copy.
      void startPush();
    }),
    onDisconnected(() => {
      linkEvent("disconnected");
      setAllOffline();
    }),
    onSuperseded(() => {
      linkEvent("superseded");
      setAllOffline();
    }),
    onUpdateRequired(() => {
      linkEvent("update_required");
      setAllOffline();
    }),
    onPeerConnected((peerId) => {
      ensureContact(peerId, true);
      // Someone used our invite (or we used theirs): open the new chat.
      if (top().name === "new-chat") replace({ name: "chat", peerId });
      else addToast(t().toast_contact_added(displayName(peerId)), "success");
    }),
    onPeerProfile(({ peer, name }) => setContactName(peer, name)),
    onContactRequest((peer) => {
      setContactRequest(peer, true);
      addToast(t().toast_request, "info");
    }),
    onMessage((ui) => {
      const msg = toChatMessage(ui.from, ui);
      if (msg.file && msg.file.kind !== "file") trackMedia(msg.file.file_id);
      // A copy sent again (its receipt was lost) is neither new nor unread.
      if (!addMessage(ui.from, msg)) return;
      const seen = viewing(ui.from);
      noteMessage(ui.from, msg, seen);
      setContactOnline(ui.from, true);
      // Requests stay quiet until accepted.
      if (!seen && !contacts[ui.from]?.request) void notifyMessage(displayName(ui.from), previewOf(msg).text);
    }),
    onMessageStatus(({ msg_id, status }) => {
      setMessageStatus(msg_id, status);
      const peer = peerOf(msg_id);
      if (!peer) return;
      setLastStatus(peer, msg_id, status);
      if (status === "sent" || status === "queued") setContactOnline(peer, status === "sent");
    }),
    onFileOffered((offer) => {
      upsertTransfer({
        file_id: offer.file_id,
        file_name: offer.name,
        total_size: offer.size,
        progress: 0,
        direction: "receive",
        status: "offered",
      });
    }),
    onFileProgress(({ file_id, progress }) => {
      setMediaProgress(file_id, progress);
      if (hasTransfer(file_id)) upsertTransfer({ file_id, progress, status: "active" });
    }),
    onFileComplete((fileId) => {
      setMediaProgress(fileId, 1);
      if (hasTransfer(fileId)) upsertTransfer({ file_id: fileId, progress: 1, status: "complete" });
    }),
    onFileFailed(({ file_id, reason }) => {
      if (hasTransfer(file_id)) upsertTransfer({ file_id, status: "error", error: reasonText(reason) });
    }),
    onError(toastError),
    onAnonymityLevel(({ level }) => setOnionUp(level > 0)),
    api.on("push_key", (key) => void onPushKey(key)),
    api.on("push_state", onPushState),
  ]);

  await connectGateway(() =>
    api.connectToGateway(connection.gatewayAddr, anonymousSettings.enabled, anonymousSettings.bridgeLines),
  );
  await refreshConversations();

  return () => {
    setInvitesReady(false);
    setSharesReady(false);
    stopNavigation();
    for (const off of unsubscribe) off();
  };
}

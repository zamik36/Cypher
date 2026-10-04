import { convertFileSrc, invoke, type InvokeArgs } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { isPermissionGranted, requestPermission, sendNotification } from "@tauri-apps/plugin-notification";
import type { ConversationEntry, LinkInfo, MediaSent, Platform, TransferInfo, UiMessage } from "@cypher/ui/platform";

let stopLevels: UnlistenFn | null = null;

/**
 * Android grants RECORD_AUDIO through the WebView's permission prompt; the
 * native recorder can only open the microphone after that.
 */
async function ensureMicPermission() {
  if (!/Android/i.test(navigator.userAgent)) return;
  const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
  for (const track of stream.getTracks()) track.stop();
}

function releaseLevels() {
  stopLevels?.();
  stopLevels = null;
}

/** Runs a command that returns nothing. */
async function command(cmd: string, args?: InvokeArgs): Promise<void> {
  await invoke(cmd, args);
}

export const tauriPlatform: Platform = {
  kind: "desktop",
  hasIdentity: () => invoke<boolean>("has_identity"),
  createIdentity: (nickname, passphrase) => invoke<string>("create_identity", { nickname, passphrase }),
  unlockIdentity: (passphrase) => invoke<[string, string]>("unlock_identity", { passphrase }),
  importMnemonic: (mnemonic, nickname, passphrase) =>
    invoke<string>("import_mnemonic", { mnemonic, nickname, passphrase }),
  exportMnemonic: (passphrase) => invoke<string>("export_mnemonic", { passphrase }),
  connectToGateway: (addr, anonymous, bridges) => invoke<string>("connect_to_gateway", { addr, anonymous, bridges }),
  applyAnonymousSettings: (anonymous, bridges) => command("apply_anonymous_settings", { anonymous, bridges }),
  createLink: () => invoke<LinkInfo>("create_link"),
  joinLink: (linkId) => invoke<string>("join_link", { linkId }),
  sendMessage: (peerId, text) => invoke<string>("send_message", { peerId, text }),
  markRead: (peerId, msgIds) => command("mark_read", { peerId, msgIds }),
  pickAndSend: (peerId) => invoke<TransferInfo[]>("browse_and_send", { peerId }),
  acceptFile: (fileId) => command("accept_file", { fileId }),
  cancelTransfer: (fileId) => command("cancel_transfer", { fileId }),
  generateQr: (linkId) => invoke<string>("generate_qr", { linkId }),
  safetyNumber: (peerId) => invoke<string>("safety_number", { peerId }),
  reconnect: () => command("reconnect"),
  startVoice: async (onLevel) => {
    await ensureMicPermission();
    releaseLevels();
    stopLevels = await listen<number>("cypher://voice_level", (e) => onLevel(e.payload));
    try {
      await command("voice_start");
    } catch (e) {
      releaseLevels();
      throw e;
    }
  },
  stopVoice: async (peerId) => {
    try {
      return await invoke<MediaSent | null>("voice_stop", { peerId });
    } finally {
      releaseLevels();
    }
  },
  cancelVoice: async () => {
    releaseLevels();
    await command("voice_cancel");
  },
  sendVideoNote: async (peerId, note) => {
    const video = new Uint8Array(await note.blob.arrayBuffer());
    const body = new Uint8Array(note.poster.length + video.length);
    body.set(note.poster);
    body.set(video, note.poster.length);
    return invoke<MediaSent>("send_video_note", body, {
      headers: {
        "x-peer": peerId,
        "x-mime": note.mime,
        "x-duration-ms": String(note.durationMs),
        "x-poster-len": String(note.poster.length),
      },
    });
  },
  mediaUrl: (fileId) => Promise.resolve(convertFileSrc(fileId, "cypher-media")),
  getConversations: () => invoke<ConversationEntry[]>("get_conversations"),
  getHistory: (peerId, limit, before) => invoke<UiMessage[]>("get_history", { peerId, limit, before }),
  clearChatHistory: () => command("clear_chat_history"),
  on: (channel, cb) => listen<Parameters<typeof cb>[0]>(`cypher://${channel}`, (e) => cb(e.payload)),
  notifications: {
    supported: () => true,
    permission: async () => ((await isPermissionGranted()) ? "granted" : "default"),
    request: () => requestPermission(),
    send: (title, body) => {
      sendNotification({ title, body, group: "messages", autoCancel: true });
      return Promise.resolve();
    },
  },
};

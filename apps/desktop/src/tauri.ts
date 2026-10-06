import { convertFileSrc, invoke, type InvokeArgs } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { isPermissionGranted, requestPermission, sendNotification } from "@tauri-apps/plugin-notification";
import type { ConversationEntry, LinkInfo, MediaSent, Platform, TransferInfo, UiMessage } from "@cypher/ui/platform";

let stopLevels: UnlistenFn | null = null;

const isAndroid = /Android/i.test(navigator.userAgent);

/**
 * Android grants RECORD_AUDIO through the WebView's permission prompt; the
 * native recorder can only open the microphone after that.
 */
async function ensureMicPermission() {
  if (!isAndroid) return;
  const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
  for (const track of stream.getTracks()) track.stop();
}

function releaseLevels() {
  stopLevels?.();
  stopLevels = null;
}

/** The blob's bytes as base64, encoded natively by the browser. */
function base64Of(blob: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => {
      const url = reader.result as string;
      resolve(url.slice(url.indexOf(",") + 1));
    };
    reader.onerror = () => reject(reader.error ?? new Error("cannot read the video note"));
    reader.readAsDataURL(blob);
  });
}

/** Runs a command that returns nothing. */
async function command(cmd: string, args?: InvokeArgs): Promise<void> {
  await invoke(cmd, args);
}

export const tauriPlatform: Platform = {
  kind: "desktop",
  capabilities: { tor: true, gatewayConfig: !isAndroid, revealFile: !isAndroid },
  hasIdentity: () => invoke<boolean>("has_identity"),
  createIdentity: (nickname, passphrase) => invoke<string>("create_identity", { nickname, passphrase }),
  unlockIdentity: (passphrase) => invoke<[string, string]>("unlock_identity", { passphrase }),
  importMnemonic: (mnemonic, nickname, passphrase) =>
    invoke<string>("import_mnemonic", { mnemonic, nickname, passphrase }),
  eraseDevice: () => command("erase_device"),
  exportMnemonic: (passphrase) => invoke<string>("export_mnemonic", { passphrase }),
  connectToGateway: (addr, anonymous, bridges) => invoke<string>("connect_to_gateway", { addr, anonymous, bridges }),
  applyAnonymousSettings: (anonymous, bridges) => command("apply_anonymous_settings", { anonymous, bridges }),
  createLink: () => invoke<LinkInfo>("create_link"),
  joinLink: (linkId) => invoke<string>("join_link", { linkId }),
  sendMessage: (peerId, text, replyTo) => invoke<string>("send_message", { peerId, text, replyTo: replyTo ?? null }),
  deleteMessage: (peerId, msgId, timestamp) =>
    command("delete_message", { peerId, msgId, timestamp: Math.round(timestamp) }),
  markRead: (peerId, msgIds) => command("mark_read", { peerId, msgIds }),
  pickAndSend: (peerId) => invoke<TransferInfo[]>("browse_and_send", { peerId }),
  sendDropped: (peerId, dropId) => invoke<TransferInfo[]>("send_dropped", { peerId, id: dropId }),
  sendFiles: async (peerId, files) => {
    const sent: TransferInfo[] = [];
    for (const file of files) {
      const payload = isAndroid ? { data: await base64Of(file) } : new Uint8Array(await file.arrayBuffer());
      sent.push(
        await invoke<TransferInfo>("send_bytes", payload, {
          headers: {
            "x-peer": peerId,
            "x-name": encodeURIComponent(file.name || "file"),
            "x-mime": encodeURIComponent(file.type || "application/octet-stream"),
          },
        }),
      );
    }
    return sent;
  },
  acceptFile: (fileId) => command("accept_file", { fileId }),
  cancelTransfer: (fileId) => command("cancel_transfer", { fileId }),
  fileSaved: (fileId) => invoke<boolean>("file_saved", { fileId }),
  openFile: (file) => command("open_file", { fileId: file.file_id }),
  revealFile: (fileId) => command("reveal_file", { fileId }),
  generateQr: (linkId) => invoke<string>("generate_qr", { linkId }),
  ...(isAndroid && {
    scanQr: async () => {
      const scanner = await import("@tauri-apps/plugin-barcode-scanner");
      if ((await scanner.requestPermissions()) !== "granted") {
        throw new DOMException("camera permission denied", "NotAllowedError");
      }
      const { content } = await scanner.scan({ windowed: true, formats: [scanner.Format.QRCode] });
      return content;
    },
    cancelScan: async () => (await import("@tauri-apps/plugin-barcode-scanner")).cancel(),
  }),
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
    const body = new Blob([note.poster.slice(), note.blob]);
    // Android's IPC bridge carries only JSON, so the bytes travel as base64 there.
    const payload = isAndroid ? { data: await base64Of(body) } : new Uint8Array(await body.arrayBuffer());
    return invoke<MediaSent>("send_video_note", payload, {
      headers: {
        "x-peer": peerId,
        "x-mime": note.mime,
        "x-duration-ms": String(note.durationMs),
        "x-poster-len": String(note.poster.length),
      },
    });
  },
  mediaUrl: (fileId) => Promise.resolve(convertFileSrc(fileId, "cypher-media")),
  imageUrl: (fileId) => Promise.resolve(convertFileSrc(`preview-${fileId}`, "cypher-media")),
  getConversations: () => invoke<ConversationEntry[]>("get_conversations"),
  renamePeer: (peerId, alias) => command("rename_peer", { peerId, alias }),
  deleteConversation: (peerId) => command("delete_conversation", { peerId }),
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

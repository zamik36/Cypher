import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { isPermissionGranted, requestPermission, sendNotification } from "@tauri-apps/plugin-notification";
import type { ConversationEntry, LinkInfo, Platform, TransferInfo, UiMessage } from "@cypher/ui/platform";

export const tauriPlatform: Platform = {
  kind: "desktop",
  hasIdentity: () => invoke<boolean>("has_identity"),
  createIdentity: (nickname, passphrase) => invoke<string>("create_identity", { nickname, passphrase }),
  unlockIdentity: (passphrase) => invoke<[string, string]>("unlock_identity", { passphrase }),
  importMnemonic: (mnemonic, nickname, passphrase) =>
    invoke<string>("import_mnemonic", { mnemonic, nickname, passphrase }),
  exportMnemonic: (passphrase) => invoke<string>("export_mnemonic", { passphrase }),
  connectToGateway: (addr, anonymous, bridges) => invoke<string>("connect_to_gateway", { addr, anonymous, bridges }),
  applyAnonymousSettings: (anonymous, bridges) => invoke<void>("apply_anonymous_settings", { anonymous, bridges }),
  createLink: () => invoke<LinkInfo>("create_link"),
  joinLink: (linkId) => invoke<string>("join_link", { linkId }),
  sendMessage: (peerId, text) => invoke<string>("send_message", { peerId, text }),
  markRead: (peerId, msgIds) => invoke<void>("mark_read", { peerId, msgIds }),
  pickAndSend: (peerId) => invoke<TransferInfo[]>("browse_and_send", { peerId }),
  acceptFile: (fileId) => invoke<void>("accept_file", { fileId }),
  cancelTransfer: (fileId) => invoke<void>("cancel_transfer", { fileId }),
  generateQr: (linkId) => invoke<string>("generate_qr", { linkId }),
  getConversations: () => invoke<ConversationEntry[]>("get_conversations"),
  getHistory: (peerId, limit, before) => invoke<UiMessage[]>("get_history", { peerId, limit, before }),
  clearChatHistory: () => invoke<void>("clear_chat_history"),
  on: (channel, cb) => listen(`cypher://${channel}`, (e) => cb(e.payload as never)),
  notifications: {
    supported: () => true,
    permission: async () => ((await isPermissionGranted()) ? "granted" : "default"),
    request: () => requestPermission(),
    send: async (title, body) => sendNotification({ title, body, group: "messages", autoCancel: true }),
  },
};

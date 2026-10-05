import type { ConversationEntry, LinkInfo, Platform, TransferInfo, UiMessage } from "@cypher/ui/platform";
import type { Method, Methods, WorkerMessage } from "./protocol";
import { WebVoiceRecorder } from "./voice";

const DOWNLOAD_RELEASE_MS = 60_000;
/** Presses shorter than this are accidental taps, as on desktop. */
const MIN_VOICE_MS = 500;

const worker = new Worker(new URL("./worker.ts", import.meta.url), { type: "module" });
const pending = new Map<number, { resolve: (v: unknown) => void; reject: (e: Error) => void }>();
const listeners = new Map<string, Set<(payload: unknown) => void>>();
let nextId = 1;
let voice: WebVoiceRecorder | null = null;
/** Decrypted notes stay as object URLs until history is cleared. */
const mediaUrls = new Map<string, Promise<string>>();

worker.onmessage = ({ data }: MessageEvent<WorkerMessage>) => {
  if ("id" in data) {
    const call = pending.get(data.id);
    pending.delete(data.id);
    if (data.ok) call?.resolve(data.result);
    else call?.reject(new Error(data.error));
  } else if ("event" in data) {
    listeners.get(data.event)?.forEach((cb) => cb(data.payload));
  } else {
    save(data.download.name, data.download.blob);
    setTimeout(() => void call("releaseDownload", data.download.fileId), DOWNLOAD_RELEASE_MS);
  }
};

function call<M extends Method>(method: M, ...args: Parameters<Methods[M]>): Promise<ReturnType<Methods[M]>> {
  const id = nextId++;
  return new Promise((resolve, reject) => {
    pending.set(id, { resolve: resolve as (v: unknown) => void, reject });
    worker.postMessage({ id, method, args });
  });
}

function save(name: string, blob: Blob) {
  const url = URL.createObjectURL(blob);
  const a = Object.assign(document.createElement("a"), { href: url, download: name });
  a.click();
  setTimeout(() => URL.revokeObjectURL(url), DOWNLOAD_RELEASE_MS);
}

function pickFiles(): Promise<File[]> {
  return new Promise((resolve) => {
    const input = Object.assign(document.createElement("input"), { type: "file", multiple: true });
    input.onchange = () => resolve(Array.from(input.files ?? []));
    input.oncancel = () => resolve([]);
    input.click();
  });
}

/** The web client always talks to the origin that served it (CSP `connect-src 'self'`). */
function endpoints(): [string, string] {
  const origin = `${location.protocol === "https:" ? "wss:" : "ws:"}//${location.host}`;
  return [`${origin}/ws`, `${origin}/relay`];
}

export const webPlatform: Platform = {
  kind: "web",
  capabilities: { tor: false, gatewayConfig: false, revealFile: false },
  hasIdentity: () => call("hasIdentity"),
  createIdentity: (nickname, passphrase) => call("createIdentity", nickname, passphrase),
  unlockIdentity: (passphrase) => call("unlockIdentity", passphrase),
  importMnemonic: (mnemonic, nickname, passphrase) => call("importMnemonic", mnemonic, nickname, passphrase),
  exportMnemonic: (passphrase) => call("exportMnemonic", passphrase),
  connectToGateway: (_addr, anonymous) => call("connect", ...endpoints(), anonymous),
  applyAnonymousSettings: (anonymous) => call("setAnonymity", anonymous),
  createLink: async (): Promise<LinkInfo> => ({ link_id: await call("createLink") }),
  joinLink: (linkId) => call("joinLink", linkId),
  sendMessage: async (peerId, text) => {
    const { msgId } = await call("command", { type: "send_text", peer: peerId, text });
    return msgId ?? "";
  },
  markRead: async (peerId, msgIds) => {
    await call("command", { type: "mark_read", peer: peerId, ids: msgIds });
  },
  pickAndSend: async (peerId): Promise<TransferInfo[]> => {
    const files = await pickFiles();
    if (files.length === 0) return [];
    const sent = await call("sendFiles", peerId, files);
    return sent.map((f) => ({ ...f, progress: 0, direction: "send", status: "active" }));
  },
  acceptFile: async (fileId) => {
    await call("command", { type: "accept_file", file_id: fileId });
  },
  cancelTransfer: async (fileId) => {
    await call("command", { type: "cancel_transfer", file_id: fileId });
  },
  generateQr: (linkId) => call("qr", linkId),
  safetyNumber: (peerId) => call("safetyNumber", peerId),
  reconnect: () => call("reconnect"),
  startVoice: async (onLevel) => {
    if (voice) throw new Error("already recording");
    voice = await WebVoiceRecorder.start(onLevel);
  },
  stopVoice: async (peerId) => {
    const recorder = voice;
    voice = null;
    if (!recorder) throw new Error("not recording");
    const clip = await recorder.stop();
    if (clip.durationMs < MIN_VOICE_MS) return null;
    return call("sendMedia", peerId, clip.blob, clip.mime, "voice", clip.durationMs, { frames: clip.frames });
  },
  cancelVoice: () => {
    voice?.cancel();
    voice = null;
    return Promise.resolve();
  },
  sendVideoNote: (peerId, note) =>
    call("sendMedia", peerId, note.blob, note.mime, "video_note", note.durationMs, { poster: note.poster }),
  mediaUrl: (fileId) => {
    let url = mediaUrls.get(fileId);
    if (!url) {
      url = call("mediaBlob", fileId).then((blob) => URL.createObjectURL(blob));
      url.catch(() => mediaUrls.delete(fileId));
      mediaUrls.set(fileId, url);
    }
    return url;
  },
  getConversations: () => call("conversations") as Promise<ConversationEntry[]>,
  renamePeer: async (peerId, alias) => {
    await call("command", { type: "rename_peer", peer: peerId, alias });
  },
  deleteConversation: (peerId) => call("forgetPeer", peerId),
  getHistory: (peerId, limit, before) => call("history", peerId, limit, before) as Promise<UiMessage[]>,
  clearChatHistory: async () => {
    await call("clearHistory");
    for (const url of mediaUrls.values())
      void url.then(
        (u) => URL.revokeObjectURL(u),
        () => undefined,
      );
    mediaUrls.clear();
  },
  on: (channel, cb) => {
    const set = listeners.get(channel) ?? new Set();
    // The worker forwards core events verbatim; `Events` is their contract.
    const handler = cb as (payload: unknown) => void;
    set.add(handler);
    listeners.set(channel, set);
    return Promise.resolve(() => {
      set.delete(handler);
    });
  },
  notifications: {
    supported: () => "Notification" in self,
    permission: () => Promise.resolve(Notification.permission),
    request: () => Notification.requestPermission(),
    send: async (title, body) => {
      // `serviceWorker` is missing outside secure contexts, despite the DOM typings.
      const reg = "serviceWorker" in navigator ? await navigator.serviceWorker.getRegistration() : undefined;
      if (reg) await reg.showNotification(title, { body, tag: "messages" });
      else new Notification(title, { body, tag: "messages" });
    },
  },
};

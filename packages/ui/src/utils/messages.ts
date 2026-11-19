import type { ChatMessage, UiFile, UiMessage } from "../platform";
import type { IconName } from "../components/Icon";
import { t } from "../i18n";

/** Who sent an outgoing message, in `ChatMessage.from`. */
export const ME = "me";

/** A stored or live message as the conversation keeps it. */
export function toChatMessage(peerId: string, m: UiMessage): ChatMessage {
  return {
    msg_id: m.msg_id,
    from: m.outgoing ? ME : peerId,
    text: m.text,
    timestamp: m.timestamp,
    status: m.status,
    file: m.file,
    reply_to: m.reply_to,
  };
}

export const isMine = (m: ChatMessage) => m.from === ME;

/** A voice or round video note: rendered as a player, not as text. */
export const noteOf = (m: ChatMessage): UiFile | undefined => (m.file && m.file.kind !== "file" ? m.file : undefined);

/** How a message reads in the chat list and in notifications. */
export function previewOf(m: ChatMessage): { icon?: IconName; text: string } {
  const tr = t();
  if (m.file?.kind === "voice") return { icon: "mic", text: tr.preview_voice };
  if (m.file?.kind === "video_note") return { icon: "video", text: tr.preview_video };
  if (m.file) return { icon: "file", text: m.file.name };
  return { text: m.text };
}

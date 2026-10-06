import { describe, expect, it } from "vitest";
import type { UiFile, UiMessage } from "../platform";
import { isMine, ME, noteOf, previewOf, toChatMessage } from "./messages";

const file = (kind: UiFile["kind"]): UiFile => ({
  file_id: "f",
  name: "a.pdf",
  size: 3,
  mime: "",
  kind,
  duration_ms: null,
});

describe("messages", () => {
  it("keep the author of stored messages", () => {
    const stored: UiMessage = {
      msg_id: "m",
      from: "peer",
      outgoing: true,
      text: "hi",
      timestamp: 5,
      status: "read",
      file: null,
      reply_to: "m0",
    };
    expect(toChatMessage("peer", stored)).toEqual({
      msg_id: "m",
      from: ME,
      text: "hi",
      timestamp: 5,
      status: "read",
      file: null,
      reply_to: "m0",
    });
    expect(isMine(toChatMessage("peer", stored))).toBe(true);
    expect(isMine(toChatMessage("peer", { ...stored, outgoing: false }))).toBe(false);
  });

  it("tell notes from files", () => {
    const base = { from: "p", text: "", timestamp: 0 };
    expect(noteOf({ ...base, file: file("voice") })?.kind).toBe("voice");
    expect(noteOf({ ...base, file: file("file") })).toBeUndefined();
    expect(noteOf(base)).toBeUndefined();
  });

  it("preview as an icon and a line", () => {
    const base = { from: "p", text: "hello", timestamp: 0 };
    expect(previewOf(base)).toEqual({ text: "hello" });
    expect(previewOf({ ...base, file: file("voice") }).icon).toBe("mic");
    expect(previewOf({ ...base, file: file("video_note") }).icon).toBe("video");
    expect(previewOf({ ...base, file: file("file") })).toEqual({ icon: "file", text: "a.pdf" });
  });
});

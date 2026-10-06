import { describe, expect, it } from "vitest";
import { clearAllDrafts, clearDraft, draftOf, setDraftText, setReplyTo } from "./drafts";

describe("drafts", () => {
  it("keep text and the message answered per chat", () => {
    expect(draftOf("a")).toEqual({ text: "", replyTo: null });
    setDraftText("a", "hello");
    setReplyTo("a", "m1");
    setDraftText("b", "other");
    expect(draftOf("a")).toEqual({ text: "hello", replyTo: "m1" });
    expect(draftOf("b")).toEqual({ text: "other", replyTo: null });
    clearDraft("a");
    expect(draftOf("a")).toEqual({ text: "", replyTo: null });
    clearAllDrafts();
    expect(draftOf("b")).toEqual({ text: "", replyTo: null });
  });
});

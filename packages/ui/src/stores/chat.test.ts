import { beforeEach, describe, expect, it } from "vitest";
import type { ChatMessage } from "../platform";
import { addMessage, clearAllMessages, getMessages, peerOf, setMessageStatus, setMessages } from "./chat";

const msg = (id: string | undefined, text = id ?? "local"): ChatMessage => ({
  ...(id && { msg_id: id }),
  from: "me",
  text,
  timestamp: 0,
  status: "pending",
});

describe("chat store", () => {
  beforeEach(clearAllMessages);

  it("appends messages per peer and ignores a repeated id", () => {
    addMessage("alice", msg("m1"));
    addMessage("alice", msg("m2"));
    addMessage("alice", msg("m1", "duplicate"));
    addMessage("bob", msg("m3"));
    expect(getMessages("alice").map((m) => m.text)).toEqual(["m1", "m2"]);
    expect(getMessages("bob")).toHaveLength(1);
    expect(peerOf("m3")).toBe("bob");
  });

  it("keeps messages without an id", () => {
    addMessage("alice", msg(undefined));
    addMessage("alice", msg(undefined));
    expect(getMessages("alice")).toHaveLength(2);
  });

  it("evicts the oldest beyond the in-memory cap and forgets their ids", () => {
    for (let i = 0; i < 1502; i++) addMessage("alice", msg(`m${i}`));
    const kept = getMessages("alice");
    expect(kept).toHaveLength(1500);
    expect(kept[0]?.msg_id).toBe("m2");
    expect(peerOf("m0")).toBeUndefined();
    expect(peerOf("m1501")).toBe("alice");
  });

  it("updates the status of a known message only", () => {
    addMessage("alice", msg("m1"));
    setMessageStatus("m1", "delivered");
    setMessageStatus("unknown", "read");
    expect(getMessages("alice")[0]?.status).toBe("delivered");
  });

  it("replaces a conversation with loaded history", () => {
    addMessage("alice", msg("old"));
    setMessages("alice", [msg("h1"), msg("h2")]);
    expect(getMessages("alice").map((m) => m.msg_id)).toEqual(["h1", "h2"]);
    setMessageStatus("h2", "read");
    expect(getMessages("alice")[1]?.status).toBe("read");
  });

  it("clears every conversation", () => {
    addMessage("alice", msg("m1"));
    clearAllMessages();
    expect(getMessages("alice")).toEqual([]);
    expect(peerOf("m1")).toBeUndefined();
  });
});

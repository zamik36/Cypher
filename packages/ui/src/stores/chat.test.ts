import { beforeEach, describe, expect, it } from "vitest";
import type { ChatMessage } from "../platform";
import {
  addMessage,
  clearAllMessages,
  findMessage,
  getMessages,
  hasOlder,
  historyLoaded,
  mergeHistory,
  peerOf,
  prependHistory,
  removeChat,
  removeMessage,
  setHasOlder,
  setMessageStatus,
  setMessages,
} from "./chat";

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
    expect(addMessage("alice", msg("m1", "duplicate"))).toBe(false);
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

  it("merges stored history before messages that arrived live", () => {
    addMessage("alice", msg("m3", "while you were away"));
    addMessage("alice", msg(undefined, "local only"));
    setMessageStatus("m3", "read");
    expect(historyLoaded("alice")).toBe(false);

    mergeHistory("alice", [msg("m1"), msg("m2"), { ...msg("m3", "while you were away"), status: "delivered" }]);
    expect(historyLoaded("alice")).toBe(true);
    expect(getMessages("alice").map((m) => m.text)).toEqual(["m1", "m2", "while you were away", "local only"]);
    expect(getMessages("alice")[2]?.status).toBe("read");
    expect(peerOf("m1")).toBe("alice");
  });

  it("puts older pages in front, skipping what is already shown", () => {
    addMessage("alice", msg("m3"));
    expect(prependHistory("alice", [msg("m1"), msg("m2"), msg("m3"), msg(undefined, "unsent")])).toBe(3);
    expect(getMessages("alice").map((m) => m.text)).toEqual(["m1", "m2", "unsent", "m3"]);
    expect(prependHistory("alice", [msg("m1")])).toBe(0);
    expect(getMessages("alice")).toHaveLength(4);
  });

  it("remembers whether older history exists", () => {
    expect(hasOlder("alice")).toBe(false);
    setHasOlder("alice", true);
    expect(hasOlder("alice")).toBe(true);
    clearAllMessages();
    expect(hasOlder("alice")).toBe(false);
  });

  it("drops one deleted message and finds the others", () => {
    addMessage("alice", msg("m1"));
    addMessage("alice", msg("m2"));
    removeMessage("alice", "m1");
    expect(getMessages("alice").map((m) => m.msg_id)).toEqual(["m2"]);
    expect(peerOf("m1")).toBeUndefined();
    expect(findMessage("alice", "m2")?.text).toBe("m2");
    expect(findMessage("bob", "m2")).toBeUndefined();
  });

  it("forgets one conversation and keeps the others", () => {
    addMessage("alice", msg("m1"));
    addMessage("alice", msg(undefined));
    addMessage("bob", msg("m2"));
    mergeHistory("alice", []);
    setHasOlder("alice", true);
    removeChat("alice");
    expect(getMessages("alice")).toEqual([]);
    expect(historyLoaded("alice")).toBe(false);
    expect(hasOlder("alice")).toBe(false);
    expect(peerOf("m1")).toBeUndefined();
    expect(peerOf("m2")).toBe("bob");
  });

  it("clears every conversation", () => {
    addMessage("alice", msg("m1"));
    mergeHistory("alice", []);
    clearAllMessages();
    expect(getMessages("alice")).toEqual([]);
    expect(historyLoaded("alice")).toBe(false);
    expect(peerOf("m1")).toBeUndefined();
  });
});

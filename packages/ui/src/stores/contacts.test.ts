import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ChatMessage } from "../platform";

const load = () => import("./contacts");

const msg = (from: string, timestamp: number, text = "hi"): ChatMessage => ({
  msg_id: `${from}${timestamp}`,
  from,
  text,
  timestamp,
});

describe("contacts", () => {
  beforeEach(() => vi.resetModules());

  it("load from the conversation list, newest first", async () => {
    const { loadConversations, sortedContacts, displayName } = await load();
    loadConversations([
      {
        peer_id: "aaaaaa11",
        alias: null,
        name: "Ann",
        request: false,
        blocked: false,
        last_message_at: 10,
        last: null,
        unread: 0,
      },
      {
        peer_id: "bbbbbb22",
        alias: "Bob",
        name: "Robert",
        request: false,
        blocked: false,
        last_message_at: 20,
        last: {
          msg_id: "m",
          from: "bbbbbb22",
          outgoing: false,
          text: "yo",
          timestamp: 20,
          status: "delivered",
          file: null,
          reply_to: null,
        },
        unread: 2,
      },
    ]);
    expect(sortedContacts().map((c) => c.peerId)).toEqual(["bbbbbb22", "aaaaaa11"]);
    expect(sortedContacts()[0]?.last?.from).toBe("bbbbbb22");
    expect(displayName("bbbbbb22")).toBe("Bob");
    expect(displayName("aaaaaa11")).toBe("Ann");
    expect(sortedContacts("bo").map((c) => c.peerId)).toEqual(["bbbbbb22"]);
  });

  it("count unread only while the chat is not in view", async () => {
    const { noteMessage, contacts, markConversationRead, totalUnread } = await load();
    noteMessage("p1", msg("p1", 1), false);
    noteMessage("p1", msg("p1", 2), false);
    noteMessage("p1", msg("me", 3), false);
    noteMessage("p1", msg("p1", 4), true);
    expect(contacts["p1"]?.unread).toBe(2);
    expect(contacts["p1"]?.last?.timestamp).toBe(4);
    expect(totalUnread()).toBe(2);
    markConversationRead("p1");
    expect(contacts["p1"]?.unread).toBe(0);
    noteMessage("p1", msg("p1", 0), false);
    expect(contacts["p1"]?.last?.timestamp).toBe(4);
  });

  it("keep the last message's tick in step", async () => {
    const { noteMessage, setLastStatus, contacts } = await load();
    noteMessage("p3", { ...msg("me", 1), status: "sent" }, false);
    setLastStatus("p3", "other", "read");
    expect(contacts["p3"]?.last?.status).toBe("sent");
    setLastStatus("p3", "me1", "read");
    expect(contacts["p3"]?.last?.status).toBe("read");
  });

  it("show another last message after a deletion", async () => {
    const { noteMessage, setLastMessage, contacts } = await load();
    noteMessage("p4", msg("p4", 5), false);
    setLastMessage("p4", msg("p4", 2));
    expect(contacts["p4"]?.last?.timestamp).toBe(2);
    expect(contacts["p4"]?.lastAt).toBe(2);
    setLastMessage("p4", null);
    expect(contacts["p4"]?.last).toBeNull();
    setLastMessage("nobody", null);
    expect(contacts["nobody"]).toBeUndefined();
  });

  it("keep requests and blocked contacts out of the chats", async () => {
    const {
      ensureContact,
      noteMessage,
      setContactRequest,
      setContactBlocked,
      sortedContacts,
      requestContacts,
      blockedContacts,
      totalUnread,
    } = await load();
    ensureContact("friend");
    noteMessage("stranger", msg("stranger", 9), false);
    setContactRequest("stranger", true);
    setContactBlocked("pest", true);
    expect(sortedContacts().map((c) => c.peerId)).toEqual(["friend"]);
    expect(requestContacts().map((c) => c.peerId)).toEqual(["stranger"]);
    expect(blockedContacts().map((c) => c.peerId)).toEqual(["pest"]);
    expect(totalUnread()).toBe(0);
    setContactRequest("stranger", false);
    expect(totalUnread()).toBe(1);
  });

  it("hold the profile's own nickname", async () => {
    const { nickname, setNickname } = await import("./profile");
    expect(nickname()).toBeNull();
    setNickname("carol");
    expect(nickname()).toBe("carol");
  });

  it("track names, presence and removal", async () => {
    const {
      avatarName,
      ensureContact,
      setAlias,
      setContactName,
      setContactOnline,
      setAllOffline,
      removeContact,
      contacts,
      displayName,
    } = await load();
    ensureContact("p2", true);
    expect(contacts["p2"]?.online).toBe(true);
    ensureContact("p2", true);
    expect(avatarName("p2")).toBe("p2");
    setContactName("p2", " Anya ");
    expect(displayName("p2")).toBe("Anya");
    setAlias("p2", "  Anna ");
    expect(displayName("p2")).toBe("Anna");
    expect(avatarName("p2")).toBe("Anna");
    setContactName("p2", null);
    expect(contacts["p2"]?.name).toBeNull();
    setAlias("p2", "   ");
    expect(contacts["p2"]?.alias).toBeNull();
    setContactOnline("p2", false);
    setContactOnline("p2", true);
    setAllOffline();
    expect(contacts["p2"]?.online).toBe(false);
    removeContact("p2");
    expect(contacts["p2"]).toBeUndefined();
  });
});

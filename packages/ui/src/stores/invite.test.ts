import { beforeEach, describe, expect, it, vi } from "vitest";

describe("incoming invites", () => {
  beforeEach(() => vi.resetModules());

  it("wait for the app to unlock, then open New chat with the code", async () => {
    const nav = await import("./nav");
    const { receiveInvite, setInvitesReady } = await import("./invite");
    const code = `${"a".repeat(26)}-${"b".repeat(26)}`;
    receiveInvite("https://cyphermessanger.tech/");
    receiveInvite(`https://cyphermessanger.tech/join#${code}`);
    expect(nav.top()).toEqual({ name: "chats" });
    setInvitesReady(true);
    expect(nav.top()).toEqual({ name: "new-chat", tab: "join", code });
    nav.reset();
    receiveInvite(`cypher://join/${code}`);
    expect(nav.top()).toEqual({ name: "new-chat", tab: "join", code });
    setInvitesReady(false);
    nav.reset();
    setInvitesReady(true);
    expect(nav.top()).toEqual({ name: "chats" });
  });
});

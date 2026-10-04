import { beforeEach, describe, expect, it, vi } from "vitest";

const load = () => import("./connection");

describe("gateway address", () => {
  beforeEach(() => {
    vi.resetModules();
    localStorage.clear();
  });

  it.each([
    ["", "cyphermessanger.tech:9100"],
    ["   ", "cyphermessanger.tech:9100"],
    ["example.org", "example.org:9100"],
    ["example.org:443", "example.org:443"],
    ["wss://example.org/path?q#f", "example.org:9100"],
    ["https://example.org:8443/", "example.org:8443"],
    ["[::1]", "[::1]:9100"],
    ["[::1]:7000", "[::1]:7000"],
    ["tcp:///nothing", "cyphermessanger.tech:9100"],
  ])("normalizes %j to %j", async (raw, expected) => {
    const { normalizeGatewayAddr } = await load();
    expect(normalizeGatewayAddr(raw)).toBe(expected);
  });

  it("restores and persists the normalized address", async () => {
    localStorage.setItem("cypher-gateway", "saved.example");
    const { connection, setGatewayAddr } = await load();
    expect(connection.gatewayAddr).toBe("saved.example:9100");
    expect(setGatewayAddr(" other.example:1 ")).toBe("other.example:1");
    expect(localStorage.getItem("cypher-gateway")).toBe("other.example:1");
  });
});

describe("peers", () => {
  beforeEach(() => vi.resetModules());

  const peer = (peerId: string, online = true) => ({
    peerId,
    roomCode: "direct",
    role: "guest" as const,
    displayName: peerId.slice(0, 6),
    online,
  });

  it("adds each peer once, selects the first and updates presence", async () => {
    const { addPeer, connection, markAllPeersOffline, setActivePeer, setPeerOnline, shortName } = await load();
    addPeer(peer("aaaaaaaa"));
    addPeer(peer("bbbbbbbb"));
    expect(connection.activePeerId).toBe("aaaaaaaa");

    addPeer({ ...peer("aaaaaaaa", false), displayName: "Alice", roomCode: "other" });
    expect(connection.peers).toHaveLength(2);
    expect(connection.peers[0]).toMatchObject({ displayName: "Alice", online: false, roomCode: "direct" });

    setPeerOnline("aaaaaaaa", true);
    expect(connection.peers[0]?.online).toBe(true);
    markAllPeersOffline();
    expect(connection.peers.every((p) => !p.online)).toBe(true);

    setActivePeer("bbbbbbbb");
    expect(connection.activePeerId).toBe("bbbbbbbb");
    expect(shortName("0123456789")).toBe("012345");
  });
});

describe("connection state", () => {
  beforeEach(() => vi.resetModules());

  it("is online only once the server accepted us", async () => {
    const { nextLink, linkEvent, isOnline } = await load();
    linkEvent("start");
    expect(isOnline()).toBe(false);
    linkEvent("connected");
    expect(isOnline()).toBe(true);
    expect(nextLink("idle", "start")).toBe("connecting");
    expect(nextLink("connecting", "disconnected")).toBe("connecting");
    expect(nextLink("connecting", "connected")).toBe("online");
    expect(nextLink("online", "disconnected")).toBe("reconnecting");
    expect(nextLink("reconnecting", "connected")).toBe("online");
    expect(nextLink("connecting", "failed")).toBe("failed");
    expect(nextLink("failed", "start")).toBe("connecting");
  });

  it("stays stopped until the user acts", async () => {
    const { nextLink } = await load();
    expect(nextLink("online", "superseded")).toBe("superseded");
    expect(nextLink("superseded", "disconnected")).toBe("superseded");
    expect(nextLink("superseded", "start")).toBe("connecting");
    expect(nextLink("online", "update_required")).toBe("update_required");
    expect(nextLink("update_required", "disconnected")).toBe("update_required");
    expect(nextLink("update_required", "start")).toBe("update_required");
  });

  it("reports why it could not start, and forgets it on the next attempt", async () => {
    const { connectGateway, connection } = await load();
    await connectGateway(() => Promise.reject(new Error("identity is locked")));
    expect(connection.link).toBe("failed");
    expect(connection.linkError).toBe("identity is locked");

    await connectGateway(() => Promise.resolve());
    expect(connection.link).toBe("connecting");
    expect(connection.linkError).toBeNull();
  });
});

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

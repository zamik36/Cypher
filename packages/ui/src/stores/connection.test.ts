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
    expect(nextLink("online", "unlinked")).toBe("unlinked");
    expect(nextLink("unlinked", "disconnected")).toBe("unlinked");
    expect(nextLink("unlinked", "start")).toBe("unlinked");
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

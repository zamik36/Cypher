import { beforeEach, describe, expect, it, vi } from "vitest";

const KEY = "cypher-anonymous-settings";
const load = () => import("./anonymity");

describe("anonymous settings", () => {
  beforeEach(() => {
    vi.resetModules();
    localStorage.clear();
  });

  it("defaults to enabled with no bridges", async () => {
    const { anonymousSettings } = await load();
    expect({ ...anonymousSettings }).toEqual({ enabled: true, bridgeLines: [] });
  });

  it("restores saved settings, dropping malformed bridge lines", async () => {
    localStorage.setItem(KEY, JSON.stringify({ enabled: false, bridgeLines: ["a", 1, "b"] }));
    const { anonymousSettings } = await load();
    expect(anonymousSettings.enabled).toBe(false);
    expect([...anonymousSettings.bridgeLines]).toEqual(["a", "b"]);
  });

  it.each([["not json"], [JSON.stringify({ bridgeLines: "x" })]])("falls back on %j", async (raw) => {
    localStorage.setItem(KEY, raw);
    const { anonymousSettings } = await load();
    expect({ ...anonymousSettings, bridgeLines: [...anonymousSettings.bridgeLines] }).toEqual({
      enabled: true,
      bridgeLines: [],
    });
  });

  it("persists what it is given", async () => {
    const { anonymousSettings, setAnonymousSettings } = await load();
    setAnonymousSettings({ enabled: false, bridgeLines: ["x"] });
    expect(anonymousSettings.enabled).toBe(false);
    expect(JSON.parse(localStorage.getItem(KEY) ?? "null")).toEqual({ enabled: false, bridgeLines: ["x"] });
  });
});

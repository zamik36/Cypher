import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Platform } from "../platform";

function platform(subscribed: boolean) {
  const push = {
    enable: vi.fn<() => Promise<void>>().mockResolvedValue(undefined),
    subscribe: vi.fn<(key: string) => Promise<boolean>>().mockResolvedValue(subscribed),
    disable: vi.fn<() => Promise<void>>().mockResolvedValue(undefined),
  };
  const platform = { push, notifications: { supported: () => true } } as unknown as Platform;
  return { platform, spies: push };
}

async function load(subscribed = true, notifications = true) {
  vi.resetModules();
  localStorage.setItem("cypher-notifications-enabled", String(notifications));
  const { registerPlatform } = await import("../platform");
  const { platform: registered, spies } = platform(subscribed);
  registerPlatform(registered);
  return { push: await import("./push"), p: spies };
}

describe("push", () => {
  beforeEach(() => localStorage.clear());

  it("asks for the key on start, subscribes with it, and reports the server", async () => {
    const { push, p } = await load();
    expect(push.pushWanted()).toBe(true);
    await push.startPush();
    expect(p.enable).toHaveBeenCalledOnce();
    expect(push.pushState()).toBe("pending");
    await push.onPushKey("04ab");
    expect(p.subscribe).toHaveBeenCalledWith("04ab");
    push.onPushState("registered");
    expect(push.pushState()).toBe("registered");
  });

  it("is unavailable when the browser refuses the subscription", async () => {
    const { push } = await load(false);
    await push.startPush();
    await push.onPushKey("04ab");
    expect(push.pushState()).toBe("unavailable");
  });

  it("stays off without notifications, and turning it off reaches the server", async () => {
    const quiet = await load(true, false);
    await quiet.push.startPush();
    await quiet.push.onPushKey("04ab");
    expect(quiet.p.enable).not.toHaveBeenCalled();
    expect(quiet.p.subscribe).not.toHaveBeenCalled();

    const { push, p } = await load();
    await push.setPushWanted(false);
    expect(p.disable).toHaveBeenCalledOnce();
    expect(push.pushState()).toBe("off");
    expect(localStorage.getItem("cypher-push")).toBe("0");
    push.onPushState("registered");
    expect(push.pushState()).toBe("off");
    await push.setPushWanted(true);
    expect(p.enable).toHaveBeenCalledOnce();
  });
});

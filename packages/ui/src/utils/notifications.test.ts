import { beforeEach, describe, expect, it, vi } from "vitest";
import { fakeNotifications } from "../test/platform";
import {
  notificationPermissionState,
  notificationsEnabled,
  notifyMessage,
  previewEnabled,
  requestNotificationAccess,
  setNotificationsEnabled,
  setPreviewEnabled,
} from "./notifications";

const inBackground = () => vi.spyOn(document, "hasFocus").mockReturnValue(false);

describe("notifications", () => {
  beforeEach(() => localStorage.clear());

  it("are off until the user opts in", () => {
    fakeNotifications();
    expect(notificationsEnabled()).toBe(false);
    setNotificationsEnabled(true);
    setPreviewEnabled(true);
    expect(notificationsEnabled()).toBe(true);
    expect(previewEnabled()).toBe(true);
  });

  it("report denied where the platform has none", async () => {
    const n = fakeNotifications("granted", false);
    setNotificationsEnabled(true);
    expect(notificationsEnabled()).toBe(false);
    expect(previewEnabled()).toBe(false);
    await expect(notificationPermissionState()).resolves.toBe("denied");
    await expect(requestNotificationAccess()).resolves.toBe("denied");
    expect(n.request).not.toHaveBeenCalled();
  });

  it("ask the platform for permission when supported", async () => {
    const n = fakeNotifications("default");
    await expect(notificationPermissionState()).resolves.toBe("default");
    await expect(requestNotificationAccess()).resolves.toBe("default");
    expect(n.request).toHaveBeenCalledOnce();
  });

  it("hide the text unless previews are on, and trim long previews", async () => {
    const n = fakeNotifications();
    inBackground();
    setNotificationsEnabled(true);
    await notifyMessage("alice", "secret");
    expect(n.send).toHaveBeenLastCalledWith("Cypher", "New encrypted message");

    setPreviewEnabled(true);
    await notifyMessage("alice", "hi");
    expect(n.send).toHaveBeenLastCalledWith("Cypher", "hi");
    await notifyMessage("alice", "x".repeat(150));
    expect(n.send).toHaveBeenLastCalledWith("Cypher", `${"x".repeat(100)}...`);
  });

  it("stay quiet when disabled, not permitted or the app is in front", async () => {
    let n = fakeNotifications();
    inBackground();
    await notifyMessage("alice", "hi");
    expect(n.send).not.toHaveBeenCalled();

    setNotificationsEnabled(true);
    n = fakeNotifications("denied");
    await notifyMessage("alice", "hi");
    expect(n.send).not.toHaveBeenCalled();

    n = fakeNotifications();
    vi.spyOn(document, "hasFocus").mockReturnValue(true);
    await notifyMessage("alice", "hi");
    expect(n.send).not.toHaveBeenCalled();
  });
});

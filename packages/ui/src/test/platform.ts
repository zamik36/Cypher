import { vi } from "vitest";
import { registerPlatform, type Notifications, type Platform } from "../platform";

/** Registers a platform whose notifications are spies; other methods are absent. */
export function fakeNotifications(permission: NotificationPermission = "granted", supported = true) {
  const notifications = {
    supported: vi.fn(() => supported),
    permission: vi.fn(() => Promise.resolve(permission)),
    request: vi.fn(() => Promise.resolve(permission)),
    send: vi.fn(() => Promise.resolve()),
  } satisfies Notifications;
  registerPlatform({ notifications } as unknown as Platform);
  return notifications;
}

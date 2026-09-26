import { api } from "../platform";

const ENABLED_KEY = "cypher-notifications-enabled";
const PREVIEW_KEY = "cypher-notifications-preview";

export function notificationsSupported(): boolean {
  return api.notifications.supported();
}

export function notificationsEnabled(): boolean {
  return notificationsSupported() && localStorage.getItem(ENABLED_KEY) === "true";
}

export function previewEnabled(): boolean {
  return notificationsSupported() && localStorage.getItem(PREVIEW_KEY) === "true";
}

export function setNotificationsEnabled(enabled: boolean): void {
  localStorage.setItem(ENABLED_KEY, String(enabled));
}

export function setPreviewEnabled(enabled: boolean): void {
  localStorage.setItem(PREVIEW_KEY, String(enabled));
}

export function notificationPermissionState(): Promise<NotificationPermission> {
  return notificationsSupported() ? api.notifications.permission() : Promise.resolve("denied");
}

export function requestNotificationAccess(): Promise<NotificationPermission> {
  return notificationsSupported() ? api.notifications.request() : Promise.resolve("denied");
}

/** Message previews are shown only when the user opted in. */
export async function notifyMessage(_senderName: string, text: string): Promise<void> {
  if (!notificationsEnabled()) return;
  if ((await api.notifications.permission()) !== "granted") return;
  if (document.visibilityState === "visible" && document.hasFocus()) return;
  const body = previewEnabled()
    ? (text.length > 100 ? `${text.slice(0, 100)}...` : text)
    : "New encrypted message";
  await api.notifications.send("Cypher", body);
}

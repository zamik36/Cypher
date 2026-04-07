import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from "@tauri-apps/plugin-notification";
import { isTauri } from "@tauri-apps/api/core";

const ENABLED_KEY = "cypher-notifications-enabled";
const PREVIEW_KEY = "cypher-notifications-preview";

export function notificationsSupported(): boolean {
  return typeof window !== "undefined" && isTauri();
}

export function notificationsEnabled(): boolean {
  if (!notificationsSupported()) return false;
  return localStorage.getItem(ENABLED_KEY) === "true";
}

export function previewEnabled(): boolean {
  if (!notificationsSupported()) return false;
  return localStorage.getItem(PREVIEW_KEY) === "true";
}

export function setNotificationsEnabled(enabled: boolean): void {
  localStorage.setItem(ENABLED_KEY, String(enabled));
}

export function setPreviewEnabled(enabled: boolean): void {
  localStorage.setItem(PREVIEW_KEY, String(enabled));
}

export async function notificationPermissionState(): Promise<NotificationPermission> {
  if (!notificationsSupported()) {
    return "denied";
  }

  if (await isPermissionGranted()) {
    return "granted";
  }

  if (typeof Notification !== "undefined") {
    return Notification.permission;
  }

  return "default";
}

export async function requestNotificationAccess(): Promise<NotificationPermission> {
  if (!notificationsSupported()) {
    return "denied";
  }

  return requestPermission();
}

export async function notifyMessage(_senderName: string, text: string): Promise<void> {
  if (!notificationsSupported()) return;
  if (!notificationsEnabled()) return;
  if (!(await isPermissionGranted())) return;
  if (document.visibilityState === "visible" && document.hasFocus()) return;

  const body = previewEnabled()
    ? (text.length > 100 ? `${text.slice(0, 100)}...` : text)
    : "New encrypted message";

  sendNotification({
    title: "Cypher",
    body,
    group: "messages",
    autoCancel: true,
  });
}

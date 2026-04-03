/**
 * Browser notification helpers for incoming messages.
 *
 * Uses Service Worker notifications (showNotification) which work
 * reliably when the PWA is in the background or minimized.
 *
 * Privacy: message content is NEVER included in notifications by default
 * because OS notification systems log, sync, and display content on lock
 * screens — violating E2E guarantees. Users can opt-in to previews.
 */

const ENABLED_KEY = "cypher-notifications-enabled";
const PREVIEW_KEY = "cypher-notifications-preview";

export function notificationsSupported(): boolean {
  return "Notification" in window && "serviceWorker" in navigator;
}

/** Notifications are opt-in (disabled by default for privacy). */
export function notificationsEnabled(): boolean {
  if (!notificationsSupported()) return false;
  return localStorage.getItem(ENABLED_KEY) === "true";
}

/** Whether to show message preview text in notifications. */
export function previewEnabled(): boolean {
  return localStorage.getItem(PREVIEW_KEY) === "true";
}

export function setNotificationsEnabled(enabled: boolean): void {
  localStorage.setItem(ENABLED_KEY, String(enabled));
}

export function setPreviewEnabled(enabled: boolean): void {
  localStorage.setItem(PREVIEW_KEY, String(enabled));
}

export async function requestNotificationPermission(): Promise<NotificationPermission> {
  if (!notificationsSupported()) return "denied";
  return Notification.requestPermission();
}

/**
 * Show a system notification for an incoming message.
 * Uses the Service Worker so it works when the tab is in the background.
 *
 * By default only shows "New message" without content to preserve E2E privacy.
 */
export async function notifyMessage(_senderName: string, text: string): Promise<void> {
  if (!notificationsEnabled()) return;
  if (Notification.permission !== "granted") return;
  // Don't notify if the page is visible and focused.
  if (document.visibilityState === "visible" && document.hasFocus()) return;

  const title = "Cypher";
  const body = previewEnabled()
    ? (text.length > 100 ? text.slice(0, 100) + "..." : text)
    : "New encrypted message";

  try {
    const reg = await navigator.serviceWorker.ready;
    await reg.showNotification(title, {
      body,
      icon: "/icons/icon-192.png",
      tag: "cypher-msg",
      data: { type: "message" },
    } as NotificationOptions);
  } catch {
    // Fallback to basic Notification API.
    const n = new Notification(title, {
      body,
      icon: "/icons/icon-192.png",
      tag: "cypher-msg",
    });
    n.onclick = () => { window.focus(); n.close(); };
    setTimeout(() => n.close(), 8000);
  }
}

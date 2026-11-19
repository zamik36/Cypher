/** What this device is called until the user names it. */

const BROWSERS: [string, string][] = [
  ["Edg/", "Edge"],
  ["OPR/", "Opera"],
  ["Firefox/", "Firefox"],
  ["Chrome/", "Chrome"],
  ["Safari/", "Safari"],
];
const SYSTEMS: [string, string][] = [
  ["Android", "Android"],
  ["iPhone", "iPhone"],
  ["iPad", "iPad"],
  ["Windows", "Windows"],
  ["Mac OS", "Mac"],
  ["Linux", "Linux"],
];

/** "Browser on System", as far as the user agent tells. */
export function browserName(ua: string = navigator.userAgent): string {
  const browser = BROWSERS.find(([mark]) => ua.includes(mark))?.[1] ?? "Browser";
  const system = SYSTEMS.find(([mark]) => ua.includes(mark))?.[1];
  return system ? `${browser} on ${system}` : browser;
}

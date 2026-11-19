import type { Locale } from "../i18n";

const DAY_MS = 86_400_000;

const tag = (locale: Locale) => (locale === "ru" ? "ru-RU" : "en-US");

/** A byte count for people: "820 B", "1.4 MB". */
export function formatBytes(bytes: number, locale: Locale): string {
  const [bytesUnit, ...larger] = locale === "ru" ? ["Б", "КБ", "МБ", "ГБ"] : ["B", "KB", "MB", "GB"];
  let value = bytes;
  let unit = bytesUnit;
  for (const next of larger) {
    if (value < 1024) break;
    value /= 1024;
    unit = next;
  }
  const digits = unit === bytesUnit || value >= 10 ? 0 : 1;
  const number = new Intl.NumberFormat(tag(locale), { maximumFractionDigits: digits }).format(value);
  return `${number} ${unit}`;
}

/** Time of day, "14:05". */
export function formatTime(timestamp: number, locale: Locale): string {
  return new Intl.DateTimeFormat(tag(locale), { hour: "2-digit", minute: "2-digit", hour12: false }).format(timestamp);
}

/** Midnight before `timestamp`, local time. */
function startOfDay(timestamp: number): number {
  const d = new Date(timestamp);
  d.setHours(0, 0, 0, 0);
  return d.getTime();
}

/** Whole calendar days from `then` to `now`, local time. */
function daysBetween(then: number, now: number): number {
  return Math.round((startOfDay(now) - startOfDay(then)) / DAY_MS);
}

export function sameDay(a: number, b: number): boolean {
  return startOfDay(a) === startOfDay(b);
}

/** The time a chat list shows: today the time, this week the weekday, then the date. */
export function formatListTime(timestamp: number, now: number, locale: Locale): string {
  const days = daysBetween(timestamp, now);
  if (days <= 0) return formatTime(timestamp, locale);
  if (days < 7) return new Intl.DateTimeFormat(tag(locale), { weekday: "short" }).format(timestamp);
  const sameYear = new Date(timestamp).getFullYear() === new Date(now).getFullYear();
  return new Intl.DateTimeFormat(tag(locale), {
    day: "numeric",
    month: "short",
    ...(sameYear ? {} : { year: "2-digit" }),
  }).format(timestamp);
}

/** The heading of a day in a conversation: "Today", "Yesterday", "3 October". */
export function dayLabel(
  timestamp: number,
  now: number,
  locale: Locale,
  words: { today: string; yesterday: string },
): string {
  const days = daysBetween(timestamp, now);
  if (days <= 0) return words.today;
  if (days === 1) return words.yesterday;
  const sameYear = new Date(timestamp).getFullYear() === new Date(now).getFullYear();
  return new Intl.DateTimeFormat(tag(locale), {
    day: "numeric",
    month: "long",
    ...(sameYear ? {} : { year: "numeric" }),
  }).format(timestamp);
}

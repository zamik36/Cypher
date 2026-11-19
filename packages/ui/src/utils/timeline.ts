import { sameDay } from "./format";

/** Messages from one side closer together than this read as one burst. */
export const GROUP_GAP_MS = 5 * 60_000;

export type TimelineItem<M> =
  | { kind: "day"; timestamp: number }
  | {
      kind: "message";
      message: M;
      /** First of its group: gets the top margin and the full corner. */
      first: boolean;
      /** Last of its group: gets the tail corner. */
      last: boolean;
    };

/**
 * Messages (oldest first) with a heading before each new day, and each
 * message marked as the first or last of its group: consecutive messages of
 * one author (`outgoing` tells them apart) on one day, at most
 * [`GROUP_GAP_MS`] apart.
 */
export function buildTimeline<M extends { timestamp: number }>(
  messages: readonly M[],
  outgoing: (message: M) => boolean,
): TimelineItem<M>[] {
  const items: TimelineItem<M>[] = [];
  const joins = (a: M | undefined, b: M | undefined) =>
    a !== undefined &&
    b !== undefined &&
    outgoing(a) === outgoing(b) &&
    sameDay(a.timestamp, b.timestamp) &&
    Math.abs(b.timestamp - a.timestamp) <= GROUP_GAP_MS;
  messages.forEach((message, i) => {
    const previous = messages[i - 1];
    if (!previous || !sameDay(previous.timestamp, message.timestamp)) {
      items.push({ kind: "day", timestamp: message.timestamp });
    }
    items.push({
      kind: "message",
      message,
      first: !joins(previous, message),
      last: !joins(message, messages[i + 1]),
    });
  });
  return items;
}

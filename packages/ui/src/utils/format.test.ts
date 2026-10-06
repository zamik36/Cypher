import { describe, expect, it } from "vitest";
import { avatarColour, initials, AVATAR_COLOURS } from "./avatar";
import { dayLabel, formatBytes, formatListTime, formatTime, sameDay } from "./format";
import { findInvite, inviteUrl } from "./invite";
import { buildTimeline, GROUP_GAP_MS } from "./timeline";

const at = (y: number, m: number, d: number, h = 12, min = 0) => new Date(y, m - 1, d, h, min).getTime();
const NOW = at(2026, 10, 5, 15, 30);
const words = { today: "Today", yesterday: "Yesterday" };

describe("format", () => {
  it("sizes", () => {
    expect(formatBytes(820, "en")).toBe("820 B");
    expect(formatBytes(1536, "en")).toBe("1.5 KB");
    expect(formatBytes(15 * 1024 * 1024, "en")).toBe("15 MB");
    expect(formatBytes(3 * 1024 ** 4, "en")).toBe("3,072 GB");
    expect(formatBytes(1536, "ru")).toBe("1,5 КБ");
  });

  it("times and days", () => {
    expect(formatTime(at(2026, 10, 5, 9, 5), "en")).toBe("09:05");
    expect(sameDay(at(2026, 10, 5, 0, 1), at(2026, 10, 5, 23, 59))).toBe(true);
    expect(sameDay(at(2026, 10, 4, 23, 59), at(2026, 10, 5, 0, 1))).toBe(false);

    expect(formatListTime(at(2026, 10, 5, 8, 0), NOW, "en")).toBe("08:00");
    expect(formatListTime(at(2026, 10, 2), NOW, "en")).toBe("Fri");
    expect(formatListTime(at(2026, 8, 20), NOW, "en")).toBe("Aug 20");
    expect(formatListTime(at(2025, 8, 20), NOW, "en")).toBe("Aug 20, 25");

    expect(dayLabel(at(2026, 10, 5, 1), NOW, "en", words)).toBe("Today");
    expect(dayLabel(at(2026, 10, 4, 23), NOW, "en", words)).toBe("Yesterday");
    expect(dayLabel(at(2026, 10, 1), NOW, "en", words)).toBe("October 1");
    expect(dayLabel(at(2026, 10, 1), NOW, "ru", words)).toBe("1 октября");
    expect(dayLabel(at(2024, 3, 8), NOW, "en", words)).toBe("March 8, 2024");
  });
});

describe("avatars", () => {
  it("take the first letters of the first and last word", () => {
    expect(initials("Anna Lee")).toBe("AL");
    expect(initials("  мария  ")).toBe("М");
    expect(initials("Jean Paul Sartre")).toBe("JS");
    expect(initials("")).toBe("");
  });

  it("keep a stable colour per contact", () => {
    const colour = avatarColour("ab12cd");
    expect(avatarColour("ab12cd")).toBe(colour);
    expect(colour).toBeGreaterThanOrEqual(0);
    expect(colour).toBeLessThan(AVATAR_COLOURS);
  });
});

describe("invites", () => {
  const code = `${"a".repeat(26)}-${"b2".repeat(13)}`;

  it("are found in whatever was pasted around them", () => {
    expect(findInvite(code)).toBe(code);
    expect(findInvite(`  Join me in Cypher: ${code.toUpperCase()} !`)).toBe(code);
    expect(findInvite("not-a-code")).toBeNull();
    expect(inviteUrl(code)).toMatch(/^https:\/\/.+\/join#/);
    expect(findInvite(inviteUrl(code))).toBe(code);
    expect(findInvite(`${"a".repeat(26)}-${"b".repeat(25)}`)).toBeNull();
  });
});

describe("timeline", () => {
  const msg = (timestamp: number, outgoing: boolean) => ({ timestamp, outgoing });

  it("heads each day and groups bursts from one side", () => {
    const yesterday = at(2026, 10, 4, 22, 0);
    const today = at(2026, 10, 5, 9, 0);
    const items = buildTimeline(
      [
        msg(yesterday, false),
        msg(today, false),
        msg(today + 60_000, false),
        msg(today + 120_000, true),
        msg(today + 120_000 + GROUP_GAP_MS + 1, true),
      ],
      (m) => m.outgoing,
    );
    expect(items.map((i) => (i.kind === "day" ? "day" : `${i.first ? "F" : "-"}${i.last ? "L" : "-"}`))).toEqual([
      "day",
      "FL",
      "day",
      "F-",
      "-L",
      "FL",
      "FL",
    ]);
  });

  it("is empty without messages", () => {
    expect(buildTimeline([], () => false)).toEqual([]);
  });
});

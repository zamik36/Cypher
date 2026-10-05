/** Number of avatar colours in the tokens (`--c-avatar-0` … `--c-avatar-7`). */
export const AVATAR_COLOURS = 8;

/** Up to two letters standing for `name`: "Anna Lee" → "AL", "bob" → "B". */
export function initials(name: string): string {
  const words = name.trim().split(/\s+/).filter(Boolean);
  const ends = words.length > 1 ? words.filter((_, i) => i === 0 || i === words.length - 1) : words;
  return ends
    .map((word) => Array.from(word)[0])
    .join("")
    .toUpperCase();
}

/** A stable colour for a contact, from its id, so it never changes with its name. */
export function avatarColour(peerId: string): number {
  let hash = 0;
  for (let i = 0; i < peerId.length; i++) hash = (hash * 31 + peerId.charCodeAt(i)) >>> 0;
  return hash % AVATAR_COLOURS;
}

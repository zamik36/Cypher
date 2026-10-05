/**
 * An invite code is `<link id>-<host fingerprint>`, both 26 lowercase base32
 * characters (see `ShareLink` in the core). People paste it with whatever
 * came around it, so it is found inside the text.
 */
const INVITE = /[a-z2-7]{26}-[a-z2-7]{26}/;

/** The invite code inside `text`, or `null` when there is none. */
export function findInvite(text: string): string | null {
  return INVITE.exec(text.trim().toLowerCase())?.[0] ?? null;
}

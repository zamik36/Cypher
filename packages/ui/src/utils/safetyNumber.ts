/** Splits a safety number into the five-digit groups people read aloud. */
export function digitGroups(number: string): string[] {
  return number.match(/\d{1,5}/g) ?? [];
}

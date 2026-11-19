/** How many words of the recovery phrase the user re-enters to show they wrote it down. */
export const CHECKED_WORDS = 3;

/** The words of a recovery phrase, however it was spaced or cased. */
export function phraseWords(phrase: string): string[] {
  return phrase.trim().toLowerCase().split(/\s+/).filter(Boolean);
}

/**
 * `count` distinct word positions (0-based, ascending) out of `total` to ask
 * for. `random` returns a number in [0, 1), like `Math.random`.
 */
export function pickPositions(total: number, count = CHECKED_WORDS, random: () => number = Math.random): number[] {
  const pool = Array.from({ length: total }, (_, i) => i);
  // A partial Fisher–Yates shuffle: the first `count` entries end up random.
  const picked = Math.min(count, total);
  for (let i = 0; i < picked; i++) {
    const j = i + Math.floor(random() * (total - i));
    const [a, b] = [pool[i], pool[j]];
    if (a === undefined || b === undefined) break;
    pool[i] = b;
    pool[j] = a;
  }
  return pool.slice(0, picked).sort((a, b) => a - b);
}

/** Whether `answers[k]` is the word at `positions[k]` of `words`, for every k. */
export function answersMatch(
  words: readonly string[],
  positions: readonly number[],
  answers: readonly string[],
): boolean {
  return (
    positions.length === answers.length &&
    positions.every((position, k) => {
      const answer = answers[k]?.trim().toLowerCase();
      return answer !== undefined && answer !== "" && answer === words[position];
    })
  );
}

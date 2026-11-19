import { describe, expect, it } from "vitest";
import { answersMatch, phraseWords, pickPositions } from "./recovery";

describe("recovery phrase check", () => {
  it("splits a phrase however it was typed", () => {
    expect(phraseWords("  Abandon  ability\nable ")).toEqual(["abandon", "ability", "able"]);
    expect(phraseWords("")).toEqual([]);
  });

  it("asks for distinct positions in order", () => {
    for (let seed = 0; seed < 50; seed++) {
      let state = seed;
      const random = () => {
        state = (state * 1103515245 + 12345) % 2 ** 31;
        return state / 2 ** 31;
      };
      const positions = pickPositions(24, 3, random);
      expect(positions).toHaveLength(3);
      expect(new Set(positions).size).toBe(3);
      expect([...positions].sort((a, b) => a - b)).toEqual(positions);
      expect(positions.every((p) => p >= 0 && p < 24)).toBe(true);
    }
    expect(pickPositions(2, 3)).toEqual([0, 1]);
  });

  it("accepts only the right words, ignoring case and spaces", () => {
    const words = phraseWords("one two three four five");
    expect(answersMatch(words, [1, 3], [" Two", "FOUR "])).toBe(true);
    expect(answersMatch(words, [1, 3], ["two", "five"])).toBe(false);
    expect(answersMatch(words, [1, 3], ["two", ""])).toBe(false);
    expect(answersMatch(words, [1, 3], ["two"])).toBe(false);
  });
});

import { describe, expect, it } from "vitest";
import { level } from "./voice";

describe("voice meter level", () => {
  it.each([
    [0, 0],
    [-1, 0],
    [0.001, 0],
    [1e-6, 0],
    [0.1, 2 / 3],
    [1, 1],
    [4, 1],
  ])("rms %d → %d (−60 dBFS..0 dBFS)", (rms, expected) => {
    expect(level(rms)).toBeCloseTo(expected, 9);
  });
});

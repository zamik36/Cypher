import { describe, expect, it } from "vitest";
import { digitGroups } from "./safetyNumber";

describe("digitGroups", () => {
  it("reads 60 digits as twelve groups of five", () => {
    const number = "0123456789".repeat(6);
    const groups = digitGroups(number);
    expect(groups).toHaveLength(12);
    expect(groups[0]).toBe("01234");
    expect(groups.join("")).toBe(number);
  });

  it("returns nothing for an empty number", () => {
    expect(digitGroups("")).toEqual([]);
  });
});

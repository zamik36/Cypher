import { describe, expect, it } from "vitest";
import { findDeviceOffer, offerName } from "./device";

/** An offer as the core writes it, for a device called `name`. */
function offer(name: string): string {
  const head = "01" + "ab".repeat(32) + "cd".repeat(32) + "07000000";
  const tail = Array.from(new TextEncoder().encode(name), (b) => b.toString(16).padStart(2, "0")).join("");
  return `cypher-device:${head}${tail}`;
}

describe("device offers", () => {
  it("are found inside whatever was scanned or pasted", () => {
    const code = offer("Laptop");
    expect(findDeviceOffer(`scan: ${code.toUpperCase()} `)).toBe(code);
    expect(findDeviceOffer("cypher-device:abcd")).toBeNull();
    expect(findDeviceOffer("an invite, not a device")).toBeNull();
  });

  it("carry the name the new device gave itself", () => {
    expect(offerName(offer("Ноутбук"))).toBe("Ноутбук");
    expect(offerName(offer(""))).toBe("");
  });
});

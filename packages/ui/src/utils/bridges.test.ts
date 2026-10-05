import { describe, expect, it } from "vitest";
import { checkBridges } from "./bridges";

const FP = "0123456789ABCDEF0123456789ABCDEF01234567";

describe("bridge lines", () => {
  it("accept plain bridges, with or without the Bridge keyword", () => {
    const check = checkBridges(`\n 192.0.2.1:443  ${FP} \nBridge [2001:db8::1]:9001 $${FP}\n192.0.2.1:443 ${FP}\n`);
    expect(check.problem).toBeNull();
    expect(check.lines).toEqual([`192.0.2.1:443 ${FP}`, `Bridge [2001:db8::1]:9001 $${FP}`]);
  });

  it("name the first line that cannot be used, and why", () => {
    expect(checkBridges(`192.0.2.1:443 ${FP}\nobfs4 192.0.2.2:443 ${FP} cert=x iat-mode=0`).problem).toEqual({
      line: 2,
      reason: "transport",
    });
    expect(checkBridges("not a bridge\nwebtunnel 1.2.3.4:443 x").problem).toEqual({ line: 1, reason: "format" });
    expect(checkBridges(`192.0.2.1 ${FP}`).problem?.reason).toBe("format");
  });

  it("treat an empty list as no bridges", () => {
    expect(checkBridges("  \n\n")).toEqual({ lines: [], problem: null });
  });
});

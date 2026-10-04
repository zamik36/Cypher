import { describe, expect, it } from "vitest";
import { t } from "../i18n";
import { joinErrorText } from "./joinError";

describe("joinErrorText", () => {
  it("explains a key substitution instead of showing the code", () => {
    expect(joinErrorText("KeyMismatch")).toBe(t().join_key_mismatch);
    expect(joinErrorText(new Error("NotFound"))).toBe(t().join_not_found);
  });

  it("passes unknown errors through", () => {
    expect(joinErrorText(new Error("gateway down"))).toBe("gateway down");
  });
});

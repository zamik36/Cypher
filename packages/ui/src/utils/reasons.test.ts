import { describe, expect, it } from "vitest";
import { t } from "../i18n";
import { reasonText } from "./reasons";

describe("reasonText", () => {
  it("words the core's reasons instead of showing their codes", () => {
    expect(reasonText("KeyMismatch")).toBe(t().join_key_mismatch);
    expect(reasonText(new Error("NotFound"))).toBe(t().join_not_found);
    expect(reasonText("UpdateRequired")).toBe(t().error_update_required);
    expect(reasonText(new Error("unsafe_type"))).toBe(t().file_unsafe);
  });

  it("passes unknown errors through", () => {
    expect(reasonText(new Error("gateway down"))).toBe("gateway down");
  });
});

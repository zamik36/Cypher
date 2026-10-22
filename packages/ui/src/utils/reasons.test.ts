import { describe, expect, it } from "vitest";
import { t } from "../i18n";
import { isConnecting, reasonText } from "./reasons";

describe("reasonText", () => {
  it("words the core's reasons instead of showing their codes", () => {
    expect(reasonText("key_mismatch")).toBe(t().join_key_mismatch);
    expect(reasonText(new Error("not_found"))).toBe(t().join_not_found);
    expect(reasonText("update_required")).toBe(t().error_update_required);
    expect(reasonText(new Error("unsafe_type"))).toBe(t().file_unsafe);
  });

  it("words a cancelled device link as the core's cancellations", () => {
    expect(reasonText(new Error("cancelled"))).toBe(t().error_cancelled);
  });

  it("knows the core's reasons only by their snake_case names", () => {
    expect(reasonText("KeyMismatch")).toBe("KeyMismatch");
  });

  it("words the desktop's own waits", () => {
    expect(reasonText("connecting")).toBe(t().error_connecting);
    expect(reasonText(new Error("lagged"))).toBe(t().error_lagged);
  });

  it("tells a client still starting apart from other failures", () => {
    expect(isConnecting("connecting")).toBe(true);
    expect(isConnecting(new Error("connecting"))).toBe(true);
    expect(isConnecting("not connected")).toBe(false);
  });

  it("passes unknown errors through", () => {
    expect(reasonText(new Error("gateway down"))).toBe("gateway down");
  });
});

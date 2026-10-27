import { describe, expect, it } from "vitest";
import { CommandError, type CmdErrorShape } from "../errors";
import { t } from "../i18n";
import { isConnecting, reasonText } from "./reasons";

const failure = (shape: CmdErrorShape) => new CommandError(shape);

describe("reasonText", () => {
  it("words the core's reasons an event carries instead of showing their codes", () => {
    expect(reasonText("key_mismatch")).toBe(t().join_key_mismatch);
    expect(reasonText(new Error("not_found"))).toBe(t().join_not_found);
    expect(reasonText("update_required")).toBe(t().error_update_required);
  });

  it("knows the core's reasons only by their snake_case names", () => {
    expect(reasonText("KeyMismatch")).toBe("KeyMismatch");
    expect(reasonText("toString")).toBe("toString");
  });

  it("words a command's error by its kind", () => {
    expect(reasonText(failure({ kind: "unsafe_type", message: "opening the file would run it" }))).toBe(
      t().file_unsafe,
    );
    expect(reasonText(failure({ kind: "locked", message: "identity is locked" }))).toBe(t().error_locked);
    // Plain data, as a worker posts it, words the same.
    expect(reasonText({ kind: "connecting", message: "connecting" })).toBe(t().error_connecting);
    expect(reasonText({ kind: "lagged", message: "replies were missed" })).toBe(t().error_lagged);
  });

  it("words a cancelled device link like the core's cancellations", () => {
    expect(reasonText(failure({ kind: "cancelled", message: "cancelled" }))).toBe(t().error_cancelled);
    expect(reasonText("cancelled")).toBe(t().error_cancelled);
  });

  it("words failures and identity errors by their reason", () => {
    expect(reasonText(failure({ kind: "failed", reason: "offline", message: "failed: Offline" }))).toBe(
      t().join_offline,
    );
    const wrong = failure({ kind: "identity", reason: "wrong_passphrase", message: "wrong passphrase" });
    expect(reasonText(wrong)).toBe(t().identity_err_wrong_passphrase);
  });

  it("shows an internal error, or anything else, as it says itself", () => {
    expect(reasonText(failure({ kind: "internal", message: "disk full" }))).toBe("disk full");
    expect(reasonText(new Error("gateway down"))).toBe("gateway down");
  });
});

describe("isConnecting", () => {
  it("tells a client still starting apart from other failures", () => {
    expect(isConnecting(failure({ kind: "connecting", message: "connecting" }))).toBe(true);
    expect(isConnecting({ kind: "connecting", message: "connecting" })).toBe(true);
    expect(isConnecting(failure({ kind: "not_connected", message: "not connected" }))).toBe(false);
    expect(isConnecting("connecting")).toBe(false);
  });
});

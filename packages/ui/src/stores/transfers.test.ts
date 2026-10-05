import { describe, expect, it } from "vitest";
import { hasTransfer, transferOf, transfers, upsertTransfer } from "./transfers";

describe("transfers store", () => {
  it("fills defaults for a new transfer and merges later updates", () => {
    upsertTransfer({ file_id: "f1" });
    expect(transfers.at(-1)).toEqual({
      file_id: "f1",
      file_name: "f1",
      total_size: 0,
      progress: 0,
      direction: "receive",
      status: "active",
    });
    upsertTransfer({ file_id: "f1", progress: 0.5, file_name: "a.txt" });
    expect(transfers.filter((t) => t.file_id === "f1")).toEqual([
      expect.objectContaining({ progress: 0.5, file_name: "a.txt" }),
    ]);
    expect(hasTransfer("f1")).toBe(true);
    expect(hasTransfer("f2")).toBe(false);
  });

  it("keeps given fields of a new transfer", () => {
    upsertTransfer({
      file_id: "f3",
      file_name: "b",
      total_size: 9,
      progress: 1,
      direction: "send",
      status: "complete",
    });
    expect(transferOf("f3")).toBe(transfers.find((t) => t.file_id === "f3"));
    expect(transferOf("nope")).toBeUndefined();
    expect(transferOf("f3")).toMatchObject({
      direction: "send",
      status: "complete",
      total_size: 9,
    });
  });
});

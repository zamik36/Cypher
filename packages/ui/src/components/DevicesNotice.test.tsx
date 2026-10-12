import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import DevicesNotice from "./DevicesNotice";
import { registerPlatform, type Platform } from "../platform";
import { dismissRestored, markRestored, setNoRoom, takeOwnDevices } from "../stores/devices";
import { setHistoryTransfer } from "../stores/history";
import { top } from "../stores/nav";

const replace = vi.fn<(id: number) => Promise<void>>().mockResolvedValue(undefined);
const cancelHistory = vi.fn<() => Promise<void>>().mockResolvedValue(undefined);

describe("DevicesNotice", () => {
  beforeEach(() => {
    replace.mockClear();
    registerPlatform({ devices: { replace, cancelHistory } } as unknown as Platform);
    setNoRoom(null);
    setHistoryTransfer(null);
    takeOwnDevices({ this: 1, devices: [{ id: 1, name: "" }] });
    dismissRestored();
  });
  afterEach(cleanup);

  it("asks which device to replace when the list is full, then replaces it", async () => {
    setNoRoom({
      this: 7,
      devices: [
        { id: 2, name: "" },
        { id: 3, name: "" },
      ],
    });
    render(() => <DevicesNotice />);
    expect(screen.getByTestId("no-room").textContent).toContain("No room for this device");

    fireEvent.click(screen.getByRole("button", { name: "Device 3" }));
    expect(screen.getByText(/Replace “Device 3”\?/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(replace).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "Device 3" }));
    fireEvent.click(screen.getByRole("button", { name: "Replace" }));
    await waitFor(() => expect(replace).toHaveBeenCalledWith(3));

    takeOwnDevices({ this: 7, devices: [{ id: 7, name: "" }] });
    expect(screen.queryByTestId("no-room")).toBeNull();
  });

  it("tells a restored device that the others keep working, once there are others", () => {
    markRestored();
    render(() => <DevicesNotice />);
    expect(screen.queryByTestId("restored-hint")).toBeNull();

    takeOwnDevices({
      this: 5,
      devices: [
        { id: 1, name: "" },
        { id: 5, name: "" },
      ],
    });
    expect(screen.getByTestId("restored-hint")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Open devices" }));
    expect(top()).toEqual({ name: "settings-section", section: "devices" });
    expect(screen.queryByTestId("restored-hint")).toBeNull();
  });

  it("follows the history coming from another device, which the user can stop", () => {
    render(() => <DevicesNotice />);
    expect(screen.queryByTestId("history-receiving")).toBeNull();
    setHistoryTransfer({ file_id: "h", outgoing: true, progress: 0.5, failed: null });
    expect(screen.queryByTestId("history-receiving")).toBeNull();

    setHistoryTransfer({ file_id: "h", outgoing: false, progress: 0.42, failed: null });
    expect(screen.getByTestId("history-receiving").textContent).toContain("Receiving the message history… 42%");
    fireEvent.click(screen.getByRole("button", { name: "Stop" }));
    expect(cancelHistory).toHaveBeenCalled();
  });

  it("lets the user dismiss the hint", () => {
    markRestored();
    takeOwnDevices({
      this: 5,
      devices: [
        { id: 1, name: "" },
        { id: 5, name: "" },
      ],
    });
    render(() => <DevicesNotice />);
    fireEvent.click(screen.getByRole("button", { name: "Got it" }));
    expect(screen.queryByTestId("restored-hint")).toBeNull();
  });
});

import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import UpdateBanner from "./UpdateBanner";
import { registerPlatform, type Platform } from "../platform";

function withUpdates(updates: Platform["updates"]) {
  registerPlatform({ ...(updates ? { updates } : {}), devices: {} } as unknown as Platform);
}

describe("UpdateBanner", () => {
  afterEach(cleanup);

  it("offers a newer version and installs it only when asked", async () => {
    const install = vi.fn<() => Promise<void>>().mockResolvedValue(undefined);
    withUpdates({ check: () => Promise.resolve("0.4.0"), install });
    render(() => <UpdateBanner />);

    const banner = await screen.findByTestId("update-banner");
    expect(banner.textContent).toContain("Version 0.4.0 is available");
    expect(install).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Update and restart" }));
    await waitFor(() => expect(install).toHaveBeenCalledOnce());
  });

  it("can be put off for this run", async () => {
    withUpdates({ check: () => Promise.resolve("0.4.0"), install: vi.fn() });
    render(() => <UpdateBanner />);
    await screen.findByTestId("update-banner");
    fireEvent.click(screen.getByRole("button", { name: "Later" }));
    expect(screen.queryByTestId("update-banner")).toBeNull();
  });

  it("says nothing when there is no update, it cannot be checked, or the platform has none", async () => {
    for (const updates of [
      { check: () => Promise.resolve(null), install: vi.fn() },
      { check: () => Promise.reject(new Error("offline")), install: vi.fn() },
      undefined,
    ]) {
      withUpdates(updates);
      render(() => <UpdateBanner />);
      await Promise.resolve();
      expect(screen.queryByTestId("update-banner")).toBeNull();
      cleanup();
    }
  });

  it("shows why an install failed and lets it be tried again", async () => {
    const install = vi.fn<() => Promise<void>>().mockRejectedValue(new Error("bad signature"));
    withUpdates({ check: () => Promise.resolve("0.4.0"), install });
    render(() => <UpdateBanner />);
    await screen.findByTestId("update-banner");
    fireEvent.click(screen.getByRole("button", { name: "Update and restart" }));
    await waitFor(() => expect(install).toHaveBeenCalledOnce());
    const retry = screen.getByRole<HTMLButtonElement>("button", { name: "Update and restart" });
    await waitFor(() => expect(retry.disabled).toBe(false));
  });
});

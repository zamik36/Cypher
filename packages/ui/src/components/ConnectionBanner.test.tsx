import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import ConnectionBanner from "./ConnectionBanner";
import { registerPlatform, type Platform } from "../platform";
import { changedServer, setChangedServer } from "../stores/serverKey";

const trustServerKey = vi.fn<() => Promise<void>>().mockResolvedValue(undefined);

describe("ConnectionBanner", () => {
  beforeEach(() => {
    trustServerKey.mockClear();
    registerPlatform({ trustServerKey, devices: {} } as unknown as Platform);
    setChangedServer(null);
  });
  afterEach(cleanup);

  it("warns that the server's key changed and trusts it only when asked twice", async () => {
    render(() => <ConnectionBanner />);
    expect(screen.queryByTestId("server-key-changed")).toBeNull();

    setChangedServer("gateway.example:9100");
    const banner = screen.getByTestId("server-key-changed");
    expect(banner.textContent).toContain("The server's key changed");

    fireEvent.click(screen.getByRole("button", { name: "Trust the new key" }));
    expect(banner.textContent).toContain("reinstalled");
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(trustServerKey).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "Trust the new key" }));
    fireEvent.click(screen.getByRole("button", { name: "Trust anyway" }));
    await waitFor(() => expect(trustServerKey).toHaveBeenCalledOnce());
    await waitFor(() => expect(changedServer()).toBeNull());
    expect(screen.queryByTestId("server-key-changed")).toBeNull();
  });
});

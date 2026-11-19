import { beforeEach, describe, expect, it, vi } from "vitest";

const load = () => import("./devices");

describe("own devices", () => {
  beforeEach(() => vi.resetModules());

  it("follow the core, with links and removals shown at once", async () => {
    const devices = await load();
    devices.addOwnDevice({ id: 9, name: "Early" });
    devices.dropOwnDevice(9);
    expect(devices.ownDevices()).toBeNull();

    devices.setOwnDevices({ this: 1, devices: [{ id: 1, name: "" }] });
    devices.addOwnDevice({ id: 2, name: "Laptop" });
    devices.addOwnDevice({ id: 2, name: "Laptop" });
    expect(devices.ownDevices()?.devices).toEqual([
      { id: 1, name: "" },
      { id: 2, name: "Laptop" },
    ]);
    devices.dropOwnDevice(2);
    expect(devices.ownDevices()).toEqual({ this: 1, devices: [{ id: 1, name: "" }] });
  });
});

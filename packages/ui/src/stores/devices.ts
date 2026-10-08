import { createSignal } from "solid-js";
import type { DeviceInfo } from "../platform";

/** This identity's devices as the core last told, and which one this is. */
export interface OwnDevices {
  this: number;
  devices: DeviceInfo[];
}

const [ownDevices, setOwnDevices] = createSignal<OwnDevices | null>(null);

export { ownDevices, setOwnDevices };

/** A newly linked device, named, before the core's next full list. */
export function addOwnDevice(device: DeviceInfo): void {
  const current = ownDevices();
  if (!current) return;
  const others = current.devices.filter((d) => d.id !== device.id);
  setOwnDevices({ ...current, devices: [...others, device] });
}

/** Drops a device from the list shown until the core's next full list. */
export function dropOwnDevice(id: number): void {
  const current = ownDevices();
  if (current) setOwnDevices({ ...current, devices: current.devices.filter((d) => d.id !== id) });
}

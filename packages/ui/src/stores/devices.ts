import { createSignal } from "solid-js";
import type { DeviceInfo } from "../platform";

/** This identity's devices as the core last told, and which one this is. */
export interface OwnDevices {
  this: number;
  devices: DeviceInfo[];
}

const [ownDevices, setOwnDevices] = createSignal<OwnDevices | null>(null);
/** The full list this device found, while the user picks one to replace. */
const [noRoom, setNoRoom] = createSignal<OwnDevices | null>(null);
/** Restored from the recovery phrase in this session, and not yet told. */
const [restored, setRestored] = createSignal(false);

export { noRoom, ownDevices, setNoRoom, setOwnDevices };

/** The core's list: this device is on it, so it no longer waits for room. */
export function takeOwnDevices(list: OwnDevices): void {
  setNoRoom(null);
  setOwnDevices(list);
}

/** This install restored the profile from its phrase, next to its other devices. */
export function markRestored(): void {
  setRestored(true);
}

export function dismissRestored(): void {
  setRestored(false);
}

/** Whether to tell the user that the profile's other devices keep working. */
export function restoredHint(): boolean {
  return restored() && (ownDevices()?.devices.length ?? 0) > 1;
}

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

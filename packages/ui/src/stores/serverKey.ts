import { createSignal } from "solid-js";

/**
 * The server that showed another signing key than the one this device
 * remembers for it; its anonymous channel is not used until the user trusts
 * the new key. `null` when nothing changed.
 */
const [changedServer, setChangedServer] = createSignal<string | null>(null);

export { changedServer, setChangedServer };

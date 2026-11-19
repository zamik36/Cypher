import { createSignal } from "solid-js";

/** The nickname of the unlocked identity; it never leaves this device. */
const [nickname, setNickname] = createSignal<string | null>(null);

export { nickname, setNickname };

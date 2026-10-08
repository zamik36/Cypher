/**
 * A new device's offer (`LinkOffer` in the core): `cypher-device:` and hex of
 * version(1) ‖ throwaway identity(32) ‖ its key(32) ‖ device id(4) ‖ name.
 * People paste it with whatever came around it, so it is found inside the
 * text.
 */
const OFFER = /cypher-device:[0-9a-f]+/;
const NAME_AT = 1 + 32 + 32 + 4;

/** The device offer inside `text`, or `null` when there is none. */
export function findDeviceOffer(text: string): string | null {
  const offer = OFFER.exec(text.trim().toLowerCase())?.[0];
  return offer && offer.length > "cypher-device:".length + NAME_AT * 2 ? offer : null;
}

/** The name the new device gave itself, as its offer carries it. */
export function offerName(offer: string): string {
  const hex = offer.slice("cypher-device:".length + NAME_AT * 2);
  const bytes = new Uint8Array((hex.match(/../g) ?? []).map((b) => parseInt(b, 16)));
  return new TextDecoder().decode(bytes).trim();
}

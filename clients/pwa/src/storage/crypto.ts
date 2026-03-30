/**
 * Client-side encryption helpers using Web Crypto API.
 *
 * Used by both identity storage and message persistence.
 * Mirrors the Rust `persistence::encryption` module.
 */

const NONCE_LEN = 12;

/** Cast Uint8Array to ArrayBuffer (TS5 strictness workaround). */
function buf(data: Uint8Array): ArrayBuffer {
  return data.buffer.slice(data.byteOffset, data.byteOffset + data.byteLength) as ArrayBuffer;
}

/** Encrypt data with AES-256-GCM. Returns { ciphertext, nonce }. */
export async function encrypt(
  key: CryptoKey,
  data: Uint8Array,
): Promise<{ ciphertext: Uint8Array; nonce: Uint8Array }> {
  const nonce = crypto.getRandomValues(new Uint8Array(NONCE_LEN));
  const ct = await crypto.subtle.encrypt(
    { name: "AES-GCM", iv: buf(nonce) },
    key,
    buf(data),
  );
  return { ciphertext: new Uint8Array(ct), nonce };
}

/** Decrypt data with AES-256-GCM. */
export async function decrypt(
  key: CryptoKey,
  ciphertext: Uint8Array,
  nonce: Uint8Array,
): Promise<Uint8Array> {
  const pt = await crypto.subtle.decrypt(
    { name: "AES-GCM", iv: buf(nonce) },
    key,
    buf(ciphertext),
  );
  return new Uint8Array(pt);
}

/** Derive a 256-bit AES-GCM key from a passphrase + salt using PBKDF2.
 *
 *  Note: browsers don't have Argon2id natively; PBKDF2 with 600k iterations
 *  is the OWASP-recommended alternative for Web Crypto API.
 */
export async function deriveKeyFromPassphrase(
  passphrase: string,
  salt: Uint8Array,
): Promise<CryptoKey> {
  const raw = new TextEncoder().encode(passphrase);
  const baseKey = await crypto.subtle.importKey("raw", buf(raw), "PBKDF2", false, [
    "deriveKey",
  ]);
  return crypto.subtle.deriveKey(
    { name: "PBKDF2", salt: buf(salt), iterations: 600_000, hash: "SHA-256" },
    baseKey,
    { name: "AES-GCM", length: 256 },
    false,
    ["encrypt", "decrypt"],
  );
}

/** Import a raw 32-byte key as an AES-GCM CryptoKey. */
export async function importAesKey(raw: Uint8Array): Promise<CryptoKey> {
  return crypto.subtle.importKey("raw", buf(raw), { name: "AES-GCM" }, false, [
    "encrypt",
    "decrypt",
  ]);
}

/** Derive a 32-byte sub-key from a seed via HKDF-SHA256. */
export async function hkdfDerive(
  seed: Uint8Array,
  info: string,
): Promise<Uint8Array> {
  const baseKey = await crypto.subtle.importKey("raw", buf(seed), "HKDF", false, [
    "deriveBits",
  ]);
  const bits = await crypto.subtle.deriveBits(
    {
      name: "HKDF",
      hash: "SHA-256",
      salt: buf(new Uint8Array(0)),
      info: buf(new TextEncoder().encode(info)),
    },
    baseKey,
    256,
  );
  return new Uint8Array(bits);
}

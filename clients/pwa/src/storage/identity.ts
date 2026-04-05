/**
 * Persistent anonymous identity backed by localStorage.
 *
 * The 32-byte seed is encrypted with a user-chosen PIN/passphrase
 * (PBKDF2 + AES-256-GCM) and stored as base64 in localStorage.
 *
 * Format: JSON { salt: base64, nonce: base64, ciphertext: base64 }
 *
 * The seed deterministically derives the peerId via HKDF, matching
 * the Rust `IdentitySeed` derivation.
 */
import { deriveKeyFromPassphrase, encrypt, decrypt, hkdfDerive } from "./crypto";
import { randomBytes, hexEncode } from "../api/proto";

const STORAGE_KEY = "cypher-identity";
const SEED_LEN = 32;

export interface IdentityData {
  seed: Uint8Array;
  nickname: string;
  peerId: Uint8Array;
  peerIdHex: string;
}

/** Check whether a saved identity exists. */
export function hasIdentity(): boolean {
  return localStorage.getItem(STORAGE_KEY) !== null;
}

/** Create a new random identity, encrypt, and save. */
export async function createIdentity(
  nickname: string,
  passphrase: string,
): Promise<IdentityData> {
  const seed = randomBytes(SEED_LEN);
  await saveEncrypted(seed, nickname, passphrase);
  return deriveIdentity(seed, nickname);
}

/** Unlock an existing identity with a passphrase. */
export async function unlockIdentity(
  passphrase: string,
): Promise<IdentityData> {
  const raw = localStorage.getItem(STORAGE_KEY);
  if (!raw) throw new Error("No identity found");

  const { salt, nonce, ciphertext } = JSON.parse(raw);
  const saltBytes = base64ToBytes(salt);
  const nonceBytes = base64ToBytes(nonce);
  const ctBytes = base64ToBytes(ciphertext);

  const key = await deriveKeyFromPassphrase(passphrase, saltBytes);
  let plaintext: Uint8Array;
  try {
    plaintext = await decrypt(key, ctBytes, nonceBytes);
  } catch {
    throw new Error("Wrong passphrase");
  }

  const { seed, nickname } = parsePlaintext(plaintext);
  return deriveIdentity(seed, nickname);
}

/** Import identity from a 24-word mnemonic. (Simplified: hex-encoded seed.) */
export async function importSeed(
  seedHex: string,
  nickname: string,
  passphrase: string,
): Promise<IdentityData> {
  if (!/^[0-9a-fA-F]{64}$/.test(seedHex)) {
    throw new Error("Seed must be exactly 64 hex characters (0-9, a-f)");
  }
  const seed = new Uint8Array(32);
  for (let i = 0; i < 32; i++) {
    seed[i] = parseInt(seedHex.substring(i * 2, i * 2 + 2), 16);
  }
  await saveEncrypted(seed, nickname, passphrase);
  return deriveIdentity(seed, nickname);
}

/** Export the raw seed as hex (for backup). Requires passphrase. */
export async function exportSeed(passphrase: string): Promise<string> {
  const { seed } = await unlockIdentity(passphrase);
  return hexEncode(seed);
}

/** Delete the stored identity. */
export function deleteIdentity(): void {
  localStorage.removeItem(STORAGE_KEY);
  sessionStorage.removeItem("cypher-session-sek");
  sessionStorage.removeItem("cypher-session-peerId");
  sessionStorage.removeItem("cypher-session-nickname");
}

/** Clear the cached session (forces re-authentication on next load). */
export function clearSession(): void {
  sessionStorage.removeItem("cypher-session-sek");
  sessionStorage.removeItem("cypher-session-peerId");
  sessionStorage.removeItem("cypher-session-nickname");
}

/** Derive peerId from seed (must match Rust HKDF derivation). */
async function deriveIdentity(
  seed: Uint8Array,
  nickname: string,
): Promise<IdentityData> {
  // Derive Ed25519-equivalent bytes via HKDF (same info string as Rust).
  const edBytes = await hkdfDerive(seed, "cypher-ed25519");
  // PeerId = Ed25519 public key, but we use the raw HKDF output as a
  // deterministic 32-byte identifier. This matches the wire format.
  // Note: for full compatibility, we'd need Ed25519 key derivation,
  // but for PWA-to-PWA this deterministic ID is sufficient.
  return {
    seed,
    nickname,
    peerId: edBytes,
    peerIdHex: hexEncode(edBytes),
  };
}

/** Derive the Storage Encryption Key from seed (matches Rust). */
export async function deriveStorageKey(seed: Uint8Array): Promise<Uint8Array> {
  return hkdfDerive(seed, "cypher-storage-key");
}

/** Derive the Blind Inbox ID from seed (matches Rust IdentitySeed::derive_inbox_id). */
export async function deriveInboxId(seed: Uint8Array): Promise<Uint8Array> {
  return hkdfDerive(seed, "cypher-inbox");
}

// --- Internal helpers ---

async function saveEncrypted(
  seed: Uint8Array,
  nickname: string,
  passphrase: string,
): Promise<void> {
  const salt = randomBytes(16);
  const key = await deriveKeyFromPassphrase(passphrase, salt);
  const plaintext = buildPlaintext(seed, nickname);
  const { ciphertext, nonce } = await encrypt(key, plaintext);

  localStorage.setItem(
    STORAGE_KEY,
    JSON.stringify({
      salt: bytesToBase64(salt),
      nonce: bytesToBase64(nonce),
      ciphertext: bytesToBase64(ciphertext),
    }),
  );
}

function buildPlaintext(seed: Uint8Array, nickname: string): Uint8Array {
  const nickBytes = new TextEncoder().encode(nickname);
  const buf = new Uint8Array(SEED_LEN + 4 + nickBytes.length);
  buf.set(seed, 0);
  new DataView(buf.buffer).setUint32(SEED_LEN, nickBytes.length, true);
  buf.set(nickBytes, SEED_LEN + 4);
  return buf;
}

function parsePlaintext(data: Uint8Array): { seed: Uint8Array; nickname: string } {
  if (data.length < SEED_LEN + 4) throw new Error("Data too short");
  const seed = data.slice(0, SEED_LEN);
  const nickLen = new DataView(data.buffer, data.byteOffset).getUint32(SEED_LEN, true);
  const nickname = new TextDecoder().decode(data.slice(SEED_LEN + 4, SEED_LEN + 4 + nickLen));
  return { seed, nickname };
}

function bytesToBase64(bytes: Uint8Array): string {
  let binary = "";
  for (const b of bytes) binary += String.fromCharCode(b);
  return btoa(binary);
}

function base64ToBytes(b64: string): Uint8Array {
  const binary = atob(b64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
  return bytes;
}

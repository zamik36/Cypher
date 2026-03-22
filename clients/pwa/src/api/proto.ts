/**
 * Binary protocol codec matching the Rust cypher-proto wire format.
 *
 * Wire format (little-endian):
 *   constructor_id: u32
 *   Bytes/String → u32 length prefix + data + padding to 4-byte alignment
 *   Int → u32 LE
 *   Long → u64 LE (as two u32s, BigInt not needed for our values)
 */


export const CID = {
  SessionInit:        0xA1000001,
  SessionAck:         0xA1000002,
  SignalRequestPeer:  0xB1000001,
  SignalIceCandidate: 0xB1000002,
  SignalOffer:        0xB1000003,
  SignalAnswer:       0xB1000004,
  KeysUploadPrekeys:  0xB2000001,
  KeysGetPrekeys:     0xB2000002,
  ChatSend:           0xC1000001,
  ChatReceive:        0xC1000002,
  FileOffer:          0xD1000001,
  FileAccept:         0xD1000002,
  FileChunk:          0xD1000003,
  FileComplete:       0xD1000004,
  FileChunkAck:       0xD1000005,
  FileResume:         0xD1000006,
} as const;


class Writer {
  private parts: Uint8Array[] = [];
  private len = 0;

  u32(v: number) {
    const b = new Uint8Array(4);
    new DataView(b.buffer).setUint32(0, v, true);
    this.parts.push(b);
    this.len += 4;
  }

  u64(v: number) {
    // Write as two u32 LE halves (works for values < 2^53).
    const b = new Uint8Array(8);
    const dv = new DataView(b.buffer);
    dv.setUint32(0, v & 0xFFFFFFFF, true);
    dv.setUint32(4, Math.floor(v / 0x100000000), true);
    this.parts.push(b);
    this.len += 8;
  }

  bytes(data: Uint8Array) {
    this.u32(data.length);
    this.parts.push(data);
    this.len += data.length;
    const pad = (4 - (data.length % 4)) % 4;
    if (pad > 0) {
      this.parts.push(new Uint8Array(pad));
      this.len += pad;
    }
  }

  string(s: string) {
    this.bytes(new TextEncoder().encode(s));
  }

  build(): Uint8Array {
    const out = new Uint8Array(this.len);
    let offset = 0;
    for (const p of this.parts) {
      out.set(p, offset);
      offset += p.length;
    }
    return out;
  }
}

class Reader {
  private dv: DataView;
  offset = 0;
  constructor(private data: Uint8Array) {
    this.dv = new DataView(data.buffer, data.byteOffset, data.byteLength);
  }

  u32(): number {
    const v = this.dv.getUint32(this.offset, true);
    this.offset += 4;
    return v;
  }

  u64(): number {
    const lo = this.dv.getUint32(this.offset, true);
    const hi = this.dv.getUint32(this.offset + 4, true);
    this.offset += 8;
    return hi * 0x100000000 + lo;
  }

  bytes(): Uint8Array {
    const len = this.u32();
    const data = this.data.slice(this.offset, this.offset + len);
    this.offset += len;
    const pad = (4 - (len % 4)) % 4;
    this.offset += pad;
    return data;
  }

  string(): string {
    return new TextDecoder().decode(this.bytes());
  }
}


export interface SessionInit { clientId: Uint8Array; nonce: Uint8Array }
export interface SessionAck { serverNonce: Uint8Array; timestamp: number }
export interface ChatSendMsg { peerId: Uint8Array; ciphertext: Uint8Array; ratchetKey: Uint8Array; msgNo: number }
export interface FileOfferMsg {
  peerId: Uint8Array; fileId: Uint8Array; name: string;
  size: number; chunks: number; hash: Uint8Array; compressed: number;
}
export interface FileAcceptMsg { peerId: Uint8Array; fileId: Uint8Array }
export interface FileChunkMsg {
  peerId: Uint8Array; fileId: Uint8Array; index: number;
  data: Uint8Array; hash: Uint8Array; ratchetKey: Uint8Array; msgNo: number;
}
export interface FileCompleteMsg { peerId: Uint8Array; fileId: Uint8Array }
export interface FileChunkAckMsg { peerId: Uint8Array; fileId: Uint8Array; index: number }
export interface SignalRequestPeerMsg { linkId: string }

export type ProtoMessage =
  | { type: "SessionInit"; msg: SessionInit }
  | { type: "SessionAck"; msg: SessionAck }
  | { type: "SignalRequestPeer"; msg: SignalRequestPeerMsg }
  | { type: "ChatSend"; msg: ChatSendMsg }
  | { type: "FileOffer"; msg: FileOfferMsg }
  | { type: "FileAccept"; msg: FileAcceptMsg }
  | { type: "FileChunk"; msg: FileChunkMsg }
  | { type: "FileComplete"; msg: FileCompleteMsg }
  | { type: "FileChunkAck"; msg: FileChunkAckMsg }
  | { type: "Unknown"; cid: number };


export function encodeSessionInit(clientId: Uint8Array, nonce: Uint8Array): Uint8Array {
  const w = new Writer();
  w.u32(CID.SessionInit);
  w.bytes(clientId);
  w.bytes(nonce);
  return w.build();
}

export function encodeSignalRequestPeer(linkId: string): Uint8Array {
  const w = new Writer();
  w.u32(CID.SignalRequestPeer);
  w.string(linkId);
  return w.build();
}

export function encodeChatSend(peerId: Uint8Array, ciphertext: Uint8Array, ratchetKey: Uint8Array, msgNo: number): Uint8Array {
  const w = new Writer();
  w.u32(CID.ChatSend);
  w.bytes(peerId);
  w.bytes(ciphertext);
  w.bytes(ratchetKey);
  w.u32(msgNo);
  return w.build();
}

export function encodeFileOffer(
  peerId: Uint8Array, fileId: Uint8Array, name: string,
  size: number, chunks: number, hash: Uint8Array, compressed: number,
): Uint8Array {
  const w = new Writer();
  w.u32(CID.FileOffer);
  w.bytes(peerId);
  w.bytes(fileId);
  w.string(name);
  w.u64(size);
  w.u32(chunks);
  w.bytes(hash);
  w.u32(compressed);
  return w.build();
}

export function encodeFileAccept(peerId: Uint8Array, fileId: Uint8Array): Uint8Array {
  const w = new Writer();
  w.u32(CID.FileAccept);
  w.bytes(peerId);
  w.bytes(fileId);
  return w.build();
}

export function encodeFileChunk(
  peerId: Uint8Array, fileId: Uint8Array, index: number,
  data: Uint8Array, hash: Uint8Array, ratchetKey: Uint8Array, msgNo: number,
): Uint8Array {
  const w = new Writer();
  w.u32(CID.FileChunk);
  w.bytes(peerId);
  w.bytes(fileId);
  w.u32(index);
  w.bytes(data);
  w.bytes(hash);
  w.bytes(ratchetKey);
  w.u32(msgNo);
  return w.build();
}

export function encodeFileComplete(peerId: Uint8Array, fileId: Uint8Array): Uint8Array {
  const w = new Writer();
  w.u32(CID.FileComplete);
  w.bytes(peerId);
  w.bytes(fileId);
  return w.build();
}

export function encodeFileChunkAck(peerId: Uint8Array, fileId: Uint8Array, index: number): Uint8Array {
  const w = new Writer();
  w.u32(CID.FileChunkAck);
  w.bytes(peerId);
  w.bytes(fileId);
  w.u32(index);
  return w.build();
}


export function dispatch(data: Uint8Array): ProtoMessage {
  if (data.length < 4) return { type: "Unknown", cid: 0 };
  const r = new Reader(data);
  const cid = r.u32();

  switch (cid) {
    case CID.SessionInit:
      return { type: "SessionInit", msg: { clientId: r.bytes(), nonce: r.bytes() } };
    case CID.SessionAck:
      return { type: "SessionAck", msg: { serverNonce: r.bytes(), timestamp: r.u64() } };
    case CID.SignalRequestPeer:
      return { type: "SignalRequestPeer", msg: { linkId: r.string() } };
    case CID.ChatSend:
      return { type: "ChatSend", msg: { peerId: r.bytes(), ciphertext: r.bytes(), ratchetKey: r.bytes(), msgNo: r.u32() } };
    case CID.FileOffer:
      return { type: "FileOffer", msg: { peerId: r.bytes(), fileId: r.bytes(), name: r.string(), size: r.u64(), chunks: r.u32(), hash: r.bytes(), compressed: r.u32() } };
    case CID.FileAccept:
      return { type: "FileAccept", msg: { peerId: r.bytes(), fileId: r.bytes() } };
    case CID.FileChunk:
      return { type: "FileChunk", msg: { peerId: r.bytes(), fileId: r.bytes(), index: r.u32(), data: r.bytes(), hash: r.bytes(), ratchetKey: r.bytes(), msgNo: r.u32() } };
    case CID.FileComplete:
      return { type: "FileComplete", msg: { peerId: r.bytes(), fileId: r.bytes() } };
    case CID.FileChunkAck:
      return { type: "FileChunkAck", msg: { peerId: r.bytes(), fileId: r.bytes(), index: r.u32() } };
    default:
      return { type: "Unknown", cid };
  }
}


export function hexEncode(data: Uint8Array): string {
  return Array.from(data).map((b) => b.toString(16).padStart(2, "0")).join("");
}

export function hexDecode(hex: string): Uint8Array {
  const bytes = new Uint8Array(hex.length / 2);
  for (let i = 0; i < bytes.length; i++) {
    bytes[i] = parseInt(hex.substring(i * 2, i * 2 + 2), 16);
  }
  return bytes;
}

/** Generate random bytes. */
export function randomBytes(n: number): Uint8Array {
  const buf = new Uint8Array(n);
  crypto.getRandomValues(buf);
  return buf;
}

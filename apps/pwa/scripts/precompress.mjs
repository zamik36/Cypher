// Writes zstd, brotli and gzip siblings of every compressible file in dist/
// at maximum levels, for Caddy's `file_server { precompressed }`: clients
// get smaller responses and the server spends no CPU compressing per request.

import { readFileSync, readdirSync, statSync, writeFileSync } from "node:fs";
import { extname, join } from "node:path";
import { brotliCompressSync, constants, gzipSync, zstdCompressSync } from "node:zlib";

const DIST = new URL("../dist/", import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, "$1");
const COMPRESSIBLE = new Set([".html", ".js", ".mjs", ".css", ".wasm", ".json", ".svg", ".webmanifest", ".txt"]);
/// Below this, headers outweigh the savings.
const MIN_BYTES = 1024;

const encoders = [
  [".zst", (data) => zstdCompressSync(data, { params: { [constants.ZSTD_c_compressionLevel]: 19 } })],
  [".br", (data) => brotliCompressSync(data, { params: { [constants.BROTLI_PARAM_QUALITY]: 11 } })],
  [".gz", (data) => gzipSync(data, { level: 9 })],
];

function* files(dir) {
  for (const name of readdirSync(dir)) {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) yield* files(path);
    else yield path;
  }
}

let original = 0;
let smallest = 0;
for (const path of files(DIST)) {
  if (!COMPRESSIBLE.has(extname(path))) continue;
  const data = readFileSync(path);
  if (data.length < MIN_BYTES) continue;
  const sizes = encoders.map(([suffix, encode]) => {
    const packed = encode(data);
    writeFileSync(path + suffix, packed);
    return packed.length;
  });
  original += data.length;
  smallest += Math.min(...sizes);
}
console.log(`precompressed ${(original / 1024).toFixed(0)} KiB -> ${(smallest / 1024).toFixed(0)} KiB (best encoding)`);

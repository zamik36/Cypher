#!/usr/bin/env node
/**
 * Generate PNG icons from SVG source for PWA manifest.
 * Install: npm install -D @resvg/resvg-js
 * Run:     node scripts/generate-icons.js
 */
import { readFileSync, writeFileSync } from "fs";
import { join, dirname } from "path";
import { fileURLToPath } from "url";
import { Resvg } from "@resvg/resvg-js";

const __dirname = dirname(fileURLToPath(import.meta.url));
const iconsDir = join(__dirname, "..", "public", "icons");
const svg = readFileSync(join(iconsDir, "icon-512.svg"), "utf-8");

const SIZES = [
  { name: "icon-180.png", size: 180 },
  { name: "icon-192.png", size: 192 },
  { name: "icon-512.png", size: 512 },
  { name: "icon-maskable-512.png", size: 512, maskable: true },
];

for (const { name, size, maskable } of SIZES) {
  let input = svg;

  // Maskable icons need 10% safe-zone padding
  if (maskable) {
    const pad = Math.round(size * 0.1);
    const inner = size - pad * 2;
    input = `<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="0 0 ${size} ${size}">
  <rect width="${size}" height="${size}" fill="#0b0e14"/>
  <svg x="${pad}" y="${pad}" width="${inner}" height="${inner}" viewBox="0 0 512 512">
    ${svg.replace(/<\/?svg[^>]*>/g, "")}
  </svg>
</svg>`;
  }

  const resvg = new Resvg(input, { fitTo: { mode: "width", value: size } });
  writeFileSync(join(iconsDir, name), resvg.render().asPng());
  console.log(`${name} (${size}x${size})`);
}

console.log("Icons generated.");

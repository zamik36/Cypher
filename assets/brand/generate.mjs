import { copyFileSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { createRequire } from "node:module";
import { execFileSync } from "node:child_process";
import { Resvg } from "@resvg/resvg-js";

const require = createRequire(import.meta.url);
const brand = dirname(fileURLToPath(import.meta.url));
const root = resolve(brand, "../..");
const source = readFileSync(join(brand, "icon.svg"), "utf8");
const mark = source.match(/<path\b[^>]*\/>/s)[0];
const background = "#101112";
const desktop = join(root, "apps/desktop/src-tauri/icons");
const android = join(root, "apps/desktop/src-tauri/gen/android/app/src/main/res");
const pwa = join(root, "apps/pwa/public/icons");
const scratch = join(root, "target/branding/tauri");

function svg(body) {
  return `<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">${body}</svg>\n`;
}

function png(path, input, size) {
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, new Resvg(input, { fitTo: { mode: "width", value: size } }).render().asPng());
}

const opaque = svg(`<rect width="1024" height="1024" fill="${background}"/>${mark}`);
// Android guarantees a circle of radius 33dp on a 108dp adaptive canvas.
const foreground = svg(`<g transform="translate(153.6 153.6) scale(.7)">${mark}</g>`);
const maskable = svg(
  `<rect width="1024" height="1024" fill="${background}"/><g transform="translate(51.2 51.2) scale(.9)">${mark}</g>`,
);

mkdirSync(dirname(scratch), { recursive: true });
writeFileSync(join(dirname(scratch), "android-foreground.svg"), foreground);
writeFileSync(join(dirname(scratch), "maskable.svg"), maskable);
for (const size of [16, 32, 48, 512]) {
  png(join(dirname(scratch), `preview-${size}.png`), source, size);
}

png(join(brand, "icon-1024.png"), source, 1024);
for (const size of [192, 512]) {
  writeFileSync(
    join(pwa, `icon-${size}.svg`),
    source.replace('width="1024" height="1024"', `width="${size}" height="${size}"`),
  );
  png(join(pwa, `icon-${size}.png`), source, size);
}
png(join(pwa, "icon-180.png"), opaque, 180);
png(join(pwa, "icon-maskable-512.png"), maskable, 512);

execFileSync(
  process.execPath,
  [
    require.resolve("@tauri-apps/cli/tauri.js"),
    "icon",
    join(brand, "icon-1024.png"),
    "--output",
    scratch,
    "--ios-color",
    background,
  ],
  { cwd: root, stdio: "inherit" },
);
for (const entry of readdirSync(desktop, { withFileTypes: true })) {
  if (entry.isFile() && entry.name !== ".gitkeep") {
    copyFileSync(join(scratch, entry.name), join(desktop, entry.name));
  }
}
// Tauri emits ICNS representations in hash-map order; canonicalize the container.
const icnsPath = join(desktop, "icon.icns");
const icns = readFileSync(icnsPath);
const chunks = [];
for (let offset = 8; offset < icns.length;) {
  const length = icns.readUInt32BE(offset + 4);
  if (length < 8 || offset + length > icns.length) throw new Error("Invalid ICNS chunk length");
  chunks.push(icns.subarray(offset, offset + length));
  offset += length;
}
chunks.sort(Buffer.compare);
writeFileSync(icnsPath, Buffer.concat([icns.subarray(0, 8), ...chunks]));
for (const name of readdirSync(join(desktop, "ios"))) {
  const generated = readFileSync(join(scratch, "ios", name));
  png(join(desktop, "ios", name), opaque, generated.readUInt32BE(16));
}

for (const [density, scale] of Object.entries({ mdpi: 1, hdpi: 1.5, xhdpi: 2, xxhdpi: 3, xxxhdpi: 4 })) {
  const dir = join(android, `mipmap-${density}`);
  png(join(dir, "ic_launcher.png"), source, 48 * scale);
  png(join(dir, "ic_launcher_foreground.png"), foreground, 108 * scale);
  const round = svg(
    `<defs><clipPath id="round"><circle cx="512" cy="512" r="512"/></clipPath></defs><g clip-path="url(#round)">${opaque.replace(/<\/?svg[^>]*>/g, "")}</g>`,
  );
  png(join(dir, "ic_launcher_round.png"), round, 48 * scale);
}
writeFileSync(
  join(android, "values/ic_launcher_background.xml"),
  `<?xml version="1.0" encoding="utf-8"?>\n<resources>\n  <color name="ic_launcher_background">${background}</color>\n</resources>\n`,
);
writeFileSync(
  join(android, "drawable/ic_launcher_background.xml"),
  `<?xml version="1.0" encoding="utf-8"?>\n<shape xmlns:android="http://schemas.android.com/apk/res/android" android:shape="rectangle">\n  <solid android:color="${background}"/>\n</shape>\n`,
);
const path = mark.match(/\bd="([^"]+)"/)[1];
writeFileSync(
  join(android, "drawable-v24/ic_launcher_foreground.xml"),
  `<?xml version="1.0" encoding="utf-8"?>\n<vector xmlns:android="http://schemas.android.com/apk/res/android" android:width="108dp" android:height="108dp" android:viewportWidth="1024" android:viewportHeight="1024">\n  <group android:translateX="153.6" android:translateY="153.6" android:scaleX="0.7" android:scaleY="0.7">\n    <path android:fillColor="#D8FF32" android:fillType="evenOdd" android:pathData="${path}"/>\n  </group>\n</vector>\n`,
);
console.log("Generated brand, PWA, desktop, iOS and Android icons from assets/brand/icon.svg");

# Cypher icon assets

Edit `icon.svg`, the 1024-unit master, to change the artwork. The mark combines
a circular outer curve, curved negative space and a square lower-left section
on a rounded square. The background is `#101112`; the mark is `#D8FF32`.

From the repository root, with the existing npm dependencies installed:

```sh
node assets/brand/generate.mjs
# Equivalent existing entry point:
npm run generate-icons --workspace cypher-pwa
```

The generator uses `@resvg/resvg-js` for SVG rasterization and the installed
Tauri CLI 2.12 icon command for desktop PNG/ICO/ICNS. It writes intermediate
Tauri exports and 16/32/48/512 PNG previews under `target/branding/`. ICNS chunks
are sorted because Tauri's container ordering varies between runs.
Generation does not require Cargo or an emulator.
Re-running with the same dependency versions produces identical asset bytes.

| Destination                 | Dimensions (pixels)                                   | Alpha / background                          |
| --------------------------- | ----------------------------------------------------- | ------------------------------------------- |
| Brand master PNG            | 1024 square                                           | Transparent outside rounded square          |
| PWA SVG / PNG               | 192, 512 square                                       | Transparent outside rounded square          |
| Apple touch PNG             | 180 square                                            | Opaque near-black; OS applies corners       |
| PWA maskable PNG            | 512 square                                            | Opaque near-black; mark scaled to 90%       |
| Desktop PNG                 | 32, 64, 128, 256, 512 and existing Windows tile sizes | Transparent corners                         |
| Desktop ICO                 | 16, 24, 32, 48, 64, 256                               | Alpha; multiple resolutions                 |
| Desktop ICNS                | 16 through 1024 representations                       | Alpha; multiple resolutions                 |
| Existing iOS PNG set        | 20 through 1024                                       | Opaque near-black; OS applies corners       |
| Android legacy / round      | 48, 72, 96, 144, 192                                  | Transparent rounded-square / circle corners |
| Android adaptive foreground | 108, 162, 216, 324, 432                               | Transparent, mark only                      |

Android adaptive layers retain the existing resource names and XML wiring. The
foreground is centered at 70% of master size inside the full 108dp canvas, keeping
all artwork inside Android's guaranteed central circle (66dp diameter). The
vector drawable fallback uses the same transform. Both background resources are
solid near-black. The PWA maskable mark fits its central circle of radius 40% of
the canvas. Neither adaptive format includes a second rounded-square tile.

Onboarding loads the master SVG through Vite's asset URL handling. Its empty alt
text keeps the logo decorative beside the existing heading. Only its artwork
and local logo backing changed; the app's accent theme is unchanged.

PWA icon URLs and manifest wiring stay stable. The existing service worker uses
network-first requests for `/icons/` and a fresh cache name per build; offline
clients may retain the prior cached icon until online. Installed launchers and
Apple touch icons can cache artwork independently and may need reinstalling.
Vite hashes/inlines the onboarding asset as part of the app build. Android and
desktop launchers can also retain icons until reinstall/restart. Do not treat a
cached old launcher icon as proof that packaged resources are wrong.

After changing the artwork, rebuild the PWA and Android package. Check the
onboarding screen, installed launcher icon and packaged resources on a device
or emulator.

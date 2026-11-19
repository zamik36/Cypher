import { writeFile } from "node:fs/promises";
import { expect, test } from "@playwright/test";
import { chromium } from "@playwright/test";
import { createIdentity, newUser } from "./users";

const [WIDTH, HEIGHT] = [640, 480];

/** One still Y4M frame (4:2:0, grey chroma) from 8-bit luma. */
function y4m(luma: Uint8Array): Buffer {
  const header = Buffer.from(`YUV4MPEG2 W${WIDTH} H${HEIGHT} F30:1 Ip A1:1 C420jpeg\nFRAME\n`);
  const chroma = Buffer.alloc((WIDTH / 2) * (HEIGHT / 2) * 2, 128);
  return Buffer.concat([header, Buffer.from(luma), chroma]);
}

test("an invite QR shown on one screen is scanned by another camera", async ({ browser, baseURL }, info) => {
  const host = await newUser(browser, baseURL ?? "");
  await createIdentity(host, "hana");
  await host.getByRole("button", { name: "New chat" }).click();
  const qr = host.locator(".invite-qr img");
  await expect(qr).toBeVisible();

  // The QR as a camera would see it: dark modules on white, in the middle.
  const luma = await qr.evaluate(
    async (img: HTMLImageElement, { width, height }) => {
      await img.decode();
      const canvas = Object.assign(document.createElement("canvas"), { width, height });
      const g = canvas.getContext("2d");
      if (!g) throw new Error("no canvas");
      g.fillStyle = "#fff";
      g.fillRect(0, 0, width, height);
      const side = 360;
      g.imageSmoothingEnabled = false;
      g.drawImage(img, (width - side) / 2, (height - side) / 2, side, side);
      const { data } = g.getImageData(0, 0, width, height);
      return Array.from({ length: width * height }, (_, i) => {
        const [r = 0, gr = 0, b = 0] = data.subarray(i * 4, i * 4 + 3);
        return Math.round(0.299 * r + 0.587 * gr + 0.114 * b);
      });
    },
    { width: WIDTH, height: HEIGHT },
  );
  const video = info.outputPath("invite.y4m");
  await writeFile(video, y4m(Uint8Array.from(luma)));

  const camera = await chromium.launch({
    args: [
      "--use-fake-ui-for-media-stream",
      "--use-fake-device-for-media-stream",
      `--use-file-for-fake-video-capture=${video}`,
    ],
  });
  try {
    const guest = await newUser(camera, baseURL ?? "");
    await createIdentity(guest, "gus");
    await guest.getByRole("button", { name: "New chat" }).click();
    await guest.getByRole("tab", { name: "Enter code" }).click();
    await guest.getByRole("button", { name: "Scan QR code" }).click();
    await expect(guest.getByRole("dialog", { name: "Scan QR code" })).toBeVisible();
    for (const page of [host, guest]) await expect(page.getByTestId("chat-header")).toBeVisible();
    await expect(guest.getByTestId("chat-header")).toContainText("hana");
  } finally {
    await camera.close();
  }
});

import { expect, test, type Page } from "@playwright/test";
import { createIdentity, incoming, newUser, outgoing, pair } from "./users";

/** Presses the record button, waits for `recording` to show, holds, releases. */
async function holdRecord(page: Page, title: RegExp, recording: string, holdMs: number): Promise<void> {
  const box = await page.getByTitle(title).boundingBox();
  if (!box) throw new Error("record button is not visible");
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.down();
  await expect(page.locator(recording)).toBeVisible();
  await page.waitForTimeout(holdMs);
  await page.mouse.up();
}

test("voice and round video notes record, deliver and play", async ({ browser, baseURL }) => {
  const alice = await newUser(browser, baseURL ?? "");
  const bob = await newUser(browser, baseURL ?? "");
  await createIdentity(alice, "alice");
  await createIdentity(bob, "bob");
  await pair(alice, bob);

  await test.step("hold to record a voice note", async () => {
    await holdRecord(bob, /Hold to record a voice message/, ".recording-bar", 1500);
    await expect(outgoing(bob).locator(".voice-bubble")).toBeVisible();

    const note = incoming(alice).locator(".voice-bubble");
    await expect(note.locator(".voice-bar")).toHaveCount(64);
    await note.getByRole("button", { name: "Play" }).click();
    await expect(note.getByRole("button", { name: "Pause" })).toBeVisible();
    await expect(note.locator(".voice-bar.played").first()).toBeVisible();
  });

  await test.step("tap to switch to video, hold to record a round note", async () => {
    await bob.getByTitle(/Hold to record a voice message/).click();
    await holdRecord(bob, /Hold to record a video message/, ".video-note-overlay", 2000);

    const note = incoming(alice).locator(".round-video");
    await note.click();
    await expect(note).toHaveClass(/playing/);
    const size = () => note.locator("video").evaluate((v: HTMLVideoElement) => [v.videoWidth, v.videoHeight]);
    await expect.poll(size).toEqual([384, 384]);
  });
});

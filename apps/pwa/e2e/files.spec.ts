import { expect, test, type Page } from "@playwright/test";
import { createIdentity, incoming, newUser, outgoing, pair } from "./users";

/** Drops a freshly drawn PNG named `name` on the open chat, as a file manager would. */
async function dropPicture(page: Page, name: string): Promise<void> {
  const transfer = await page.evaluateHandle(async (fileName) => {
    const canvas = Object.assign(document.createElement("canvas"), { width: 96, height: 64 });
    const g = canvas.getContext("2d");
    if (!g) throw new Error("no canvas");
    g.fillStyle = "#2fbf8a";
    g.fillRect(0, 0, 96, 64);
    const blob = await new Promise<Blob | null>((done) => canvas.toBlob(done, "image/png"));
    if (!blob) throw new Error("no png");
    const data = new DataTransfer();
    data.items.add(new File([blob], fileName, { type: "image/png" }));
    return data;
  }, name);
  const chat = page.locator("section.chat");
  await chat.dispatchEvent("dragenter", { dataTransfer: transfer });
  await expect(page.locator(".chat__drop")).toBeVisible();
  await chat.dispatchEvent("drop", { dataTransfer: transfer });
  await expect(page.locator(".chat__drop")).toBeHidden();
}

test("a picture dropped on the chat arrives with a preview and opens full screen", async ({ browser, baseURL }) => {
  const alice = await newUser(browser, baseURL ?? "");
  const bob = await newUser(browser, baseURL ?? "");
  await createIdentity(alice, "alice");
  await createIdentity(bob, "bob");
  await pair(alice, bob);

  await dropPicture(bob, "sunset.png");
  await expect(outgoing(bob).getByTestId("file-card")).toContainText("sunset.png");

  const card = incoming(alice).getByTestId("file-card");
  await card.click();
  await alice.getByRole("menuitem", { name: "Download" }).click();
  await expect(card).toHaveAttribute("data-state", "complete");
  await expect(card.locator(".file-card__image")).toBeVisible();

  await card.click();
  await alice.getByRole("menuitem", { name: "View" }).click();
  const viewer = alice.getByRole("dialog", { name: "sunset.png" });
  await expect(viewer).toBeVisible();
  await alice.keyboard.press("Escape");
  await expect(viewer).toBeHidden();
  // Escape closed the picture, not the chat.
  await expect(alice.getByTestId("chat-header")).toBeVisible();
});

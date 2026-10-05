import { createHash, randomBytes } from "node:crypto";
import { readFile } from "node:fs/promises";
import { expect, test } from "@playwright/test";
import {
  createIdentity,
  expectConnected,
  incoming,
  linkStatus,
  messages,
  newUser,
  openFirstChat,
  openSettings,
  outgoing,
  ownPeerId,
  pair,
  PASSPHRASE,
  say,
  setPassphrase,
  unlock,
} from "./users";

test("two people pair, chat, send a file and come back later", async ({ browser, baseURL }) => {
  const alice = await newUser(browser, baseURL ?? "");
  const bob = await newUser(browser, baseURL ?? "");
  await createIdentity(alice, "alice");
  await createIdentity(bob, "bob");
  await pair(alice, bob);

  await test.step("chat both ways with delivery and read receipts", async () => {
    await say(bob, "hello alice");
    await expect(incoming(alice).getByText("hello alice")).toBeVisible();
    // Alice has the chat open, so the message is marked read.
    await expect(outgoing(bob).locator('[data-testid="message-status"][data-status="read"]')).toBeVisible();
    await say(alice, "hi bob");
    await expect(incoming(bob).getByText("hi bob")).toBeVisible();
    // The chat list shows the conversation with its last message.
    await expect(bob.getByTestId("chat-row").first()).toContainText("hi bob");
  });

  await test.step("both sides see the same safety number", async () => {
    const readNumber = async (page: typeof alice) => {
      await page.getByRole("button", { name: "Verify safety number" }).click();
      const dialog = page.getByRole("dialog");
      const digits = dialog.locator(".safety-digits");
      await expect(digits.locator("li")).toHaveCount(12);
      const number = (await digits.getAttribute("aria-label")) ?? "";
      await dialog.getByRole("button", { name: "Close" }).click();
      await expect(dialog).toBeHidden();
      return number;
    };
    const [fromAlice, fromBob] = [await readNumber(alice), await readNumber(bob)];
    expect(fromAlice).toMatch(/^\d{60}$/);
    expect(fromAlice).toBe(fromBob);
  });

  await test.step("a 3 MiB file arrives intact, accepted from the chat", async () => {
    const payload = randomBytes(3 * 1024 * 1024 + 12_345);
    const chooser = bob.waitForEvent("filechooser");
    await bob.getByRole("button", { name: "Attach a file" }).click();
    await (await chooser).setFiles({ name: "payload.bin", mimeType: "application/octet-stream", buffer: payload });

    const card = incoming(alice).getByTestId("file-card");
    await expect(card).toContainText("payload.bin");
    const download = alice.waitForEvent("download");
    await card.getByRole("button", { name: "Accept" }).click();
    const saved = await (await download).path();
    const digest = (bytes: Buffer) => createHash("sha256").update(bytes).digest("hex");
    expect(digest(await readFile(saved))).toBe(digest(payload));
    await expect(card).toHaveAttribute("data-state", "complete");
  });

  await test.step("messages sent while offline arrive after unlocking", async () => {
    await alice.reload();
    await say(bob, "while you were away");
    await unlock(alice, "not the passphrase");
    await expect(alice.getByTestId("identity-error")).toBeVisible();
    await unlock(alice);
    await openFirstChat(alice);
    for (const text of ["hello alice", "hi bob", "while you were away"]) {
      await expect(messages(alice).getByText(text)).toBeVisible();
    }
    await expect(alice.getByTestId("day-separator")).toHaveCount(1);

    // The received file is still kept: it can be saved again.
    const card = incoming(alice).getByTestId("file-card");
    await expect(card).toHaveAttribute("data-state", "complete");
    const again = alice.waitForEvent("download");
    await card.getByRole("button", { name: "Save" }).click();
    expect((await again).suggestedFilename()).toBe("payload.bin");
  });

  await test.step("a contact gets a name on this device, and back returns to the chat", async () => {
    await alice.getByRole("button", { name: "Contact info" }).click();
    await alice.getByLabel("Name", { exact: true }).fill("Bob");
    await alice.getByRole("button", { name: "Save" }).click();
    await expect(alice.getByRole("heading", { name: "Bob" })).toBeVisible();
    await alice.goBack();
    await expect(alice.getByTestId("chat-header")).toContainText("Bob");
    await expect(alice.getByTestId("chat-row").first()).toContainText("Bob");
  });

  await test.step("clearing history removes it from the screen and the device", async () => {
    await openSettings(alice, "Data and storage");
    await alice.getByRole("button", { name: "Clear all history" }).click();
    await alice.getByRole("button", { name: "Clear", exact: true }).click();
    await openFirstChat(alice);
    await expect(messages(alice)).toHaveCount(0);

    await alice.reload();
    await unlock(alice);
    await openFirstChat(alice);
    await expect(alice.getByText("No messages yet. Say hello!")).toBeVisible();
    await expect(messages(alice)).toHaveCount(0);
  });
});

test("a recovery phrase restores the identity on a new device", async ({ browser, baseURL }) => {
  const original = await newUser(browser, baseURL ?? "");
  const shown = await createIdentity(original, "carol");
  const peerId = await ownPeerId(original);

  await original.getByPlaceholder("Passphrase", { exact: true }).fill(PASSPHRASE);
  await original.getByRole("button", { name: "Show", exact: true }).click();
  const words = original.getByTestId("recovery-phrase").locator("li");
  await expect(words).toHaveCount(24);
  const phrase = (await words.allTextContents()).join(" ");
  expect(phrase).toBe(shown);

  const restored = await newUser(browser, baseURL ?? "");
  await restored.getByRole("button", { name: "Restore from phrase" }).click();
  await restored.getByPlaceholder("Recovery phrase (24 words)").fill(phrase);
  await restored.getByPlaceholder("Nickname").fill("carol");
  await setPassphrase(restored, "a different passphrase");
  await restored.getByRole("button", { name: "Restore", exact: true }).click();
  await expectConnected(restored);
  expect(await ownPeerId(restored)).toBe(peerId);

  // One identity, one session: the new device takes it over, and the old one
  // says so instead of silently going quiet, until it takes the session back.
  const banner = original.getByRole("alert").filter({ hasText: "Cypher is open on another device" });
  await expect(banner).toBeVisible();
  await expect(linkStatus(original)).toHaveAttribute("data-state", "superseded");
  await banner.getByRole("button", { name: "Use here" }).click();
  await expectConnected(original);
  await expect(banner).toBeHidden();
  await expect(linkStatus(restored)).toHaveAttribute("data-state", "superseded");

  // A forgotten passphrase: erase the device, then restore from the phrase.
  await original.reload();
  await original.getByRole("button", { name: "Forgot your passphrase?" }).click();
  await original.getByRole("button", { name: "Erase this device", exact: true }).click();
  await expect(original.getByPlaceholder("Nickname")).toBeVisible();
  await original.getByRole("button", { name: "Restore from phrase" }).click();
  await original.getByPlaceholder("Recovery phrase (24 words)").fill(phrase);
  await original.getByPlaceholder("Nickname").fill("carol");
  await setPassphrase(original, "a passphrase remembered");
  await original.getByRole("button", { name: "Restore", exact: true }).click();
  await expectConnected(original);
  expect(await ownPeerId(original)).toBe(peerId);
});

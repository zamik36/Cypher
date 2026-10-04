import { createHash, randomBytes } from "node:crypto";
import { readFile } from "node:fs/promises";
import { expect, test } from "@playwright/test";
import {
  createIdentity,
  expectConnected,
  incoming,
  navigate,
  newUser,
  outgoing,
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
    await expect(outgoing(bob).locator(".message-status.read")).toBeVisible();
    await say(alice, "hi bob");
    await expect(incoming(bob).getByText("hi bob")).toBeVisible();
  });

  await test.step("both sides see the same safety number", async () => {
    const readNumber = async (page: typeof alice) => {
      await page.getByRole("button", { name: "Verify safety number" }).click();
      const digits = page.locator(".safety-digits");
      await expect(digits.locator("li")).toHaveCount(12);
      const number = (await digits.getAttribute("aria-label")) ?? "";
      await page.getByRole("button", { name: "Close" }).click();
      await expect(page.locator(".safety-number")).toBeHidden();
      return number;
    };
    const [fromAlice, fromBob] = [await readNumber(alice), await readNumber(bob)];
    expect(fromAlice).toMatch(/^\d{60}$/);
    expect(fromAlice).toBe(fromBob);
  });

  await test.step("a 3 MiB file arrives intact", async () => {
    const payload = randomBytes(3 * 1024 * 1024 + 12_345);
    const chooser = bob.waitForEvent("filechooser");
    await bob.getByTitle("Choose File").click();
    await (await chooser).setFiles({ name: "payload.bin", mimeType: "application/octet-stream", buffer: payload });

    await navigate(alice, "Files");
    const download = alice.waitForEvent("download");
    await alice.getByRole("button", { name: "Accept" }).click();
    const saved = await (await download).path();
    const digest = (bytes: Buffer) => createHash("sha256").update(bytes).digest("hex");
    expect(digest(await readFile(saved))).toBe(digest(payload));
  });

  await test.step("messages sent while offline arrive after unlocking", async () => {
    await alice.reload();
    await say(bob, "while you were away");
    await unlock(alice, "not the passphrase");
    await expect(alice.locator(".identity-error")).toBeVisible();
    await unlock(alice);
    await navigate(alice, "Chat");
    await alice.locator(".peer-item").first().click();
    for (const text of ["hello alice", "hi bob", "while you were away"]) {
      await expect(alice.locator(".message-group").getByText(text)).toBeVisible();
    }
  });

  await test.step("clearing history removes it from the screen and the device", async () => {
    await navigate(alice, "Settings");
    await alice.getByRole("button", { name: "Clear chat history" }).click();
    await alice.getByRole("button", { name: "Confirm delete" }).click();
    await navigate(alice, "Chat");
    await alice.locator(".peer-item").first().click();
    await expect(alice.locator(".message-group")).toHaveCount(0);

    await alice.reload();
    await unlock(alice);
    await navigate(alice, "Chat");
    await alice.locator(".peer-item").first().click();
    await expect(alice.getByText("No messages yet. Say hello!")).toBeVisible();
    await expect(alice.locator(".message-group")).toHaveCount(0);
  });
});

test("a recovery phrase restores the identity on a new device", async ({ browser, baseURL }) => {
  const original = await newUser(browser, baseURL ?? "");
  const shown = await createIdentity(original, "carol");
  const peerId = await original.locator(".status-bar .peer-pill").getAttribute("title");

  await navigate(original, "Settings");
  await original.getByPlaceholder("Enter passphrase to export").fill(PASSPHRASE);
  await original.getByRole("button", { name: "Export", exact: true }).click();
  const phrase = (await original.locator(".seed-display code").textContent()) ?? "";
  expect(phrase.split(" ")).toHaveLength(24);
  expect(phrase).toBe(shown);

  const restored = await newUser(browser, baseURL ?? "");
  await restored.getByRole("button", { name: "Import" }).click();
  await restored.getByPlaceholder("Recovery phrase (24 words)").fill(phrase);
  await restored.getByPlaceholder("Nickname").fill("carol");
  await setPassphrase(restored, "a different passphrase");
  await restored.getByRole("button", { name: "Import" }).click();
  await expect(restored.locator(".status-bar .peer-pill")).toHaveAttribute("title", peerId ?? "");

  // One identity, one session: the new device takes it over, and the old one
  // says so instead of silently going quiet, until it takes the session back.
  await expectConnected(restored);
  const banner = original.getByRole("alert").filter({ hasText: "Cypher is open on another device" });
  await expect(banner).toBeVisible();
  await expect(original.locator(".status-bar")).toContainText("Open on another device");
  await banner.getByRole("button", { name: "Use here" }).click();
  await expectConnected(original);
  await expect(banner).toBeHidden();
  await expect(restored.locator(".status-bar")).toContainText("Open on another device");
});

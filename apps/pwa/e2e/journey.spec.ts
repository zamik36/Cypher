import { createHash, randomBytes } from "node:crypto";
import { readFile } from "node:fs/promises";
import { expect, test } from "@playwright/test";
import { createIdentity, incoming, navigate, newUser, outgoing, pair, PASSPHRASE, say, unlock } from "./users";

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
});

test("a recovery phrase restores the identity on a new device", async ({ browser, baseURL }) => {
  const original = await newUser(browser, baseURL ?? "");
  await createIdentity(original, "carol");
  const peerId = await original.locator(".status-bar .peer-pill").getAttribute("title");

  await navigate(original, "Settings");
  await original.getByPlaceholder("Enter passphrase to export").fill(PASSPHRASE);
  await original.getByRole("button", { name: "Export", exact: true }).click();
  const phrase = (await original.locator(".seed-display code").textContent()) ?? "";
  expect(phrase.split(" ")).toHaveLength(24);

  const restored = await newUser(browser, baseURL ?? "");
  await restored.getByRole("button", { name: "Import" }).click();
  await restored.getByPlaceholder("Recovery phrase (24 words)").fill(phrase);
  await restored.getByPlaceholder("Nickname").fill("carol");
  await restored.getByPlaceholder("Passphrase", { exact: true }).fill("a different passphrase");
  await restored.getByRole("button", { name: "Import" }).click();
  await expect(restored.locator(".status-bar .peer-pill")).toHaveAttribute("title", peerId ?? "");
});

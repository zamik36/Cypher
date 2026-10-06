import { expect, test, type Page } from "@playwright/test";
import { createIdentity, incoming, newUser, outgoing, pair, unlock } from "./users";

/** Posts to the share target as Android's share sheet would, through the service worker. */
async function shareFromAnotherApp(page: Page, fileName: string, text: string): Promise<void> {
  await page.evaluate(
    async ({ name, caption }) => {
      await navigator.serviceWorker.ready;
      while (!navigator.serviceWorker.controller) await new Promise((done) => setTimeout(done, 50));
      const form = new FormData();
      form.append("text", caption);
      form.append("files", new File(["shared from elsewhere"], name, { type: "text/plain" }));
      await fetch("/share", { method: "POST", body: form });
    },
    { name: fileName, caption: text },
  );
  await page.goto("/?share");
}

test("a file shared from another app goes to the chat the user picks", async ({ browser, baseURL }) => {
  const alice = await newUser(browser, baseURL ?? "");
  const bob = await newUser(browser, baseURL ?? "");
  await createIdentity(alice, "alice");
  await createIdentity(bob, "bob");
  await pair(alice, bob);

  await shareFromAnotherApp(alice, "notes.txt", "look at this");
  await expect(alice).toHaveURL(/\/$/);
  await unlock(alice);
  await expect(alice.getByTestId("share-pick")).toContainText("Choose a chat for the file");
  // The worker's copy is gone once the app has it.
  expect(await alice.evaluate(() => caches.has("cypher-share"))).toBe(false);

  await alice.getByTestId("chat-row").first().click();
  await expect(alice.getByTestId("share-pick")).toBeHidden();
  await expect(outgoing(alice).getByTestId("file-card")).toContainText("notes.txt");
  await expect(alice.getByPlaceholder("Message", { exact: true })).toHaveValue("look at this");
  await expect(incoming(bob).getByTestId("file-card")).toContainText("notes.txt");
});

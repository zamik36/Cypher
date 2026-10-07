import { expect, test } from "@playwright/test";
import { createIdentity, newUser, openSettings } from "./users";

test("a push signal shows a wake-up notification only while the app is away", async ({ browser, baseURL }) => {
  const page = await newUser(browser, baseURL ?? "");
  await page.context().grantPermissions(["notifications"]);
  await createIdentity(page, "pia");
  await openSettings(page, "Notifications");
  await page.getByRole("switch", { name: "New messages" }).click();
  const closed = page.getByRole("switch", { name: "When the app is closed" });
  await expect(closed).toBeEnabled();
  await expect(closed).toBeChecked();

  // Deliver a signal straight to the service worker, as a push service would.
  const cdp = await page.context().newCDPSession(page);
  const registration = new Promise<string>((resolve) => {
    cdp.on("ServiceWorker.workerRegistrationUpdated", ({ registrations }) => {
      const ours = registrations.find((r) => r.scopeURL.startsWith(baseURL ?? ""));
      if (ours) resolve(ours.registrationId);
    });
  });
  await cdp.send("ServiceWorker.enable");
  const registrationId = await registration;
  const origin = new URL(baseURL ?? "").origin;
  const shown = () =>
    page.evaluate(async () => {
      const reg = await navigator.serviceWorker.ready;
      return (await reg.getNotifications({ tag: "messages" })).map((n) => `${n.title}: ${n.body}`);
    });
  const push = () => cdp.send("ServiceWorker.deliverPushMessage", { origin, registrationId, data: "cypher:inbox" });

  // Open and visible: the app fetches by itself, nothing to show.
  await push();
  await page.waitForTimeout(500);
  expect(await shown()).toEqual([]);

  // Away: the signal becomes a notification without sender or text.
  // No window of ours is left open.
  await page.goto("about:blank");
  await push();
  await page.goto("/");
  await expect.poll(shown).toEqual(["Cypher: New message"]);
});

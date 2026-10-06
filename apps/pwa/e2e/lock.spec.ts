import { expect, test } from "@playwright/test";
import { createIdentity, expectConnected, newUser, openSettings, unlock } from "./users";

test("the app locks on request and after the chosen idle time", async ({ browser, baseURL }) => {
  const page = await newUser(browser, baseURL ?? "");
  await createIdentity(page, "dora");
  const passphrase = page.getByPlaceholder("Passphrase", { exact: true });

  await test.step("lock now, then unlock", async () => {
    await openSettings(page, "Privacy and network");
    await page.getByRole("button", { name: /^Lock now/ }).click();
    await expect(passphrase).toBeVisible();
    await expect(page.getByTestId("chat-list")).toHaveCount(0);
    await unlock(page);
    await expectConnected(page);
  });

  await test.step("an idle minute locks it", async () => {
    await page.clock.install();
    await openSettings(page, "Privacy and network");
    await page.getByRole("button", { name: "1 min" }).click();
    // Any input restarts the countdown, now on the installed clock.
    await page.keyboard.press("Shift");
    await page.clock.fastForward("00:50");
    await expect(passphrase).toBeHidden();
    await page.clock.fastForward("00:15");
    await expect(passphrase).toBeVisible();
  });
});

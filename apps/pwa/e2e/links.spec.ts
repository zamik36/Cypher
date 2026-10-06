import { expect, test } from "@playwright/test";
import { createIdentity, newUser } from "./users";

test("an invite link opens New chat with the code, once the app is unlocked", async ({ browser, baseURL }) => {
  const host = await newUser(browser, baseURL ?? "");
  await createIdentity(host, "hal");
  await host.getByRole("button", { name: "New chat" }).click();
  const code = (await host.getByTestId("invite-code").textContent()) ?? "";

  const context = await browser.newContext({ baseURL: baseURL ?? "", locale: "en-US" });
  const guest = await context.newPage();
  await guest.goto(`/join#${code}`);
  // The code leaves the address bar at once; it never went to the server.
  await expect(guest).toHaveURL(/\/$/);
  await createIdentity(guest, "gia");
  await expect(guest.getByPlaceholder("Invite code")).toHaveValue(code);
  await guest.getByRole("button", { name: "Start chat" }).click();
  for (const page of [host, guest]) await expect(page.getByTestId("chat-header")).toBeVisible();
  await expect(guest.getByTestId("chat-header")).toContainText("hal");
});

import { expect, test } from "@playwright/test";
import { createIdentity, incoming, newUser, openSettings, say } from "./users";

test("a second joiner on a used invite waits as a request; blocking silences a contact", async ({
  browser,
  baseURL,
}) => {
  const [alice, bob, carol] = [
    await newUser(browser, baseURL ?? ""),
    await newUser(browser, baseURL ?? ""),
    await newUser(browser, baseURL ?? ""),
  ];
  await createIdentity(alice, "alice");
  await createIdentity(bob, "bob");
  await createIdentity(carol, "carol");

  // Bob joins by Alice's invite; Carol, say having seen it too, joins after.
  await alice.getByRole("button", { name: "New chat" }).click();
  const code = (await alice.getByTestId("invite-code").textContent()) ?? "";
  for (const page of [bob, carol]) {
    await page.getByRole("button", { name: "New chat" }).click();
    await page.getByRole("tab", { name: "Enter code" }).click();
    await page.getByPlaceholder("Invite code").fill(code);
    await page.getByRole("button", { name: "Start chat" }).click();
    await expect(page.getByTestId("chat-header")).toBeVisible();
  }
  await expect(bob.getByTestId("chat-header")).toContainText("alice");

  await test.step("the second joiner is a request, told nothing until accepted", async () => {
    const requests = alice.getByTestId("requests");
    await expect(requests.getByTestId("chat-row")).toHaveCount(1);
    await expect(requests).toContainText("carol");
    await expect(carol.getByTestId("chat-header")).not.toContainText("alice");
    await say(carol, "hi, it's carol");
    await requests.getByTestId("chat-row").click();
    await expect(alice.getByTestId("request-notice")).toBeVisible();
    await expect(incoming(alice).getByText("hi, it's carol")).toBeVisible();

    await alice.getByRole("button", { name: "Accept" }).click();
    await expect(alice.getByTestId("request-notice")).toBeHidden();
    await expect(alice.getByTestId("requests")).toHaveCount(0);
    await expect(carol.getByTestId("chat-header")).toContainText("alice");
  });

  await test.step("a blocked contact is not heard until unblocked", async () => {
    await alice.getByTestId("chat-row").filter({ hasText: "bob" }).click();
    await alice.getByRole("button", { name: "Contact info" }).click();
    await alice.getByRole("button", { name: /^Block/ }).click();
    await alice.goBack();
    await expect(alice.getByTestId("blocked-notice")).toBeVisible();
    await expect(alice.getByPlaceholder("Message", { exact: true })).toBeHidden();

    await say(bob, "are you there?");
    await bob.waitForTimeout(1500);
    await expect(alice.getByText("are you there?")).toHaveCount(0);

    await openSettings(alice, "Privacy and network");
    await alice.getByRole("button", { name: "Unblock" }).click();
    await expect(alice.getByText("Nobody is blocked.")).toBeVisible();
    await say(bob, "now?");
    await alice.getByTestId("chat-row").filter({ hasText: "bob" }).click();
    await expect(incoming(alice).getByText("now?")).toBeVisible();
  });
});

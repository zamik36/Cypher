import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Page } from "@playwright/test";
import { createIdentity, newUser, openSettings, pair, say } from "./users";

/** Serious and critical problems axe finds on what `page` shows now. */
async function violations(page: Page, where: string): Promise<string[]> {
  // Colours mid-fade are not what anyone reads; spinners never finish.
  await page.evaluate(() =>
    Promise.all(
      document
        .getAnimations()
        .filter((a) => a.effect?.getComputedTiming().iterations !== Infinity)
        .map((a) => a.finished),
    ),
  );
  const result = await new AxeBuilder({ page }).analyze();
  return result.violations
    .filter((v) => v.impact === "serious" || v.impact === "critical")
    .map(
      (v) =>
        `${where}: ${v.id} (${v.impact}) — ${v.nodes.map((n) => `${n.target.join(" ")} [${n.any[0]?.message ?? ""}]`).join(", ")}`,
    );
}

test("the main screens have no serious accessibility problems", async ({ browser, baseURL }) => {
  const found: string[] = [];
  const alice = await newUser(browser, baseURL ?? "");
  found.push(...(await violations(alice, "welcome")));
  const bob = await newUser(browser, baseURL ?? "");
  await createIdentity(alice, "alice");
  await createIdentity(bob, "bob");
  found.push(...(await violations(alice, "empty chat list")));

  await alice.getByRole("button", { name: "New chat" }).click();
  await expect(alice.getByTestId("invite-code")).toBeVisible();
  found.push(...(await violations(alice, "new chat")));
  await alice.goBack();

  await pair(alice, bob);
  await say(bob, "hello there");
  await say(alice, "hi, bob");
  await expect(alice.getByTestId("message")).toHaveCount(2);
  found.push(...(await violations(alice, "chat")));
  await alice.getByTestId("message").first().click({ button: "right" });
  await expect(alice.getByRole("menu")).toBeVisible();
  found.push(...(await violations(alice, "message menu")));
  await alice.keyboard.press("Escape");

  await alice.getByTestId("chat-header").click();
  found.push(...(await violations(alice, "contact")));

  for (const section of [
    "Profile",
    "Appearance",
    "Notifications",
    "Privacy and network",
    "Data and storage",
  ] as const) {
    await openSettings(alice, section);
    found.push(...(await violations(alice, `settings: ${section}`)));
  }

  // The theme follows the system's.
  await alice.emulateMedia({ colorScheme: "dark" });
  await alice.getByTestId("chat-row").first().click();
  found.push(...(await violations(alice, "dark chat")));

  await alice.reload();
  await expect(alice.getByPlaceholder("Passphrase", { exact: true })).toBeVisible();
  found.push(...(await violations(alice, "unlock")));

  expect(found).toEqual([]);
});

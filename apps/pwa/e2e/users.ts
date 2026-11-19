import { expect, type Browser, type Page } from "@playwright/test";

export const PASSPHRASE = "correct horse battery staple";

/**
 * A person with their own browser profile (IndexedDB, OPFS, identity),
 * English UI, and a fake camera and microphone. The default viewport is
 * wide, so the chat list stays beside the open chat.
 */
export async function newUser(browser: Browser, baseURL: string): Promise<Page> {
  const context = await browser.newContext({ baseURL, locale: "en-US", permissions: ["microphone", "camera"] });
  const page = await context.newPage();
  await page.goto("/");
  return page;
}

export async function createIdentity(page: Page, nickname: string): Promise<string> {
  await page.getByPlaceholder("Nickname").fill(nickname);
  await setPassphrase(page, PASSPHRASE);
  await page.getByRole("button", { name: "Create profile" }).click();
  const phrase = await confirmRecoveryPhrase(page);
  await expectConnected(page);
  return phrase;
}

/** Types a new passphrase and its repetition. */
export async function setPassphrase(page: Page, passphrase: string): Promise<void> {
  await page.getByPlaceholder("Passphrase, at least 12 characters").fill(passphrase);
  await page.getByPlaceholder("Repeat passphrase").fill(passphrase);
}

/** Reads the phrase shown after creating an identity and answers the check. */
async function confirmRecoveryPhrase(page: Page): Promise<string> {
  const list = page.getByTestId("recovery-words").locator("li");
  await expect(list).toHaveCount(24);
  const words = await list.allTextContents();
  await page.getByRole("button", { name: "I have written it down" }).click();
  const continueButton = page.getByRole("button", { name: "Continue" });
  await expect(continueButton).toBeDisabled();
  for (const input of await page.getByRole("textbox", { name: /^Word #\d+$/ }).all()) {
    const label = (await input.getAttribute("aria-label")) ?? "";
    await input.fill(words[Number(label.replace("Word #", "")) - 1] ?? "");
  }
  await continueButton.click();
  return words.join(" ");
}

export async function unlock(page: Page, passphrase = PASSPHRASE): Promise<void> {
  await page.getByPlaceholder("Passphrase", { exact: true }).fill(passphrase);
  await page.getByPlaceholder("Passphrase", { exact: true }).press("Enter");
}

export const linkStatus = (page: Page) => page.getByTestId("link-status");

export async function expectConnected(page: Page): Promise<void> {
  await expect(linkStatus(page)).toHaveAttribute("data-state", "online");
}

/** `host` shows an invite code, `guest` enters it; both end up in the chat. */
export async function pair(host: Page, guest: Page): Promise<void> {
  await host.getByRole("button", { name: "New chat" }).click();
  const code = (await host.getByTestId("invite-code").textContent()) ?? "";
  expect(code).not.toBe("");
  await guest.getByRole("button", { name: "New chat" }).click();
  await guest.getByRole("tab", { name: "Enter code" }).click();
  await guest.getByPlaceholder("Invite code").fill(code);
  await guest.getByRole("button", { name: "Start chat" }).click();
  for (const page of [host, guest]) await expect(page.getByTestId("chat-header")).toBeVisible();
}

/** Opens the first (most recent) conversation from the chat list. */
export async function openFirstChat(page: Page): Promise<void> {
  await page.getByTestId("chat-row").first().click();
  await expect(page.getByTestId("chat-header")).toBeVisible();
}

/** Opens a section of Settings from wherever the app is. */
export async function openSettings(
  page: Page,
  section: "Profile" | "Appearance" | "Notifications" | "Privacy and network" | "Data and storage",
): Promise<void> {
  await page.getByRole("button", { name: "Settings" }).click();
  await page.getByRole("button", { name: new RegExp(`^${section}`) }).click();
}

/** Reads the profile's own ID from Settings → Profile. */
export async function ownPeerId(page: Page): Promise<string> {
  await openSettings(page, "Profile");
  const id = (await page.getByTestId("own-peer-id").textContent())?.trim() ?? "";
  expect(id).not.toBe("");
  return id;
}

export async function say(page: Page, text: string): Promise<void> {
  const input = page.getByPlaceholder("Message", { exact: true });
  await input.fill(text);
  await input.press("Enter");
}

export const messages = (page: Page) => page.getByTestId("message");
export const incoming = (page: Page) => page.locator('[data-testid="message"][data-dir="in"]');
export const outgoing = (page: Page) => page.locator('[data-testid="message"][data-dir="out"]');

import { expect, type Browser, type Page } from "@playwright/test";

export const PASSPHRASE = "correct horse battery staple";

/**
 * A person with their own browser profile (IndexedDB, OPFS, identity),
 * English UI, and a fake camera and microphone.
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
  await page.getByRole("button", { name: "Create identity" }).click();
  const phrase = await confirmRecoveryPhrase(page);
  await expectConnected(page);
  return phrase;
}

/** Types a new passphrase and its repetition. */
export async function setPassphrase(page: Page, passphrase: string): Promise<void> {
  await page.getByPlaceholder("Passphrase (min 12 chars)").fill(passphrase);
  await page.getByPlaceholder("Repeat passphrase").fill(passphrase);
}

/** Reads the phrase shown after creating an identity and answers the check. */
async function confirmRecoveryPhrase(page: Page): Promise<string> {
  const list = page.locator(".recovery-words li");
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

export async function expectConnected(page: Page): Promise<void> {
  await expect(page.locator(".status-bar")).toContainText("Connected");
  await expect(page.locator(".status-bar .dot.online")).toBeVisible();
}

/** `host` opens a room, `guest` joins it; both end up in the chat. */
export async function pair(host: Page, guest: Page): Promise<void> {
  await host.getByRole("button", { name: "Create Room" }).click();
  const code = (await host.locator(".room-code").textContent()) ?? "";
  expect(code).not.toBe("");
  await guest.getByPlaceholder("Enter room code").fill(code);
  await guest.getByRole("button", { name: "Join", exact: true }).click();
  for (const page of [host, guest]) await expect(page.locator(".chat-header")).toBeVisible();
}

/** Opens a section from the sidebar; its label may carry an unread count. */
export async function navigate(page: Page, section: "Home" | "Chat" | "Files" | "Settings"): Promise<void> {
  await page
    .getByRole("complementary")
    .getByRole("button", { name: new RegExp(`^${section}`) })
    .click();
}

export async function say(page: Page, text: string): Promise<void> {
  const input = page.getByPlaceholder("Type a message...");
  await input.fill(text);
  await input.press("Enter");
}

export const incoming = (page: Page) => page.locator(".message-group.theirs");
export const outgoing = (page: Page) => page.locator(".message-group.mine");

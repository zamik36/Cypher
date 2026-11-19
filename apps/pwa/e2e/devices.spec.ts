import { expect, test } from "@playwright/test";
import {
  createIdentity,
  expectConnected,
  incoming,
  newUser,
  openFirstChat,
  pair,
  PASSPHRASE,
  say,
  setPassphrase,
} from "./users";

test("a new device is linked by its code and gets the profile's chats and history", async ({ browser, baseURL }) => {
  const [alice, bob, laptop] = [
    await newUser(browser, baseURL ?? ""),
    await newUser(browser, baseURL ?? ""),
    await newUser(browser, baseURL ?? ""),
  ];
  await createIdentity(alice, "alice");
  await createIdentity(bob, "bob");
  await pair(alice, bob);
  await say(bob, "from before the laptop");
  await expect(incoming(alice).getByText("from before the laptop")).toBeVisible();

  await test.step("the new device shows its code", async () => {
    await laptop.getByRole("button", { name: "I already use Cypher — link this device" }).click();
    await laptop.getByLabel("Device name").fill("Laptop");
    await setPassphrase(laptop, PASSPHRASE);
    await laptop.getByRole("button", { name: "Show the code" }).click();
    await expect(laptop.getByTestId("device-offer")).toContainText("cypher-device:");
  });
  const offer = (await laptop.getByTestId("device-offer").textContent()) ?? "";

  await test.step("a device of the profile links it", async () => {
    await alice.getByRole("button", { name: "Settings" }).click();
    await alice.getByRole("button", { name: "Devices" }).click();
    await alice.getByRole("button", { name: "Enter the code instead" }).click();
    await alice.getByLabel("Code from the new device").fill(offer);
    await alice.getByRole("button", { name: "Link", exact: true }).click();
    await expect(alice.getByText("Link “Laptop”?", { exact: false })).toBeVisible();
    await expect(alice.getByRole("switch", { name: "Also transfer the message history" })).toHaveAttribute(
      "aria-checked",
      "true",
    );
    await alice.getByRole("button", { name: "Link", exact: true }).click();
    await expect(alice.getByTestId("devices")).toContainText("Laptop");
  });

  await test.step("the new device opens with the profile's contacts and earlier chats", async () => {
    await expectConnected(laptop);
    await expect(laptop.getByTestId("chat-row")).toContainText("bob");
    await expect(laptop.getByText("Your earlier chats are here")).toBeVisible();
    await openFirstChat(laptop);
    await expect(incoming(laptop).getByText("from before the laptop")).toBeVisible();
  });

  await test.step("what the contact writes reaches the new device too", async () => {
    await say(bob, "hello, both of you");
    await openFirstChat(laptop);
    await expect(incoming(laptop).getByText("hello, both of you")).toBeVisible();
  });
});

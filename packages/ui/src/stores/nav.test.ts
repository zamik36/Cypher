import { beforeEach, describe, expect, it, vi } from "vitest";

const load = () => import("./nav");

describe("navigation", () => {
  beforeEach(() => vi.resetModules());

  it("stacks screens on top of the chat list and mirrors them in history", async () => {
    const nav = await load();
    const pushState = vi.spyOn(history, "pushState");
    expect(nav.top()).toEqual({ name: "chats" });
    nav.push({ name: "chat", peerId: "p" });
    nav.push({ name: "contact", peerId: "p" });
    expect(nav.depth()).toBe(2);
    expect(pushState).toHaveBeenLastCalledWith({ depth: 2 }, "");
    expect(nav.find("chat")).toEqual({ name: "chat", peerId: "p" });
    expect(nav.find("settings")).toBeUndefined();

    nav.replace({ name: "settings" });
    expect(nav.screens().map((s) => s.name)).toEqual(["chats", "chat", "settings"]);
    expect(pushState).toHaveBeenCalledTimes(2);
  });

  it("pushes instead of replacing the chat list", async () => {
    const nav = await load();
    nav.replace({ name: "chat", peerId: "p" });
    expect(nav.screens().map((s) => s.name)).toEqual(["chats", "chat"]);
  });

  it("goes back through history, and never past the chat list", async () => {
    const nav = await load();
    const back = vi.spyOn(history, "back").mockImplementation(() => undefined);
    nav.back();
    expect(back).not.toHaveBeenCalled();
    nav.push({ name: "settings" });
    nav.back();
    expect(back).toHaveBeenCalledOnce();
  });

  it("trims the stack to a history depth, and resets", async () => {
    const nav = await load();
    nav.push({ name: "settings" });
    nav.push({ name: "settings-section", section: "privacy" });
    nav.popTo(5);
    expect(nav.depth()).toBe(2);
    nav.popTo(1);
    expect(nav.top()).toEqual({ name: "settings" });
    nav.popTo(-3);
    expect(nav.depth()).toBe(0);
    nav.push({ name: "settings" });
    nav.reset();
    expect(nav.depth()).toBe(0);
  });

  it("follows popstate and Escape once installed", async () => {
    const nav = await load();
    const back = vi.spyOn(history, "back").mockImplementation(() => undefined);
    const uninstall = nav.installNavigation();
    nav.push({ name: "settings" });
    nav.push({ name: "settings-section", section: "storage" });

    window.dispatchEvent(new PopStateEvent("popstate", { state: { depth: 1 } }));
    expect(nav.top()).toEqual({ name: "settings" });
    window.dispatchEvent(new PopStateEvent("popstate", { state: null }));
    expect(nav.depth()).toBe(0);

    nav.push({ name: "settings" });
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter" }));
    expect(back).not.toHaveBeenCalled();
    const escape = new KeyboardEvent("keydown", { key: "Escape", cancelable: true });
    window.dispatchEvent(escape);
    expect(back).toHaveBeenCalledOnce();
    expect(escape.defaultPrevented).toBe(true);

    // An open dialog closes itself first.
    const dialog = document.createElement("dialog");
    dialog.setAttribute("open", "");
    document.body.append(dialog);
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    expect(back).toHaveBeenCalledOnce();
    dialog.remove();

    uninstall();
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    expect(back).toHaveBeenCalledOnce();
  });

  it("ignores Escape on the chat list", async () => {
    const nav = await load();
    const back = vi.spyOn(history, "back");
    const uninstall = nav.installNavigation();
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    expect(back).not.toHaveBeenCalled();
    uninstall();
  });
});

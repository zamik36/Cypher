import { beforeEach, describe, expect, it, vi } from "vitest";
import en from "./en";
import ru from "./ru";

describe("translations", () => {
  it("have the same keys and argument counts in every locale", () => {
    expect(Object.keys(ru).sort()).toEqual(Object.keys(en).sort());
    for (const key of Object.keys(en) as (keyof typeof en)[]) {
      const [a, b] = [en[key], ru[key]];
      expect(typeof b, key).toBe(typeof a);
      if (typeof a === "function" && typeof b === "function") expect(b.length, key).toBe(a.length);
    }
  });

  it.each([
    [0, "0 peers", "0 пиров", "0 active chats", "0 активных чатов"],
    [1, "1 peer", "1 пир", "1 active chat", "1 активный чат"],
    [3, "3 peers", "3 пира", "3 active chats", "3 активных чата"],
    [11, "11 peers", "11 пиров", "11 active chats", "11 активных чатов"],
    [21, "21 peers", "21 пир", "21 active chats", "21 активный чат"],
    [22, "22 peers", "22 пира", "22 active chats", "22 активных чата"],
    [112, "112 peers", "112 пиров", "112 active chats", "112 активных чатов"],
  ])("pluralize %d", (n, enPeers, ruPeers, enChats, ruChats) => {
    expect([en.status_peers(n), ru.status_peers(n), en.status_active_chats(n), ru.status_active_chats(n)]).toEqual([
      enPeers,
      ruPeers,
      enChats,
      ruChats,
    ]);
  });

  it("fill in arguments", () => {
    for (const table of [en, ru]) {
      expect(table.settings_clear_confirm(3)).toContain("3");
      expect(table.settings_clear_confirm(0)).not.toMatch(/\d/);
      expect(table.toast_receiving("a.txt")).toContain("a.txt");
      expect(table.toast_clear_failed("disk")).toContain("disk");
      expect(table.toast_anonymous_save_failed("relay")).toContain("relay");
      expect(table.backup_word(17)).toContain("17");
      expect(table.settings_version("1.2.3")).toContain("1.2.3");
    }
  });

  it("have no empty strings", () => {
    for (const table of [en, ru]) {
      for (const [key, value] of Object.entries(table)) {
        if (typeof value === "string") expect(value.trim(), key).not.toBe("");
      }
    }
  });
});

describe("locale", () => {
  beforeEach(() => {
    vi.resetModules();
    localStorage.clear();
  });

  it("prefers the saved choice, then the browser language", async () => {
    vi.spyOn(navigator, "language", "get").mockReturnValue("ru-RU");
    let i18n = await import("./index");
    expect(i18n.locale()).toBe("ru");
    expect(i18n.t().nav_home).toBe(ru.nav_home);

    i18n.setLocale("en");
    expect(i18n.t().nav_home).toBe(en.nav_home);
    vi.resetModules();
    i18n = await import("./index");
    expect(i18n.locale()).toBe("en");
  });

  it("falls back to English", async () => {
    vi.spyOn(navigator, "language", "get").mockReturnValue("de-DE");
    localStorage.setItem("cypher-locale", "xx");
    const { locale } = await import("./index");
    expect(locale()).toBe("en");
  });
});

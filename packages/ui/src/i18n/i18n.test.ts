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
    [1, "1 unread", "1 непрочитанное"],
    [3, "3 unread", "3 непрочитанных"],
    [11, "11 unread", "11 непрочитанных"],
    [21, "21 unread", "21 непрочитанное"],
    [112, "112 unread", "112 непрочитанных"],
  ])("pluralize %d", (n, enUnread, ruUnread) => {
    expect([en.chats_unread(n), ru.chats_unread(n)]).toEqual([enUnread, ruUnread]);
  });

  it("fill in arguments", () => {
    for (const table of [en, ru]) {
      expect(table.storage_clear_confirm(3)).toContain("3");
      expect(table.identity_erase(2)).toContain("2");
      expect(table.privacy_bridges_transport(4)).toContain("4");
      expect(table.privacy_bridges_format(5)).toContain("5");
      expect(table.identity_erase(0)).not.toMatch(/\d/);
      expect(table.storage_clear_confirm(0)).not.toMatch(/\d/);
      expect(table.contact_fallback("a1b2c3")).toContain("a1b2c3");
      expect(table.toast_contact_added("bob")).toContain("bob");
      expect(table.invite_share_text("abc-def")).toContain("abc-def");
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
    expect(i18n.t().chats_title).toBe(ru.chats_title);

    i18n.setLocale("en");
    expect(i18n.t().chats_title).toBe(en.chats_title);
    vi.resetModules();
    i18n = await import("./index");
    expect(i18n.locale()).toBe("en");
    expect(document.documentElement.lang).toBe("en");
  });

  it("falls back to English", async () => {
    vi.spyOn(navigator, "language", "get").mockReturnValue("de-DE");
    localStorage.setItem("cypher-locale", "xx");
    const { locale } = await import("./index");
    expect(locale()).toBe("en");
  });
});

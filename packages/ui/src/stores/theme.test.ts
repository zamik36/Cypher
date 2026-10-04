import { beforeEach, describe, expect, it, vi } from "vitest";

const load = () => import("./theme");

describe("theme", () => {
  beforeEach(() => {
    vi.resetModules();
    localStorage.clear();
    document.documentElement.removeAttribute("data-theme");
  });

  it("follows the system unless the user chose", async () => {
    const { resolveTheme } = await load();
    expect(resolveTheme("system", true)).toBe("dark");
    expect(resolveTheme("system", false)).toBe("light");
    expect(resolveTheme("light", true)).toBe("light");
    expect(resolveTheme("dark", false)).toBe("dark");
  });

  it("is applied before anything renders, and the choice persists", async () => {
    const meta = document.createElement("meta");
    meta.name = "theme-color";
    document.head.append(meta);

    const first = await load();
    expect(first.themePref()).toBe("system");
    expect(document.documentElement.dataset["theme"]).toBe(first.theme());

    first.setThemePref("light");
    expect(document.documentElement.dataset["theme"]).toBe("light");
    expect(document.documentElement.style.colorScheme).toBe("light");
    expect(meta.getAttribute("content")).toBe("#f6f7f6");

    vi.resetModules();
    const reloaded = await load();
    expect(reloaded.themePref()).toBe("light");
    expect(document.documentElement.dataset["theme"]).toBe("light");
    meta.remove();
  });

  it("follows the system as it changes", async () => {
    let listener: ((e: { matches: boolean }) => void) | undefined;
    vi.stubGlobal("matchMedia", () => ({
      matches: true,
      addEventListener: (_: string, cb: (e: { matches: boolean }) => void) => (listener = cb),
    }));
    const { theme } = await load();
    expect(theme()).toBe("dark");
    listener?.({ matches: false });
    expect(theme()).toBe("light");
    expect(document.documentElement.dataset["theme"]).toBe("light");
  });

  it("works without storage, for this session only", async () => {
    vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => {
      throw new Error("denied");
    });
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new Error("denied");
    });
    const { setThemePref, themePref } = await load();
    expect(themePref()).toBe("system");
    setThemePref("dark");
    expect(themePref()).toBe("dark");
  });

  it("ignores a saved value it does not know", async () => {
    localStorage.setItem("cypher-theme", "sepia");
    const { themePref } = await load();
    expect(themePref()).toBe("system");
  });
});

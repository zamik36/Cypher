import { createSignal, untrack } from "solid-js";

/** What the user chose; `system` follows the operating system. */
export type ThemePref = "system" | "dark" | "light";
export type Theme = "dark" | "light";

const STORAGE_KEY = "cypher-theme";
/** `--c-bg` of each theme, for the browser and system bars. */
const BAR_COLOR: Record<Theme, string> = { dark: "#0f1113", light: "#f6f7f6" };

function loadPref(): ThemePref {
  try {
    const saved = localStorage.getItem(STORAGE_KEY);
    return saved === "dark" || saved === "light" || saved === "system" ? saved : "system";
  } catch {
    return "system";
  }
}

const systemQuery = typeof matchMedia === "function" ? matchMedia("(prefers-color-scheme: dark)") : null;
const [pref, setPrefSignal] = createSignal<ThemePref>(loadPref());
const [systemDark, setSystemDark] = createSignal(systemQuery?.matches ?? true);
systemQuery?.addEventListener("change", (e) => {
  setSystemDark(e.matches);
  applyTheme();
});

/** The theme `pref` stands for, given whether the system is dark. */
export function resolveTheme(choice: ThemePref, systemIsDark: boolean): Theme {
  if (choice === "system") return systemIsDark ? "dark" : "light";
  return choice;
}

export const themePref = pref;
export const theme = () => resolveTheme(pref(), systemDark());

/** Applies the current theme to the document: tokens, native controls, browser bar. */
export function applyTheme(): void {
  const resolved = theme();
  const root = document.documentElement;
  root.dataset["theme"] = resolved;
  root.style.colorScheme = resolved;
  document.querySelector('meta[name="theme-color"]')?.setAttribute("content", BAR_COLOR[resolved]);
}

export function setThemePref(next: ThemePref): void {
  setPrefSignal(next);
  try {
    localStorage.setItem(STORAGE_KEY, next);
  } catch {
    // Private mode: the choice lasts for this session only.
  }
  applyTheme();
}

// Before the first render, so no screen ever shows the wrong theme.
untrack(applyTheme);

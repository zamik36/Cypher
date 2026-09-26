import { createSignal } from "solid-js";
import en, { type TranslationKeys } from "./en";
import ru from "./ru";

export type Locale = "en" | "ru";

const translations: Record<Locale, TranslationKeys> = { en, ru };

const STORAGE_KEY = "cypher-locale";

function detectLocale(): Locale {
  const saved = localStorage.getItem(STORAGE_KEY);
  if (saved === "en" || saved === "ru") return saved;
  const lang = navigator.language.slice(0, 2);
  return lang === "ru" ? "ru" : "en";
}

const [locale, setLocaleSignal] = createSignal<Locale>(detectLocale());

export function setLocale(l: Locale): void {
  setLocaleSignal(l);
  localStorage.setItem(STORAGE_KEY, l);
}

export function t(): TranslationKeys {
  return translations[locale()];
}

export { locale };

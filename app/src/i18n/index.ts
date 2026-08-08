import { en } from "./locales/en";
import { it } from "./locales/it";
import { fr } from "./locales/fr";
import { es } from "./locales/es";
import { de } from "./locales/de";
import {
  APP_LANGUAGES,
  DEFAULT_LANGUAGE,
  LANGUAGE_LABELS,
  isAppLanguage,
  resolveSystemLanguage,
  type AppLanguage,
  type LanguagePreference,
} from "./languages";

export type { AppLanguage, LanguagePreference };
export { APP_LANGUAGES, DEFAULT_LANGUAGE, LANGUAGE_LABELS, resolveSystemLanguage, isAppLanguage };

const catalogs = { en, it, fr, es, de } as const;
export type MessageKey = keyof typeof en;

const PREF_KEY = "yamete.language";

let current: AppLanguage = DEFAULT_LANGUAGE;

export function getPreference(): LanguagePreference {
  const raw = localStorage.getItem(PREF_KEY);
  if (!raw || raw === "system") return "system";
  return isAppLanguage(raw) ? raw : "system";
}

export function setPreference(pref: LanguagePreference) {
  localStorage.setItem(PREF_KEY, pref);
  applyPreference(pref);
}

export function applyPreference(pref: LanguagePreference = getPreference()) {
  current =
    pref === "system"
      ? resolveSystemLanguage(navigator.language)
      : pref;
  document.documentElement.lang = current;
  applyDom();
}

export function t(key: MessageKey, vars?: Record<string, string | number>): string {
  const table = catalogs[current] ?? catalogs.en;
  let s: string = (table as any)[key] ?? (en as any)[key] ?? key;
  if (vars) {
    for (const [k, v] of Object.entries(vars)) {
      s = s.replaceAll(`{${k}}`, String(v));
    }
  }
  return s;
}

export function applyDom(root: ParentNode = document) {
  root.querySelectorAll<HTMLElement>("[data-i18n]").forEach((el) => {
    const key = el.dataset.i18n as MessageKey | undefined;
    if (!key) return;
    el.textContent = t(key);
  });
  root.querySelectorAll<HTMLElement>("[data-i18n-placeholder]").forEach((el) => {
    const key = el.dataset.i18nPlaceholder as MessageKey | undefined;
    if (!key || !("placeholder" in el)) return;
    (el as HTMLInputElement).placeholder = t(key);
  });
  root.querySelectorAll<HTMLElement>("[data-i18n-title]").forEach((el) => {
    const key = el.dataset.i18nTitle as MessageKey | undefined;
    if (!key) return;
    el.title = t(key);
  });
}

export function currentLanguage(): AppLanguage {
  return current;
}

import { en } from "./en";
import { it } from "./it";
import { fr } from "./fr";
import { es } from "./es";
import { de } from "./de";
import {
  DEFAULT_LOCALE,
  SITE_LOCALES,
  type Locale,
  type Strings,
} from "./types";

export {
  DEFAULT_LOCALE,
  SITE_LOCALES,
  LOCALE_LABELS,
} from "./types";
export type { Locale, Strings } from "./types";

const STRINGS: Record<Locale, Strings> = { en, it, fr, es, de };

export function isLocale(code: string | undefined): code is Locale {
  return !!code && (SITE_LOCALES as readonly string[]).includes(code);
}

export function getStrings(locale: Locale | string | undefined): Strings {
  if (isLocale(locale)) return STRINGS[locale];
  return STRINGS[DEFAULT_LOCALE];
}

/** Locale-aware path. Default locale has no prefix. path without leading slash. */
export function localeUrl(locale: Locale, path = ""): string {
  const prefix = locale === DEFAULT_LOCALE ? "" : `/${locale}`;
  const suffix = path ? `/${path.replace(/^\//, "")}` : "";
  return `${prefix}${suffix}` || "/";
}

export function format(template: string, vars: Record<string, string | number>): string {
  return template.replace(/\{(\w+)\}/g, (_, key: string) => String(vars[key] ?? ""));
}

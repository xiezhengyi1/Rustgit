import { createContext, useContext, useEffect, useMemo, type ReactNode } from "react";
import { en, type MessageKey } from "./en";
import { zh } from "./zh";
import type { LanguageSetting } from "../platform/types";

export type Locale = "en" | "zh";
export type { MessageKey };

const dictionaries: Record<Locale, Record<MessageKey, string>> = { en, zh };

export function resolveLocale(setting: LanguageSetting, systemLanguage?: string): Locale {
  if (setting === "en" || setting === "zh") return setting;
  const sys = (systemLanguage ?? (typeof navigator !== "undefined" ? navigator.language : "en")).toLowerCase();
  return sys.startsWith("zh") ? "zh" : "en";
}

export type Params = Record<string, string | number>;

export function translate(locale: Locale, key: MessageKey, params?: Params): string {
  const template = dictionaries[locale][key] ?? en[key];
  if (!params) return template;
  return template.replace(/\{(\w+)\}/g, (m, name: string) => (name in params ? String(params[name]) : m));
}

export interface I18n {
  locale: Locale;
  /** BCP 47 tag for Intl formatting */
  intlLocale: string;
  t: (key: MessageKey, params?: Params) => string;
}

const I18nContext = createContext<I18n>({
  locale: "en",
  intlLocale: "en",
  t: (k, p) => translate("en", k, p),
});

export function I18nProvider({ locale, children }: { locale: Locale; children: ReactNode }) {
  const value = useMemo<I18n>(
    () => ({ locale, intlLocale: locale === "zh" ? "zh-CN" : "en", t: (k, p) => translate(locale, k, p) }),
    [locale],
  );
  useEffect(() => {
    document.documentElement.lang = locale === "zh" ? "zh-CN" : "en";
  }, [locale]);
  return <I18nContext.Provider value={value}>{children}</I18nContext.Provider>;
}

export function useI18n(): I18n {
  return useContext(I18nContext);
}

import { derived, writable } from 'svelte/store';
import { initialLocale, localeKey, translate, type Locale, type LocaleScope } from './locale.js';
export type { Locale } from './locale.js';
export const uiLocale = writable<Locale>('zh');
export const t = derived(uiLocale, locale => (source: string, ...values: (string | number)[]) => translate(locale, source, ...values));
export const pair = derived(uiLocale, locale => (zh: string, en: string) => locale === 'en' ? en : zh);
export function setLocale(locale: Locale, scope: LocaleScope = 'operator'): void {
  uiLocale.set(locale);
  document.documentElement.lang = locale === 'en' ? 'en' : 'zh-CN';
  try { localStorage.setItem(localeKey(scope), locale); } catch { /* Native settings still persist the operator preference. */ }
}
export function initializeLocale(scope: LocaleScope = 'operator'): Locale {
  let storage: Storage | undefined;
  try { storage = window.localStorage; } catch { /* Use the URL/default. */ }
  const locale = initialLocale(scope, window.location.search, storage);
  setLocale(locale, scope);
  return locale;
}

import { english } from './messages.js';

export type Locale = 'zh' | 'en';
export type LocaleScope = 'operator' | 'wall';
export const localeKey = (scope: LocaleScope) => `forum.interface-language.${scope}`;
export function isLocale(value: unknown): value is Locale { return value === 'zh' || value === 'en'; }

// Only interface strings enter this function. Meeting text and reviewed publications
// must retain their exact content, regardless of the viewer's interface language.
export function translate(locale: Locale, source: string = '', ...values: (string | number)[]): string {
  const template = locale === 'en' && Object.prototype.hasOwnProperty.call(english, source) ? english[source] : source;
  return template.replace(/\{(\d+)\}/g, (token, index) => String(values[Number(index)] ?? token));
}
export function initialLocale(scope: LocaleScope, search: string, storage?: Pick<Storage, 'getItem'>): Locale {
  const query = new URLSearchParams(search).get('lang');
  if (isLocale(query)) return query;
  try { const saved = storage?.getItem(localeKey(scope)); if (isLocale(saved)) return saved; } catch { /* Storage may be unavailable in private browsing. */ }
  return 'zh';
}
export function displayUrlWithLocale(url: string, locale: Locale): string {
  const result = new URL(url);
  result.searchParams.set('lang', locale);
  return result.href;
}

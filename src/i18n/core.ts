import { en, type MessageDictionary, type MessageKey, zhCN } from './messages';

export type Locale = 'zh-CN' | 'en';
export type LocalePreference = 'system' | Locale;
export const LOCALE_STORAGE_KEY = 'logcrate.locale';
const dictionaries: Record<Locale, MessageDictionary> = { en, 'zh-CN': zhCN };

export function systemLocale(languages: readonly string[]): Locale {
  return languages.some((language) => language.toLowerCase().startsWith('zh')) ? 'zh-CN' : 'en';
}

export function resolveLocale(preference: LocalePreference, languages: readonly string[]): Locale {
  return preference === 'system' ? systemLocale(languages) : preference;
}

export function loadLocalePreference(storage: Pick<Storage, 'getItem'>): LocalePreference {
  try {
    const value = storage.getItem(LOCALE_STORAGE_KEY);
    return value === 'zh-CN' || value === 'en' || value === 'system' ? value : 'system';
  } catch {
    return 'system';
  }
}

export function saveLocalePreference(storage: Pick<Storage, 'setItem'>, value: LocalePreference) {
  try {
    storage.setItem(LOCALE_STORAGE_KEY, value);
  } catch {
    /* keep in-memory value */
  }
}

export function translate(
  locale: Locale,
  key: MessageKey,
  params: Record<string, string | number> = {},
): string {
  const template = dictionaries[locale][key] ?? en[key] ?? key;
  return template.replace(/\{(\w+)\}/g, (_, name: string) => String(params[name] ?? `{${name}}`));
}

export function dictionaryKeys(locale: Locale): string[] {
  return Object.keys(dictionaries[locale]).sort();
}

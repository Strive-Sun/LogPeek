import assert from 'node:assert/strict';
import test from 'node:test';
import {
  dictionaryKeys,
  loadLocalePreference,
  resolveLocale,
  saveLocalePreference,
  systemLocale,
  translate,
} from '../i18n/core';
import { localizeKnownError } from '../i18n/errors';
test('resolves Chinese system locales and falls back to English', () => {
  assert.equal(systemLocale(['zh-TW']), 'zh-CN');
  assert.equal(systemLocale(['fr-FR']), 'en');
  assert.equal(resolveLocale('en', ['zh-CN']), 'en');
});
test('persists valid preferences and falls back safely', () => {
  const data = new Map<string, string>();
  const storage = {
    getItem: (k: string) => data.get(k) ?? null,
    setItem: (k: string, v: string) => {
      data.set(k, v);
    },
  };
  saveLocalePreference(storage, 'zh-CN');
  assert.equal(loadLocalePreference(storage), 'zh-CN');
  data.set('logcrate.locale', 'broken');
  assert.equal(loadLocalePreference(storage), 'system');
  assert.equal(
    loadLocalePreference({
      getItem() {
        throw new Error('no storage');
      },
    }),
    'system',
  );
});
test('dictionaries have identical keys and interpolate values', () => {
  assert.deepEqual(dictionaryKeys('en'), dictionaryKeys('zh-CN'));
  assert.equal(translate('en', 'tabs.more', { count: 3 }), 'More (3) ▾');
  assert.equal(translate('zh-CN', 'tabs.more', { count: 3 }), '更多 (3) ▾');
});
test('known backend errors are localized while unknown details remain intact', () => {
  const t = (key: Parameters<typeof translate>[1], params?: Record<string, string | number>) =>
    translate('en', key, params);
  assert.equal(localizeKnownError('文件不存在', t), 'File does not exist');
  assert.equal(localizeKnownError('条目不存在: app.log', t), 'Entry does not exist: app.log');
  assert.equal(localizeKnownError('native failure 42', t), 'native failure 42');
});

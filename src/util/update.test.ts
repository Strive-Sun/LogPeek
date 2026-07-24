import assert from 'node:assert/strict';
import { describe, it } from 'node:test';
import {
  AUTO_CHECK_STORAGE_KEY,
  classifyUpdateCheck,
  downloadPercent,
  errorMessage,
  formatBytes,
  loadAutoCheck,
  loadSkippedVersion,
  saveAutoCheck,
  shouldPromptAutomatically,
  updateFailureMessage,
} from './update';

function storage(values: Record<string, string | null>) {
  return { getItem: (key: string) => values[key] ?? null };
}

describe('更新设置', () => {
  it('自动检查默认为开启，并持久化关闭选择', () => {
    assert.equal(loadAutoCheck(storage({})), true);
    assert.equal(loadAutoCheck(storage({ [AUTO_CHECK_STORAGE_KEY]: 'false' })), false);
  });

  it('本地存储不可用时安全回退', () => {
    const broken = {
      getItem: () => {
        throw new Error('storage disabled');
      },
      setItem: () => {
        throw new Error('storage disabled');
      },
    };
    assert.equal(loadAutoCheck(broken), true);
    assert.equal(loadSkippedVersion(broken), null);
    assert.doesNotThrow(() => saveAutoCheck(broken, false));
  });

  it('忽略空白的跳过版本', () => {
    assert.equal(loadSkippedVersion(storage({})), null);
    assert.equal(loadSkippedVersion(storage({ 'logcrate.update.skippedVersion': '  ' })), null);
  });

  it('只抑制已跳过版本的自动提示', () => {
    const update = { currentVersion: '1.0.1', version: '1.1.0' };
    assert.equal(shouldPromptAutomatically(update, '1.1.0'), false);
    assert.equal(shouldPromptAutomatically({ ...update, version: '1.2.0' }, '1.1.0'), true);
    assert.equal(classifyUpdateCheck(null, false, null), 'up-to-date');
    assert.equal(classifyUpdateCheck(update, true, '1.1.0'), 'skipped');
    assert.equal(classifyUpdateCheck(update, false, '1.1.0'), 'available');
    assert.equal(classifyUpdateCheck({ ...update, version: '1.2.0' }, true, '1.1.0'), 'available');
  });
});

describe('更新状态格式化', () => {
  it('提供可读的错误和字节数', () => {
    assert.equal(errorMessage(new Error('网络不可用')), '网络不可用');
    assert.equal(errorMessage('签名无效'), '签名无效');
    assert.equal(formatBytes(10 * 1024 * 1024), '10.0 MB');
    assert.equal(downloadPercent(5, 10), 50);
    assert.equal(downloadPercent(10, 10), 99);
    assert.equal(downloadPercent(5), undefined);
    assert.equal(updateFailureMessage('downloading', new Error('网络中断')), '网络中断');
    assert.equal(updateFailureMessage('installing', '签名无效'), '签名无效');
  });
});

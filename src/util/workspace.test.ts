import assert from 'node:assert/strict';
import test from 'node:test';
import {
  loadWorkspace,
  mergeSourceChangePrompt,
  removeWorkspaceTabs,
  resolvedSourcePath,
  saveWorkspace,
  sourcePathForEntryKey,
} from './workspace';

class MemoryStorage {
  value: string | null = null;
  getItem() {
    return this.value;
  }
  setItem(_key: string, value: string) {
    this.value = value;
  }
}

test('workspace round-trips tab order and active item without persisting capacity', () => {
  const storage = new MemoryStorage();
  saveWorkspace(storage, {
    visible: ['a.log', 'outer.zip::app.log'],
    overflow: ['b.log'],
    active: 'outer.zip::app.log',
    capacity: 8,
  });
  assert.deepEqual(loadWorkspace(storage, 3), {
    visible: ['a.log', 'outer.zip::app.log'],
    overflow: ['b.log'],
    active: 'outer.zip::app.log',
    capacity: 3,
  });
});

test('workspace rejects corrupt data and filters duplicate or non-restorable tabs', () => {
  const storage = new MemoryStorage();
  storage.value = '{broken';
  assert.deepEqual(loadWorkspace(storage), {
    visible: [],
    overflow: [],
    active: null,
    capacity: 4,
  });

  saveWorkspace(
    storage,
    {
      visible: ['keep.log', 'gone.log', 'keep.log'],
      overflow: ['other.log'],
      active: 'gone.log',
      capacity: 4,
    },
    (id) => id !== 'gone.log',
  );
  assert.deepEqual(loadWorkspace(storage), {
    visible: ['keep.log'],
    overflow: ['other.log'],
    active: 'keep.log',
    capacity: 4,
  });
});

test('deleted tabs are removed while the remaining layout stays valid', () => {
  const next = removeWorkspaceTabs(
    { visible: ['a', 'b'], overflow: ['c'], active: 'b', capacity: 2 },
    new Set(['b']),
  );
  assert.deepEqual(next, { visible: ['a'], overflow: ['c'], active: 'a', capacity: 2 });
  assert.equal(sourcePathForEntryKey('outer.zip::inner.tar::app.log'), 'outer.zip');
  assert.equal(
    resolvedSourcePath('debug.zip::debug.log', 'D:\\downloads\\debug.zip'),
    'D:\\downloads\\debug.zip',
  );
});

test('source prompts deduplicate event storms and deletion supersedes modification', () => {
  const samePath = (left: string, right: string) => left.toLowerCase() === right.toLowerCase();
  let queue = mergeSourceChangePrompt(
    [],
    { sourcePath: 'C:\\Logs\\app.log', kind: 'modified', revision: '1' },
    samePath,
  );
  queue = mergeSourceChangePrompt(
    queue,
    { sourcePath: 'c:\\logs\\APP.log', kind: 'modified', revision: '2' },
    samePath,
  );
  assert.equal(queue.length, 1);
  assert.equal(queue[0].revision, '2');
  queue = mergeSourceChangePrompt(
    queue,
    { sourcePath: 'C:\\Logs\\app.log', kind: 'deleted', entryKey: 'C:\\Logs\\app.log' },
    samePath,
  );
  assert.deepEqual(queue, [
    {
      sourcePath: 'C:\\Logs\\app.log',
      kind: 'deleted',
      entryKey: 'C:\\Logs\\app.log',
    },
  ]);

  queue = mergeSourceChangePrompt(
    queue,
    { sourcePath: 'c:\\logs\\APP.log', kind: 'modified', revision: '3' },
    samePath,
  );
  assert.equal(queue[0].kind, 'deleted');
  assert.equal(queue[0].entryKey, 'C:\\Logs\\app.log');
});

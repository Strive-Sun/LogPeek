import assert from 'node:assert/strict';
import test from 'node:test';
import type { IndexProgress } from '../api/types';
import { IndexProgressStore } from './indexProgress';

function progress(sessionId: string, percent: number, done = false): IndexProgress {
  return {
    sessionId,
    percent,
    indexedLines: done ? 110 : 10,
    done,
    failed: false,
    detectedEncoding: 'UTF-8',
    effectiveEncoding: 'UTF-8',
  };
}

test('已完成的小文件允许 StrictMode 重复订阅并重放终态', () => {
  const store = new IndexProgressStore();
  store.publish(progress('s1', 100, true));

  const first: IndexProgress[] = [];
  store.subscribe('s1', (event) => {
    first.push(event);
    return !event.done;
  })();

  const second: IndexProgress[] = [];
  store.subscribe('s1', (event) => {
    second.push(event);
    return !event.done;
  })();

  assert.equal(first.at(-1)?.done, true);
  assert.equal(second.at(-1)?.done, true);
  assert.equal(second.at(-1)?.indexedLines, 110);
});

test('实时订阅在终态后停止接收，但终态继续提供给后续订阅', () => {
  const store = new IndexProgressStore();
  const live: number[] = [];
  store.subscribe('s2', (event) => {
    live.push(event.percent);
    return !event.done;
  });

  store.publish(progress('s2', 10));
  store.publish(progress('s2', 100, true));
  store.publish(progress('s2', 100, true));

  assert.deepEqual(live, [10, 100]);
  assert.equal(store.getLatest('s2')?.done, true);
});

test('关闭或回收会话后不再重放旧进度', () => {
  const store = new IndexProgressStore();
  store.publish(progress('s3', 100, true));
  store.clear('s3');

  let replayed = false;
  store.subscribe('s3', () => {
    replayed = true;
  })();

  assert.equal(replayed, false);
  assert.equal(store.getLatest('s3'), undefined);
});

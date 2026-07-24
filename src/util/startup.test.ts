import assert from 'node:assert/strict';
import test from 'node:test';
import { beginStartupHandoff, type StartupStage } from '../startup';

test('启动握手在 React 提交和加载层移除后的绘制帧只报告一次可交互状态', async () => {
  const frames: FrameRequestCallback[] = [];
  let timer: (() => void) | undefined;
  let removed = false;
  let done = false;
  const stages: StartupStage[] = [];
  const loader = {
    classList: { add: (name: string) => (done = name === 'done') },
    addEventListener: () => undefined,
    removeEventListener: () => undefined,
    remove: () => (removed = true),
  };

  beginStartupHandoff({
    document: { getElementById: () => loader } as unknown as Document,
    requestFrame: (callback) => {
      frames.push(callback);
      return frames.length;
    },
    cancelFrame: () => undefined,
    setTimer: (callback) => {
      timer = callback;
      return 1 as unknown as ReturnType<typeof setTimeout>;
    },
    clearTimer: () => undefined,
    markStage: async (stage) => {
      stages.push(stage);
    },
  });

  await Promise.resolve();
  assert.deepEqual(stages, ['react-mounted']);
  frames.shift()?.(0);
  frames.shift()?.(0);
  assert.equal(done, true);
  assert.equal(removed, false);
  timer?.();
  assert.equal(removed, true);
  frames.shift()?.(0);
  frames.shift()?.(0);
  await Promise.resolve();
  assert.deepEqual(stages, ['react-mounted', 'interactive-frame']);
});

test('组件在完成交接前卸载时不报告可交互状态', async () => {
  const frames: FrameRequestCallback[] = [];
  const stages: StartupStage[] = [];
  const stop = beginStartupHandoff({
    document: { getElementById: () => null } as unknown as Document,
    requestFrame: (callback) => {
      frames.push(callback);
      return frames.length;
    },
    cancelFrame: () => undefined,
    markStage: async (stage) => {
      stages.push(stage);
    },
  });
  stop();
  frames.forEach((frame) => frame(0));
  await Promise.resolve();
  assert.deepEqual(stages, ['react-mounted']);
});

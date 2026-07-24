import { spawn } from 'node:child_process';
import { existsSync } from 'node:fs';
import { readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { basename, resolve } from 'node:path';

function argument(name, fallback) {
  const index = process.argv.indexOf(`--${name}`);
  return index >= 0 ? process.argv[index + 1] : fallback;
}

const defaultBinary =
  process.platform === 'win32'
    ? 'src-tauri/target/release/logcrate.exe'
    : 'src-tauri/target/release/logcrate';
const binary = resolve(argument('binary', defaultBinary));
const runs = Number.parseInt(argument('runs', '10'), 10);
const timeoutMs = Number.parseInt(argument('timeout', '30000'), 10);

if (!Number.isInteger(runs) || runs < 1) throw new Error('--runs 必须是正整数');
if (!existsSync(binary)) throw new Error(`找不到发布版可执行文件：${binary}`);

const delay = (milliseconds) =>
  new Promise((resolveDelay) => setTimeout(resolveDelay, milliseconds));

async function stopProcess(child) {
  if (child.exitCode !== null) return;
  child.kill();
  await Promise.race([new Promise((resolveExit) => child.once('exit', resolveExit)), delay(3000)]);
}

async function runOnce(index) {
  const tracePath = resolve(tmpdir(), `logcrate-startup-${process.pid}-${index}.json`);
  await rm(tracePath, { force: true });
  const child = spawn(binary, [], {
    env: { ...process.env, LOGCRATE_STARTUP_TRACE: tracePath },
    stdio: 'ignore',
    windowsHide: false,
  });
  const deadline = Date.now() + timeoutMs;
  try {
    while (Date.now() < deadline) {
      if (existsSync(tracePath)) {
        try {
          const snapshot = JSON.parse(await readFile(tracePath, 'utf8'));
          const interactive = snapshot.stages.find((stage) => stage.name === 'interactive-frame');
          if (interactive) return snapshot;
        } catch {
          // The asynchronous writer may still be replacing the trace file.
        }
      }
      if (child.exitCode !== null) throw new Error(`进程提前退出，退出码 ${child.exitCode}`);
      await delay(25);
    }
    throw new Error(`${timeoutMs} ms 内未达到可交互主页面`);
  } finally {
    await stopProcess(child);
    await rm(tracePath, { force: true });
  }
}

function elapsedMilliseconds(snapshot, stageName) {
  const stage = snapshot.stages.find((item) => item.name === stageName);
  return stage ? stage.elapsedMicros / 1000 : undefined;
}

function percentile(values, ratio) {
  const sorted = [...values].sort((left, right) => left - right);
  return sorted[Math.max(0, Math.ceil(sorted.length * ratio) - 1)];
}

const snapshots = [];
for (let index = 0; index < runs; index += 1) {
  const snapshot = await runOnce(index + 1);
  snapshots.push(snapshot);
  const total = elapsedMilliseconds(snapshot, 'interactive-frame');
  console.log(`${index + 1}/${runs}: ${total.toFixed(1)} ms`);
}

const totals = snapshots.map((snapshot) => elapsedMilliseconds(snapshot, 'interactive-frame'));
const stageNames = [
  ...new Set(snapshots.flatMap((snapshot) => snapshot.stages.map((stage) => stage.name))),
];
console.log(`\n${basename(binary)} 启动基准`);
console.log(`P50 ${percentile(totals, 0.5).toFixed(1)} ms`);
console.log(`P90 ${percentile(totals, 0.9).toFixed(1)} ms`);
console.log(`MAX ${Math.max(...totals).toFixed(1)} ms`);
console.log('\n阶段 P50：');
for (const stageName of stageNames) {
  const values = snapshots
    .map((snapshot) => elapsedMilliseconds(snapshot, stageName))
    .filter((value) => value !== undefined);
  if (values.length === snapshots.length) {
    console.log(`${stageName.padEnd(28)} ${percentile(values, 0.5).toFixed(1)} ms`);
  }
}

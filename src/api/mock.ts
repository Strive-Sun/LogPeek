// 浏览器开发用的 mock 后端:模拟目录监控、免解压列条目、建索引、按行读取。
// 与 Tauri 后端实现同一套 API 契约(见 api/index.ts)。

import type {
  AppUpdateInfo,
  AppUpdateProgress,
  ArchiveEntry,
  DirectoryChangeBatch,
  FileRevision,
  DroppedFileInfo,
  EncodingProgress,
  IndexProgress,
  LogLine,
  NewLogItem,
  OpenSessionResult,
  TreeNode,
} from './types';

const LEVELS = ['INFO', 'INFO', 'DEBUG', 'WARN', 'INFO', 'ERROR', 'INFO', 'TRACE'];
const MSGS = [
  'service starting up, binding to 0.0.0.0:8080',
  'loading configuration from /etc/app/config.yaml',
  'established connection to database pool (size=16)',
  'incoming request GET /api/v1/users?page=3 latency=12ms',
  'cache miss for key user:8842, falling back to db',
  'unhandled rejection in worker #3: ETIMEDOUT after 30000ms',
  'flushed 2048 records to segment 0007, wal truncated',
  'retrying upstream call attempt=2 backoff=400ms',
];

function pad(n: number, w: number) {
  return String(n).padStart(w, '0');
}

/** 稳定地为某个条目生成一行日志(纯函数,便于随机访问) */
function genLine(entrySeed: number, lineNo: number): string {
  const t = 10 * 3600 + entrySeed * 17 + lineNo; // 秒
  const hh = pad(Math.floor(t / 3600) % 24, 2);
  const mm = pad(Math.floor(t / 60) % 60, 2);
  const ss = pad(t % 60, 2);
  const lvl = LEVELS[(entrySeed + lineNo) % LEVELS.length];
  const msg = MSGS[(entrySeed * 3 + lineNo) % MSGS.length];
  let line = `2026-07-15 ${hh}:${mm}:${ss} ${lvl.padEnd(5)} [worker-${lineNo % 8}] ${msg} (seq=${lineNo})`;
  // 每隔一段插入一条超长行,演示横向滚动/截断
  if (lineNo % 500 === 0 && lineNo > 0) {
    line += ' ' + 'x'.repeat(2000) + ' <<超长行示例结束>>';
  }
  return line;
}

interface EntryMeta {
  seed: number;
  lineCount: number;
  entry: ArchiveEntry;
  /** 压缩条目需要后台解压建索引 */
  compressed: boolean;
}

// 每个条目路径 → 元信息
const ENTRY_TABLE: Record<string, EntryMeta> = {
  'crash-0715.zip::app.log': {
    seed: 1,
    lineCount: 5_000_000,
    compressed: true,
    entry: {
      path: 'app.log',
      size: 340 * 1024 * 1024,
      isLog: true,
      encrypted: false,
      isArchive: false,
    },
  },
  'crash-0715.zip::sys.log': {
    seed: 2,
    lineCount: 120_000,
    compressed: true,
    entry: {
      path: 'sys.log',
      size: 12 * 1024 * 1024,
      isLog: true,
      encrypted: false,
      isArchive: false,
    },
  },
  'crash-0715.zip::boot.txt': {
    seed: 3,
    lineCount: 8_400,
    compressed: true,
    entry: {
      path: 'boot.txt',
      size: 1.2 * 1024 * 1024,
      isLog: true,
      encrypted: false,
      isArchive: false,
    },
  },
  'crash-0715.zip::core.bin': {
    seed: 4,
    lineCount: 0,
    compressed: true,
    entry: {
      path: 'core.bin',
      size: 88 * 1024 * 1024,
      isLog: false,
      encrypted: false,
      isArchive: false,
    },
  },
  'server.log': {
    seed: 7,
    lineCount: 42_000,
    compressed: false,
    entry: {
      path: 'server.log',
      size: 6 * 1024 * 1024,
      isLog: true,
      encrypted: false,
      isArchive: false,
    },
  },
  'device3.zip::device.log': {
    seed: 9,
    lineCount: 260_000,
    compressed: true,
    entry: {
      path: 'device.log',
      size: 20 * 1024 * 1024,
      isLog: true,
      encrypted: false,
      isArchive: false,
    },
  },
};

const ARCHIVE_ENTRIES: Record<string, string[]> = {
  'crash-0715.zip': ['app.log', 'sys.log', 'boot.txt', 'core.bin'],
  'device3.zip': ['device.log'],
};

const progressTimers = new Map<string, number>();
let encodingGeneration = 0;
const encodingByKey = new Map<string, string>();

export const mockApi = {
  async fileRevision(_path: string): Promise<FileRevision> {
    return { exists: true, revision: 'mock:1' };
  },
  async setAppLocale(_locale: 'zh-CN' | 'en'): Promise<void> {},
  async getAppVersion(): Promise<string> {
    return '1.0.1';
  },

  async checkForUpdate(): Promise<AppUpdateInfo | null> {
    await delay(350);
    return {
      currentVersion: '1.0.1',
      version: '1.1.0',
      date: '2026-07-18T00:00:00Z',
      body: '浏览器 mock：演示新版本下载与安装流程。',
    };
  },

  async downloadAndInstallUpdate(onProgress: (progress: AppUpdateProgress) => void): Promise<void> {
    const totalBytes = 10 * 1024 * 1024;
    for (let percent = 0; percent <= 100; percent += 10) {
      await delay(80);
      onProgress({
        phase: percent === 100 ? 'installing' : 'downloading',
        downloadedBytes: Math.round((totalBytes * percent) / 100),
        totalBytes,
        percent,
      });
    }
  },

  async discardPendingUpdate(): Promise<void> {},

  async listWatchDirs(): Promise<TreeNode[]> {
    return [
      {
        id: 'dir:downloads',
        name: '下载',
        kind: 'dir',
        watchRoot: true,
        children: [
          {
            id: 'arc:crash-0715.zip',
            name: 'crash-0715.zip',
            kind: 'archive',
            size: 96 * 1024 * 1024,
            isLog: true,
            watchDir: '下载',
            unread: true,
          },
          {
            id: 'file:server.log',
            name: 'server.log',
            kind: 'file',
            size: 6 * 1024 * 1024,
            isLog: true,
            watchDir: '下载',
            unread: true,
          },
        ],
      },
      {
        id: 'dir:backup',
        name: '日志备份',
        kind: 'dir',
        watchRoot: true,
        children: [
          {
            id: 'arc:device3.zip',
            name: 'device3.zip',
            kind: 'archive',
            size: 30 * 1024 * 1024,
            isLog: true,
            watchDir: '日志备份',
            unread: true,
          },
        ],
      },
    ];
  },

  async listArchiveEntries(archiveName: string): Promise<ArchiveEntry[]> {
    // 模拟“只读中央目录”的极短延迟
    await delay(120);
    const names = ARCHIVE_ENTRIES[archiveName] ?? [];
    return names.map((n) => ENTRY_TABLE[`${archiveName}::${n}`].entry);
  },

  async expandDirectory(_path: string): Promise<TreeNode[]> {
    return [];
  },

  async collapseDirectory(_path: string): Promise<void> {},

  async newLogItems(): Promise<NewLogItem[]> {
    return [
      {
        id: 'arc:crash-0715.zip',
        name: 'crash-0715.zip',
        kind: 'archive',
        source: '下载',
        age: '2m',
      },
      { id: 'file:server.log', name: 'server.log', kind: 'file', source: '下载', age: '5m' },
      {
        id: 'arc:device3.zip',
        name: 'device3.zip',
        kind: 'archive',
        source: '日志备份',
        age: '10m',
      },
    ];
  },

  async openLogSession(entryKey: string): Promise<OpenSessionResult> {
    const meta = ENTRY_TABLE[entryKey];
    if (!meta) throw new Error(`条目不存在: ${entryKey}`);
    if (!meta.entry.isLog) throw new Error('该条目不是文本日志,无法查看');
    return {
      sessionId: `sess:${entryKey}`,
      sourcePath: entryKey.split('::', 1)[0],
      entryPath: entryKey.replace('::', ' › '),
      size: meta.entry.size,
      indexing: meta.compressed && meta.lineCount > 300_000,
      encoding: 'UTF-8',
      evictedSessionIds: [],
    };
  },

  async closeLogSession(_entryKey: string, _expectedSessionId?: string): Promise<void> {},

  async saveSessionSnapshot(
    entryKey: string,
    _suggestedName: string,
    _title: string,
  ): Promise<{ bytes: number; complete: boolean } | null> {
    const meta = ENTRY_TABLE[entryKey];
    if (!meta) throw new Error(`条目不存在: ${entryKey}`);
    return { bytes: meta.entry.size, complete: true };
  },

  /** 模拟后台建索引进度;返回取消函数 */
  subscribeIndexProgress(
    entryKey: string,
    onProgress: (p: IndexProgress) => void,
    onDone: (totalLines: number) => void,
  ): () => void {
    const meta = ENTRY_TABLE[entryKey];
    const total = meta?.lineCount ?? 0;
    const previousTimer = progressTimers.get(entryKey);
    if (previousTimer) window.clearInterval(previousTimer);
    let percent = 0;
    const timer = window.setInterval(() => {
      percent += 7 + Math.floor(percent / 20);
      if (percent >= 100) {
        percent = 100;
        onProgress({
          sessionId: `sess:${entryKey}`,
          percent,
          indexedLines: total,
          done: true,
          failed: false,
          detectedEncoding: 'UTF-8',
          effectiveEncoding: encodingByKey.get(entryKey) ?? 'UTF-8',
        });
        onDone(total);
        window.clearInterval(timer);
        progressTimers.delete(entryKey);
        return;
      }
      onProgress({
        sessionId: `sess:${entryKey}`,
        percent,
        indexedLines: Math.floor((total * percent) / 100),
        done: false,
        failed: false,
        detectedEncoding: 'UTF-8',
        effectiveEncoding: encodingByKey.get(entryKey) ?? 'UTF-8',
      });
    }, 180);
    progressTimers.set(entryKey, timer);
    return () => {
      window.clearInterval(timer);
      if (progressTimers.get(entryKey) === timer) progressTimers.delete(entryKey);
    };
  },

  async readLines(entryKey: string, start: number, count: number): Promise<LogLine[]> {
    const meta = ENTRY_TABLE[entryKey];
    if (!meta) return [];
    const end = Math.min(start + count, meta.lineCount);
    const out: LogLine[] = [];
    for (let i = start; i < end; i++) {
      const raw = genLine(meta.seed, i);
      const truncated = raw.length > 1024;
      out.push({
        lineNo: i + 1,
        content: truncated ? raw.slice(0, 1024) : raw,
        truncated,
      });
    }
    return out;
  },

  lineCount(entryKey: string): number {
    return ENTRY_TABLE[entryKey]?.lineCount ?? 0;
  },

  async setSessionEncoding(entryKey: string, encoding: string): Promise<number> {
    encodingByKey.set(entryKey, encoding);
    encodingGeneration += 1;
    return encodingGeneration;
  },

  subscribeEncodingProgress(
    entryKey: string,
    generation: number,
    onProgress: (progress: EncodingProgress) => void,
  ): () => void {
    const timer = window.setTimeout(() => {
      onProgress({
        sessionId: `sess:${entryKey}`,
        generation,
        percent: 100,
        encoding: encodingByKey.get(entryKey) ?? 'UTF-8',
        lineCount: ENTRY_TABLE[entryKey]?.lineCount ?? 0,
        done: true,
        failed: false,
      });
    }, 120);
    return () => window.clearTimeout(timer);
  },

  async addWatchDir(_title?: string): Promise<boolean> {
    throw new Error('mock.selectDirectory');
  },

  async inspectDroppedFile(_path: string): Promise<DroppedFileInfo> {
    throw new Error('mock.dropUnsupported');
  },

  async addWatchPath(_path: string): Promise<void> {},

  async removeWatchDir(_dirPath: string): Promise<void> {},

  async renameFile(path: string, newName: string): Promise<string> {
    const parent = path.replace(/[/\\][^/\\]*$/, '');
    return `${parent}/${newName}`;
  },

  async deleteFile(_path: string): Promise<void> {},

  async openPath(_path: string): Promise<void> {
    throw new Error('mock.fileManager');
  },

  async renameWatchDir(path: string, newName: string): Promise<string> {
    const parent = path.replace(/[/\\][^/\\]*$/, '');
    return `${parent}/${newName}`;
  },

  async deleteWatchDir(_path: string): Promise<void> {},

  async setFilter(_suffixes: string[], _showAll: boolean): Promise<void> {},

  async getFilter(): Promise<[string[], boolean]> {
    return [['.log', '.txt', '.out'], false];
  },

  subscribeNewLogs(_onDetect: (item: NewLogItem) => void): () => void {
    return () => {};
  },

  subscribeDirectoryChanges(_onChange: (batch: DirectoryChangeBatch) => void): () => void {
    return () => {};
  },
};

function delay(ms: number) {
  return new Promise((r) => setTimeout(r, ms));
}

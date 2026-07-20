// 真实 Tauri 后端适配层:暴露与 mockApi 完全相同的方法签名,
// 内部调用 invoke 并把绝对路径等细节隐藏起来,组件层无需改动。

import { invoke } from '@tauri-apps/api/core';
import { getVersion } from '@tauri-apps/api/app';
import { listen } from '@tauri-apps/api/event';
import { open as openDialog } from '@tauri-apps/plugin-dialog';
import { relaunch } from '@tauri-apps/plugin-process';
import { check, type Update } from '@tauri-apps/plugin-updater';
import { IndexProgressStore } from '../util/indexProgress';
import { downloadPercent, updateFailureMessage } from '../util/update';
import type {
  AppUpdateInfo,
  AppUpdateProgress,
  ArchiveEntry,
  DetectedItem,
  DirectoryChangeBatch,
  DroppedFileInfo,
  EncodingProgress,
  IndexProgress,
  LogLine,
  NewLogItem,
  OpenSessionResult,
  TreeNode,
} from './types';

// name → 绝对路径 的映射(list_watch_dirs / new items 时填充)
const pathByName = new Map<string, string>();
// entryKey("archiveName::entry" | "fileName") → sessionId
const sessionByKey = new Map<string, string>();
const totalByKey = new Map<string, number>();
const indexProgress = new IndexProgressStore();
const latestEncodingProgress = new Map<string, EncodingProgress>();
const encodingSubscribers = new Map<string, Set<(progress: EncodingProgress) => void>>();
let pendingUpdate: Update | null = null;
let sessionOpenQueue: Promise<void> = Promise.resolve();

// Start listening before any session opens so fast jobs can be replayed to late subscribers.
const progressListenerReady = listen<IndexProgress>('index-progress', (event) => {
  indexProgress.publish(event.payload);
});
const encodingListenerReady = listen<EncodingProgress>('encoding-progress', (event) => {
  const progress = event.payload;
  latestEncodingProgress.set(progress.sessionId, progress);
  encodingSubscribers.get(progress.sessionId)?.forEach((subscriber) => subscriber(progress));
});

interface RawChild {
  id: string;
  name: string;
  kind: 'dir' | 'archive' | 'file';
  path: string;
  size: number;
  isLog: boolean;
  source: string;
  unread: boolean;
}
type RawDetectedItem = Omit<RawChild, 'id' | 'unread'> & { id?: string; unread?: boolean };
interface RawDir {
  id: string;
  name: string;
  kind: 'dir';
  path: string;
  children: RawChild[];
}

type RawDirectoryChange =
  | { type: 'upsert'; node: RawDetectedItem }
  | { type: 'remove'; path: string }
  | { type: 'rename'; oldPath: string; node: RawDetectedItem }
  | { type: 'rescan'; nodes: RawDetectedItem[] };

interface RawDirectoryChangeBatch {
  watchDir: string;
  changes: RawDirectoryChange[];
}

function treeNode(raw: RawDetectedItem): TreeNode {
  pathByName.set(raw.name, raw.path);
  return {
    id: raw.path,
    name: raw.name,
    kind: raw.kind,
    path: raw.path,
    size: raw.size,
    isLog: raw.isLog,
    watchDir: raw.source,
    unread: raw.unread ?? false,
  };
}

export const tauriApi = {
  async setAppLocale(locale: 'zh-CN' | 'en'): Promise<void> {
    await invoke('set_app_locale', { locale });
  },
  getAppVersion(): Promise<string> {
    return getVersion();
  },

  async checkForUpdate(): Promise<AppUpdateInfo | null> {
    if (pendingUpdate) {
      const previousUpdate = pendingUpdate;
      pendingUpdate = null;
      await previousUpdate.close();
    }
    const update = await check({ timeout: 15_000 });
    if (!update) return null;
    pendingUpdate = update;
    return {
      currentVersion: update.currentVersion,
      version: update.version,
      date: update.date,
      body: update.body,
    };
  },

  async downloadAndInstallUpdate(onProgress: (progress: AppUpdateProgress) => void): Promise<void> {
    const update = pendingUpdate;
    if (!update) throw new Error('update.none');

    let downloadedBytes = 0;
    let totalBytes: number | undefined;
    const progressState: { phase: AppUpdateProgress['phase'] } = { phase: 'downloading' };
    try {
      await update.downloadAndInstall(
        (event) => {
          if (event.event === 'Started') {
            totalBytes = event.data.contentLength;
            onProgress({
              phase: progressState.phase,
              downloadedBytes,
              totalBytes,
              percent: totalBytes ? 0 : undefined,
            });
          } else if (event.event === 'Progress') {
            downloadedBytes += event.data.chunkLength;
            onProgress({
              phase: progressState.phase,
              downloadedBytes,
              totalBytes,
              percent: downloadPercent(downloadedBytes, totalBytes),
            });
          } else {
            progressState.phase = 'installing';
            onProgress({ phase: progressState.phase, downloadedBytes, totalBytes, percent: 100 });
          }
        },
        { timeout: 5 * 60_000 },
      );
      pendingUpdate = null;
      await relaunch();
    } catch (error) {
      pendingUpdate = null;
      await update.close().catch(() => undefined);
      throw new Error(updateFailureMessage(progressState.phase, error));
    }
  },

  async discardPendingUpdate(): Promise<void> {
    const update = pendingUpdate;
    pendingUpdate = null;
    if (update) await update.close();
  },

  async listWatchDirs(): Promise<TreeNode[]> {
    const dirs = await invoke<RawDir[]>('list_watch_dirs');
    return dirs.map((d) => ({
      id: d.id,
      name: d.name,
      kind: 'dir' as const,
      path: d.path,
      watchRoot: true,
      children: d.children.map((c) => ({ ...treeNode(c), watchDir: d.name })),
    }));
  },

  async listArchiveEntries(archiveName: string): Promise<ArchiveEntry[]> {
    const path = pathByName.get(archiveName) ?? archiveName;
    return invoke<ArchiveEntry[]>('list_archive_entries', { path });
  },

  async expandDirectory(path: string): Promise<TreeNode[]> {
    const nodes = await invoke<RawChild[]>('expand_directory', { path });
    return nodes.map(treeNode);
  },

  async collapseDirectory(path: string): Promise<void> {
    await invoke('collapse_directory', { path });
  },

  async newLogItems(): Promise<NewLogItem[]> {
    // 到达通知由事件推送(见 subscribeNewLogs);初始为空
    return [];
  },

  async openLogSession(entryKey: string): Promise<OpenSessionResult> {
    let releaseQueue!: () => void;
    const previousOpen = sessionOpenQueue;
    sessionOpenQueue = new Promise<void>((resolve) => {
      releaseQueue = resolve;
    });
    await previousOpen;
    try {
      await progressListenerReady;
      const [archiveName, entry] = entryKey.includes('::')
        ? entryKey.split('::')
        : [entryKey, entryKey.split(/[/\\]/).pop() ?? entryKey];
      const archivePath = pathByName.get(archiveName) ?? archiveName;
      const entryPath = entry || archiveName;
      const res = await invoke<OpenSessionResult>('open_log_session', {
        archivePath,
        entryPath,
      });
      for (const evictedSessionId of res.evictedSessionIds) {
        for (const [key, sessionId] of sessionByKey) {
          if (sessionId !== evictedSessionId) continue;
          sessionByKey.delete(key);
          totalByKey.delete(key);
        }
        indexProgress.clear(evictedSessionId);
        encodingSubscribers.delete(evictedSessionId);
        latestEncodingProgress.delete(evictedSessionId);
      }
      sessionByKey.set(entryKey, res.sessionId);
      const total = await invoke<number>('line_count', { sessionId: res.sessionId });
      totalByKey.set(entryKey, indexProgress.getLatest(res.sessionId)?.indexedLines ?? total);
      return res;
    } finally {
      releaseQueue();
    }
  },

  async closeLogSession(entryKey: string, expectedSessionId?: string): Promise<void> {
    const currentSessionId = sessionByKey.get(entryKey);
    const sessionId = expectedSessionId ?? currentSessionId;
    if (!expectedSessionId || currentSessionId === expectedSessionId) {
      sessionByKey.delete(entryKey);
      totalByKey.delete(entryKey);
    }
    if (!sessionId) return;
    indexProgress.clear(sessionId);
    encodingSubscribers.delete(sessionId);
    latestEncodingProgress.delete(sessionId);
    await invoke('close_log_session', { sessionId });
  },

  subscribeIndexProgress(
    entryKey: string,
    onProgress: (p: IndexProgress) => void,
    onDone: (totalLines: number) => void,
  ): () => void {
    // Dispatch live events, or replay the latest event when indexing finished very quickly.
    const sessionId = sessionByKey.get(entryKey);
    if (!sessionId) return () => {};
    let finished = false;
    const unsubscribe = indexProgress.subscribe(sessionId, (progress) => {
      if (finished) return false;
      totalByKey.set(entryKey, progress.indexedLines);
      onProgress(progress);
      if (progress.done) {
        finished = true;
        onDone(progress.indexedLines);
        return false;
      }
      return true;
    });
    return () => {
      finished = true;
      unsubscribe();
    };
  },

  async readLines(entryKey: string, start: number, count: number): Promise<LogLine[]> {
    const sid = sessionByKey.get(entryKey);
    if (!sid) return [];
    return invoke<LogLine[]>('read_lines', { sessionId: sid, start, count });
  },

  lineCount(entryKey: string): number {
    return totalByKey.get(entryKey) ?? 0;
  },

  async setSessionEncoding(entryKey: string, encoding: string): Promise<number> {
    await encodingListenerReady;
    const sessionId = sessionByKey.get(entryKey);
    if (!sessionId) throw new Error('session not found');
    latestEncodingProgress.delete(sessionId);
    return invoke<number>('set_session_encoding', { sessionId, encoding });
  },

  subscribeEncodingProgress(
    entryKey: string,
    generation: number,
    onProgress: (progress: EncodingProgress) => void,
  ): () => void {
    const sessionId = sessionByKey.get(entryKey);
    if (!sessionId) return () => {};
    let finished = false;
    const subscriber = (progress: EncodingProgress) => {
      if (finished || progress.generation !== generation) return;
      onProgress(progress);
      if (progress.done) {
        finished = true;
        encodingSubscribers.get(sessionId)?.delete(subscriber);
        latestEncodingProgress.delete(sessionId);
      }
    };
    const subscribers = encodingSubscribers.get(sessionId) ?? new Set();
    subscribers.add(subscriber);
    encodingSubscribers.set(sessionId, subscribers);
    const latest = latestEncodingProgress.get(sessionId);
    if (latest) subscriber(latest);
    return () => {
      finished = true;
      const current = encodingSubscribers.get(sessionId);
      current?.delete(subscriber);
      if (current?.size === 0) encodingSubscribers.delete(sessionId);
    };
  },

  /** 弹出文件夹选择器,添加监控目录;返回是否添加成功 */
  async addWatchDir(title?: string): Promise<boolean> {
    const dir = await openDialog({ directory: true, multiple: false, title });
    if (!dir || typeof dir !== 'string') return false;
    await invoke('add_watch_dir', { path: dir });
    return true;
  },

  async inspectDroppedFile(path: string): Promise<DroppedFileInfo> {
    return invoke<DroppedFileInfo>('inspect_dropped_file', { path });
  },

  async addWatchPath(path: string): Promise<void> {
    await invoke('add_watch_dir', { path });
  },

  async removeWatchDir(dirPath: string): Promise<void> {
    await invoke('remove_watch_dir', { path: dirPath });
  },

  /** 重命名磁盘文件(同目录内);返回新路径 */
  async renameFile(path: string, newName: string): Promise<string> {
    return invoke<string>('rename_file', { path, newName });
  },

  /** 删除文件到系统回收站 */
  async deleteFile(path: string): Promise<void> {
    await invoke('delete_file', { path });
  },

  /** 在系统文件管理器中打开/定位路径 */
  async openPath(path: string): Promise<void> {
    await invoke('open_path', { path });
  },

  /** 重命名监控目录(磁盘 + 配置);返回新路径 */
  async renameWatchDir(path: string, newName: string): Promise<string> {
    return invoke<string>('rename_watch_dir', { path, newName });
  },

  /** 删除监控目录到回收站并移除监控 */
  async deleteWatchDir(path: string): Promise<void> {
    await invoke('delete_watch_dir', { path });
  },

  async setFilter(suffixes: string[], showAll: boolean): Promise<void> {
    await invoke('set_filter', { suffixes, showAll });
  },

  /** 读取持久化的后缀筛选配置,供启动时同步 */
  async getFilter(): Promise<[string[], boolean]> {
    return invoke<[string[], boolean]>('get_filter');
  },

  /** 订阅到达事件;返回取消函数 */
  subscribeNewLogs(onDetect: (item: NewLogItem) => void): () => void {
    const un = listen<DetectedItem>('new-archive-detected', (e) => {
      const it = e.payload;
      pathByName.set(it.name, it.path);
      onDetect({
        id: it.path,
        name: it.name,
        kind: it.kind,
        source: it.source,
        age: 'now',
      });
    });
    return () => {
      un.then((f) => f());
    };
  },

  /** 订阅目录结构变化；新日志提示通过独立事件处理。 */
  subscribeDirectoryChanges(onChange: (batch: DirectoryChangeBatch) => void): () => void {
    const un = listen<RawDirectoryChangeBatch>('directory-changed', (event) => {
      const batch = event.payload;
      const changes = batch.changes.map((change) => {
        if (change.type === 'upsert') return { ...change, node: treeNode(change.node) };
        if (change.type === 'rename') {
          for (const [name, path] of pathByName) {
            if (path === change.oldPath) pathByName.delete(name);
          }
          return { ...change, node: treeNode(change.node) };
        }
        if (change.type === 'remove') {
          for (const [name, path] of pathByName) {
            if (path === change.path) pathByName.delete(name);
          }
          return change;
        }
        return { ...change, nodes: change.nodes.map(treeNode) };
      }) as DirectoryChangeBatch['changes'];
      onChange({ watchDir: batch.watchDir, changes });
    });
    return () => {
      un.then((f) => f());
    };
  },
};

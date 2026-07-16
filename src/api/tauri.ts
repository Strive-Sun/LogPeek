// 真实 Tauri 后端适配层:暴露与 mockApi 完全相同的方法签名,
// 内部调用 invoke 并把绝对路径等细节隐藏起来,组件层无需改动。

import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { open as openDialog } from '@tauri-apps/plugin-dialog';
import type {
  ArchiveEntry,
  DetectedItem,
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

interface RawChild {
  id: string;
  name: string;
  kind: 'archive' | 'file';
  path: string;
  size: number;
  isLog: boolean;
  source: string;
  unread: boolean;
}
interface RawDir {
  id: string;
  name: string;
  kind: 'dir';
  path: string;
  children: RawChild[];
}

export const tauriApi = {
  async listWatchDirs(): Promise<TreeNode[]> {
    const dirs = await invoke<RawDir[]>('list_watch_dirs');
    return dirs.map((d) => ({
      id: d.id,
      name: d.name,
      kind: 'dir' as const,
      children: d.children.map((c) => {
        pathByName.set(c.name, c.path);
        return {
          id: c.id,
          name: c.name,
          kind: c.kind,
          size: c.size,
          isLog: c.isLog,
          watchDir: d.name,
          unread: c.unread,
        };
      }),
    }));
  },

  async listArchiveEntries(archiveName: string): Promise<ArchiveEntry[]> {
    const path = pathByName.get(archiveName) ?? archiveName;
    return invoke<ArchiveEntry[]>('list_archive_entries', { path });
  },

  async newLogItems(): Promise<NewLogItem[]> {
    // 到达通知由事件推送(见 subscribeNewLogs);初始为空
    return [];
  },

  async openLogSession(entryKey: string): Promise<OpenSessionResult> {
    const [archiveName, entry] = entryKey.includes('::')
      ? entryKey.split('::')
      : [entryKey, entryKey];
    const archivePath = pathByName.get(archiveName) ?? archiveName;
    const entryPath = entry || archiveName;
    const res = await invoke<OpenSessionResult>('open_log_session', {
      archivePath,
      entryPath,
    });
    sessionByKey.set(entryKey, res.sessionId);
    const total = await invoke<number>('line_count', { sessionId: res.sessionId });
    totalByKey.set(entryKey, total);
    return res;
  },

  subscribeIndexProgress(
    entryKey: string,
    _onProgress: (p: IndexProgress) => void,
    onDone: (totalLines: number) => void,
  ): () => void {
    // 真实后端 open 返回时索引已建好,直接完成
    onDone(totalByKey.get(entryKey) ?? 0);
    return () => {};
  },

  async readLines(entryKey: string, start: number, count: number): Promise<LogLine[]> {
    const sid = sessionByKey.get(entryKey);
    if (!sid) return [];
    return invoke<LogLine[]>('read_lines', { sessionId: sid, start, count });
  },

  lineCount(entryKey: string): number {
    return totalByKey.get(entryKey) ?? 0;
  },

  /** 弹出文件夹选择器,添加监控目录;返回是否添加成功 */
  async addWatchDir(): Promise<boolean> {
    const dir = await openDialog({ directory: true, multiple: false, title: '选择监控目录' });
    if (!dir || typeof dir !== 'string') return false;
    await invoke('add_watch_dir', { path: dir });
    return true;
  },

  async removeWatchDir(dirPath: string): Promise<void> {
    await invoke('remove_watch_dir', { path: dirPath });
  },

  async setFilter(suffixes: string[], showAll: boolean): Promise<void> {
    await invoke('set_filter', { suffixes, showAll });
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
};

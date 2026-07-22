import { useCallback, useEffect, useRef, useState } from 'react';
import { getCurrentWebview } from '@tauri-apps/api/webview';
import { api, isTauri } from './api';
import type {
  AppUpdateInfo,
  AppUpdateProgress,
  DroppedFileInfo,
  NewLogItem,
  OpenSessionResult,
  TreeNode,
} from './api';
import { TopBar } from './components/TopBar';
import { UpdateDialog } from './components/UpdateDialog';
import { ConfirmDialog } from './components/ConfirmDialog';
import { DirTree } from './components/DirTree';
import { LogContent } from './components/LogContent';
import { LogTabs, type LogTabItem } from './components/LogTabs';
import { EmptyState } from './components/EmptyState';
import {
  classifyUpdateCheck,
  errorMessage,
  loadAutoCheck,
  loadSkippedVersion,
  saveAutoCheck,
  saveSkippedVersion,
  type UpdateStatus,
} from './util/update';
import {
  applyDirectoryChanges,
  findTreeNode,
  isPathInsideDirectory,
  passesDirectoryFilter,
  revealDirectoryChain,
  removedDirectoryNodes,
  sameFilePath,
} from './util/directoryTree';
import { planFileDrop, singleDroppedPath } from './util/fileDrop';
import { installAutoHideScrollbars } from './util/autoHideScrollbars';
import { useI18n } from './i18n/I18nProvider';
import { localizeKnownError } from './i18n/errors';
import {
  activateTab,
  closeTab,
  markEvictedSessions,
  openTab,
  resizeTabs,
  tabIds,
} from './util/logTabs';
import {
  loadWorkspace,
  mergeSourceChangePrompt,
  removeWorkspaceTabs,
  resolvedSourcePath,
  saveWorkspace,
  sourcePathForEntryKey,
  type SourceChangePrompt,
} from './util/workspace';

function flattenNodes(nodes: readonly TreeNode[]): TreeNode[] {
  return nodes.flatMap((node) => [node, ...flattenNodes(node.children ?? [])]);
}

interface ConfirmationRequest {
  title: string;
  message: string;
  confirmLabel: string;
  cancelLabel?: string;
  showCancel?: boolean;
  danger?: boolean;
  action: () => Promise<void>;
}

interface LogTab extends LogTabItem {
  session: OpenSessionResult | null;
  sourcePath?: string;
  error?: string;
  sourceRevision?: string;
  sourceState: 'current' | 'deleted';
}

function tabTitle(entryKey: string): string {
  const target = entryKey.includes('::') ? entryKey.split('::').slice(1).join('::') : entryKey;
  return target.split(/[/\\]/).pop() ?? target;
}

function tabContainerPath(entryKey: string): string {
  return entryKey.includes('::') ? entryKey.split('::')[0] : entryKey;
}

function sourceName(sourcePath: string): string {
  return sourcePath.split(/[/\\]/).pop() ?? sourcePath;
}

function tabSourcePath(entryKey: string, tab?: LogTab): string {
  return resolvedSourcePath(entryKey, tab?.sourcePath ?? tab?.session?.sourcePath);
}

export function App() {
  const { locale, t } = useI18n();
  const localizedError = useCallback(
    (error: unknown) => {
      const message = errorMessage(error);
      const known = {
        'fileDrop.single': 'error.singleDrop',
        'update.none': 'error.noUpdate',
        'mock.selectDirectory': 'mock.selectDirectory',
        'mock.dropUnsupported': 'mock.dropUnsupported',
        'mock.fileManager': 'mock.fileManager',
      } as const;
      return message in known
        ? t(known[message as keyof typeof known])
        : localizeKnownError(message, t);
    },
    [t],
  );
  const [theme, setTheme] = useState<'dark' | 'light'>('light');
  const [appVersion, setAppVersion] = useState('…');
  const [autoCheckUpdates, setAutoCheckUpdates] = useState(() => loadAutoCheck(localStorage));
  const [skippedVersion, setSkippedVersion] = useState(() => loadSkippedVersion(localStorage));
  const [updateStatus, setUpdateStatus] = useState<UpdateStatus>('idle');
  const [updateInfo, setUpdateInfo] = useState<AppUpdateInfo | null>(null);
  const [updateProgress, setUpdateProgress] = useState<AppUpdateProgress | null>(null);
  const [updateError, setUpdateError] = useState<string | null>(null);
  const [updatePromptOpen, setUpdatePromptOpen] = useState(false);
  const [confirmation, setConfirmation] = useState<ConfirmationRequest | null>(null);
  const confirmationRef = useRef<ConfirmationRequest | null>(null);
  const updatePromptOpenRef = useRef(false);
  const sourcePromptOpenRef = useRef(false);
  const dropBusy = useRef(false);
  const updateTaskRunning = useRef(false);
  const autoCheckStarted = useRef(false);
  const [tree, setTree] = useState<TreeNode[]>([]);
  const treeRef = useRef<TreeNode[]>([]);
  const [newItems, setNewItems] = useState<NewLogItem[]>([]);
  // 徽章数字直接由未读列表长度派生,保证徽章与列表始终一致
  const count = newItems.length;
  // 未读项 id 集合(id 即文件路径),用于左树高亮;不依赖后端 unread 标记
  const unreadIds = new Set(newItems.map((it) => it.id));
  const seen = useRef<Set<string>>(new Set());
  // 当前选中的压缩包(用于左侧树高亮)与当前查看的条目 key
  const [selectedArchive, setSelectedArchive] = useState<string | null>(null);
  const selectedArchiveRef = useRef<string | null>(null);
  const [revealedTarget, setRevealedTarget] = useState<{
    path: string;
    directories: string[];
  } | null>(null);
  const [initialWorkspace] = useState(() => loadWorkspace(localStorage, 4));
  const [tabs, setTabs] = useState<Record<string, LogTab>>(() =>
    Object.fromEntries(
      tabIds(initialWorkspace).map((id) => [
        id,
        {
          id,
          title: tabTitle(id),
          absolutePath: id.split('::').join(' › '),
          status: 'dormant' as const,
          session: null,
          sourceState: 'current' as const,
        },
      ]),
    ),
  );
  const tabsRef = useRef<Record<string, LogTab>>(tabs);
  const [tabLayout, setTabLayout] = useState(() => initialWorkspace);
  const tabLayoutRef = useRef(tabLayout);
  const tabOpenGeneration = useRef(new Map<string, number>());
  const potentialSourceChangeRef = useRef<(path: string) => void>(() => {});
  const deletedSourceRef = useRef<(path: string, subtree?: boolean) => void>(() => {});
  const selfDeletedSources = useRef<Array<{ path: string; subtree: boolean }>>([]);
  const pendingRevisionChecks = useRef(new Set<string>());
  const restoredWorkspaceStarted = useRef(false);
  const [sourcePrompts, setSourcePrompts] = useState<SourceChangePrompt[]>([]);
  const activeKey = tabLayout.active;
  const activeSourcePath = activeKey ? tabSourcePath(activeKey, tabs[activeKey]) : null;
  const activeSourceState = activeKey ? tabs[activeKey]?.sourceState : undefined;

  const updateTabLayout = useCallback(
    (updater: (current: typeof tabLayout) => typeof tabLayout) => {
      const next = updater(tabLayoutRef.current);
      tabLayoutRef.current = next;
      setTabLayout(next);
      return next;
    },
    [],
  );

  // 引用稳定,避免 LogTabs 的 ResizeObserver effect 因回调变化而反复重跑
  const handleCapacityChange = useCallback(
    (capacity: number) => updateTabLayout((layout) => resizeTabs(layout, capacity)),
    [updateTabLayout],
  );

  // 后缀筛选
  const [filter, setFilter] = useState<string[]>(['.log', '.txt', '.out']);
  const [showAll, setShowAll] = useState(false);
  // 用户一旦本地修改筛选,忽略启动时异步返回的旧配置,避免覆盖新值
  const filterEdited = useRef(false);

  // 左栏宽度(可拖动调整),持久化到 localStorage
  const [treeWidth, setTreeWidth] = useState<number>(() => {
    // Legacy key is intentionally retained so LogPeek users keep their layout after rebranding.
    const saved = Number(localStorage.getItem('logpeek.treeWidth'));
    return saved >= 160 && saved <= 720 ? saved : 300;
  });

  const startResize = useCallback(
    (e: React.MouseEvent) => {
      e.preventDefault();
      const startX = e.clientX;
      const startW = treeWidth;
      const onMove = (ev: MouseEvent) => {
        const w = Math.min(720, Math.max(160, startW + ev.clientX - startX));
        setTreeWidth(w);
      };
      const onUp = () => {
        document.removeEventListener('mousemove', onMove);
        document.removeEventListener('mouseup', onUp);
        document.body.classList.remove('resizing');
      };
      document.addEventListener('mousemove', onMove);
      document.addEventListener('mouseup', onUp);
      document.body.classList.add('resizing');
    },
    [treeWidth],
  );

  const checkForUpdates = useCallback(
    async (automatic: boolean) => {
      if (updateTaskRunning.current) return;
      updateTaskRunning.current = true;
      setUpdateStatus('checking');
      setUpdateError(null);
      setUpdateProgress(null);
      try {
        const update = await api.checkForUpdate();
        const outcome = classifyUpdateCheck(update, automatic, skippedVersion);
        if (outcome === 'up-to-date') {
          setUpdateInfo(null);
          setUpdateStatus(automatic ? 'idle' : 'up-to-date');
          return;
        }
        if (outcome === 'skipped') {
          await api.discardPendingUpdate();
          setUpdateInfo(null);
          setUpdateStatus('idle');
          return;
        }
        if (!update) return;
        setUpdateInfo(update);
        setUpdateStatus('available');
        if (automatic) setUpdatePromptOpen(true);
      } catch (error) {
        setUpdateInfo(null);
        if (automatic) {
          setUpdateStatus('idle');
        } else {
          setUpdateError(errorMessage(error));
          setUpdateStatus('error');
        }
      } finally {
        updateTaskRunning.current = false;
      }
    },
    [skippedVersion],
  );

  const changeAutoCheckUpdates = useCallback((enabled: boolean) => {
    setAutoCheckUpdates(enabled);
    saveAutoCheck(localStorage, enabled);
  }, []);

  const skipUpdate = useCallback(() => {
    if (updateInfo) {
      saveSkippedVersion(localStorage, updateInfo.version);
      setSkippedVersion(updateInfo.version);
    }
    setUpdatePromptOpen(false);
    setUpdateInfo(null);
    setUpdateStatus('idle');
    setUpdateProgress(null);
    void api.discardPendingUpdate().catch(() => undefined);
  }, [updateInfo]);

  const downloadUpdate = useCallback(async () => {
    if (updateTaskRunning.current || !updateInfo) return;
    updateTaskRunning.current = true;
    setUpdatePromptOpen(false);
    setUpdateError(null);
    setUpdateStatus('downloading');
    setUpdateProgress({ phase: 'downloading', downloadedBytes: 0 });
    try {
      await api.downloadAndInstallUpdate((progress) => {
        setUpdateProgress(progress);
        setUpdateStatus(progress.phase);
      });
      setUpdateStatus('installed');
    } catch (error) {
      setUpdateError(errorMessage(error));
      setUpdateStatus('error');
    } finally {
      updateTaskRunning.current = false;
    }
  }, [updateInfo]);

  useEffect(() => {
    api
      .getAppVersion()
      .then(setAppVersion)
      .catch(() => setAppVersion(t('common.unknown')));
  }, [t]);

  useEffect(() => {
    if (autoCheckStarted.current) return;
    autoCheckStarted.current = true;
    if (autoCheckUpdates) void checkForUpdates(true);
  }, [autoCheckUpdates, checkForUpdates]);

  useEffect(() => {
    localStorage.setItem('logpeek.treeWidth', String(treeWidth));
  }, [treeWidth]);

  useEffect(() => {
    document.documentElement.dataset.theme = theme;
  }, [theme]);

  useEffect(() => installAutoHideScrollbars(document), []);

  useEffect(() => {
    void api.setAppLocale(locale).catch(() => undefined);
  }, [locale]);

  useEffect(() => {
    tabsRef.current = tabs;
  }, [tabs]);

  useEffect(() => {
    saveWorkspace(localStorage, tabLayout, (entryKey) => tabs[entryKey]?.sourceState !== 'deleted');
  }, [tabLayout, tabs]);

  useEffect(() => {
    tabLayoutRef.current = tabLayout;
  }, [tabLayout]);

  useEffect(() => {
    selectedArchiveRef.current = selectedArchive;
  }, [selectedArchive]);

  useEffect(() => {
    confirmationRef.current = confirmation;
  }, [confirmation]);

  useEffect(() => {
    updatePromptOpenRef.current = updatePromptOpen;
  }, [updatePromptOpen]);

  useEffect(() => {
    sourcePromptOpenRef.current = sourcePrompts.length > 0;
  }, [sourcePrompts.length]);

  // 禁用 WebView 默认右键菜单(刷新/打印/检查等,对本应用无意义)
  useEffect(() => {
    const onCtx = (e: MouseEvent) => e.preventDefault();
    document.addEventListener('contextmenu', onCtx);
    return () => document.removeEventListener('contextmenu', onCtx);
  }, []);

  const refreshTree = useCallback(async () => {
    const nodes = await api.listWatchDirs();
    treeRef.current = nodes;
    setTree(nodes);
    return nodes;
  }, []);

  useEffect(() => {
    refreshTree();
    api.newLogItems().then(setNewItems);
    // 启动时同步后端持久化的后缀筛选,避免前后端筛选分叉(通知与可见树不一致)
    api.getFilter().then(([suffixes, showAllCfg]) => {
      // 若用户在响应返回前已修改筛选,则不用旧配置覆盖
      if (filterEdited.current) return;
      setFilter(suffixes);
      setShowAll(showAllCfg);
    });
    // 订阅后端到达事件
    const unsub = api.subscribeNewLogs((item) => {
      // 已读过的项不再加回;同一 id 只保留一条,避免重复事件导致计数虚高
      if (seen.current.has(item.id)) return;
      setNewItems((prev) => (prev.some((p) => p.id === item.id) ? prev : [item, ...prev]));
    });
    const unsubChanges = api.subscribeDirectoryChanges((batch) => {
      const before = treeRef.current;
      const after = applyDirectoryChanges(before, batch);
      treeRef.current = after;
      setTree(after);

      for (const change of batch.changes) {
        if (change.type === 'upsert') {
          potentialSourceChangeRef.current(change.node.path ?? change.node.id);
        } else if (change.type === 'remove') {
          deletedSourceRef.current(change.path);
        } else if (change.type === 'rename') {
          deletedSourceRef.current(change.oldPath);
        } else if (change.type === 'rescan') {
          const sources = new Set(
            Object.entries(tabsRef.current).map(([id, tab]) => tabSourcePath(id, tab)),
          );
          for (const source of sources) {
            if (
              sameFilePath(source, batch.watchDir) ||
              isPathInsideDirectory(source, batch.watchDir)
            ) {
              potentialSourceChangeRef.current(source);
            }
          }
        }
      }

      const removed = removedDirectoryNodes(before, after, batch.watchDir);
      if (removed.length === 0) return;
      removed.forEach((node) => {
        if (node.kind === 'dir') void api.collapseDirectory(node.path ?? node.id);
      });
      const removedTree = flattenNodes(removed);
      const removedIds = new Set(removedTree.map((node) => node.id));
      setNewItems((items) =>
        items.filter((item) => {
          if (!removedIds.has(item.id)) return true;
          seen.current.add(item.id);
          return false;
        }),
      );
      const selected = selectedArchiveRef.current;
      const removedPaths = removedTree.map((node) => node.path ?? node.id);
      for (const node of removedTree) {
        deletedSourceRef.current(node.path ?? node.id, node.kind === 'dir');
      }
      if (selected && removedPaths.some((path) => sameFilePath(path, selected))) {
        setSelectedArchive(null);
      }
    });
    return () => {
      unsub();
      unsubChanges();
    };
  }, [refreshTree]);

  const addDir = useCallback(async () => {
    try {
      const ok = await api.addWatchDir(t('dialog.selectWatch'));
      if (ok) refreshTree();
    } catch (error) {
      alert(t('common.openFailed', { error: localizedError(error) }));
    }
  }, [localizedError, refreshTree, t]);

  const loadDirectory = useCallback(async (path: string) => {
    const children = await api.expandDirectory(path);
    const next = applyDirectoryChanges(treeRef.current, {
      watchDir: path,
      changes: [{ type: 'rescan', nodes: children }],
    });
    treeRef.current = next;
    setTree(next);
  }, []);

  const expandDirectory = useCallback(
    async (node: TreeNode) => {
      await loadDirectory(node.path ?? node.id);
    },
    [loadDirectory],
  );

  const collapseDirectory = useCallback((node: TreeNode) => {
    void api.collapseDirectory(node.path ?? node.id);
  }, []);

  const passesFilter = useCallback(
    (node: { name: string; kind: string; isLog?: boolean }) => {
      return passesDirectoryFilter(node, filter, showAll);
    },
    [filter, showAll],
  );

  const markSeen = useCallback((id: string) => {
    seen.current.add(id);
    setNewItems((items) => items.filter((it) => it.id !== id));
  }, []);

  const updateTabs = useCallback(
    (updater: (current: Record<string, LogTab>) => Record<string, LogTab>) => {
      const next = updater(tabsRef.current);
      tabsRef.current = next;
      setTabs(next);
    },
    [],
  );

  const openEntry = useCallback(
    async (entryKey: string, unreadId?: string, options?: { force?: boolean }) => {
      const existing = tabsRef.current[entryKey];
      updateTabLayout((layout) => openTab(layout, entryKey));
      // 打开压缩包内文件时由 activeKey 高亮该文件本身,不再额外高亮其外层压缩包(避免双重背景色)
      setSelectedArchive(null);
      if (!existing) {
        updateTabs((current) => ({
          ...current,
          [entryKey]: {
            id: entryKey,
            title: tabTitle(entryKey),
            absolutePath: entryKey.split('::').join(' › '),
            status: 'opening',
            session: null,
            sourceState: 'current',
          },
        }));
      } else if (
        !options?.force &&
        (existing.status === 'ready' || existing.status === 'opening')
      ) {
        if (unreadId) markSeen(unreadId);
        return;
      } else {
        if (options?.force && existing.session) {
          await api.closeLogSession(entryKey, existing.session.sessionId).catch(() => undefined);
        }
        updateTabs((current) => ({
          ...current,
          [entryKey]: {
            ...current[entryKey],
            status: 'opening',
            session: null,
            error: undefined,
            sourceState: 'current',
          },
        }));
      }

      const generation = (tabOpenGeneration.current.get(entryKey) ?? 0) + 1;
      tabOpenGeneration.current.set(entryKey, generation);
      try {
        const opened = await api.openLogSession(entryKey);
        const revision = await api
          .fileRevision(opened.sourcePath)
          .catch(() => ({ exists: true, revision: undefined }));
        if (unreadId) markSeen(unreadId);
        if (tabOpenGeneration.current.get(entryKey) !== generation || !tabsRef.current[entryKey]) {
          await api.closeLogSession(entryKey, opened.sessionId);
          return;
        }
        updateTabs((current) => {
          const next = { ...markEvictedSessions(current, opened.evictedSessionIds) };
          next[entryKey] = {
            ...next[entryKey],
            session: opened,
            sourcePath: opened.sourcePath,
            status: 'ready',
            error: undefined,
            sourceRevision: revision.revision,
            sourceState: 'current',
          };
          return next;
        });
      } catch (error) {
        if (unreadId) markSeen(unreadId);
        if (tabOpenGeneration.current.get(entryKey) !== generation) return;
        updateTabs((current) => {
          const tab = current[entryKey];
          if (!tab) return current;
          return {
            ...current,
            [entryKey]: { ...tab, session: null, status: 'error', error: String(error) },
          };
        });
        alert(t('error.cannotOpen', { error: localizedError(error) }));
      }
    },
    [localizedError, markSeen, t, updateTabLayout, updateTabs],
  );

  const activateLogTab = useCallback(
    (entryKey: string) => {
      updateTabLayout((layout) => activateTab(layout, entryKey));
      setSelectedArchive(null);
      const tab = tabsRef.current[entryKey];
      if (tab?.sourceState === 'deleted') return;
      if (tab?.status === 'dormant' || tab?.status === 'error') void openEntry(entryKey);
    },
    [openEntry, updateTabLayout],
  );

  const closeLogTab = useCallback(
    (entryKey: string) => {
      tabOpenGeneration.current.set(entryKey, (tabOpenGeneration.current.get(entryKey) ?? 0) + 1);
      void api.closeLogSession(entryKey).catch(() => undefined);
      const nextActive = updateTabLayout((layout) => closeTab(layout, entryKey)).active;
      setSelectedArchive(null);
      updateTabs((current) => {
        const next = { ...current };
        delete next[entryKey];
        return next;
      });
      queueMicrotask(() => {
        if (!nextActive) return;
        const tab = tabsRef.current[nextActive];
        if (
          tab?.sourceState !== 'deleted' &&
          (tab?.status === 'dormant' || tab?.status === 'error')
        ) {
          void openEntry(nextActive);
        }
      });
    },
    [openEntry, updateTabLayout, updateTabs],
  );

  const closeTabsMatching = useCallback(
    (predicate: (entryKey: string) => boolean) => {
      Object.keys(tabsRef.current).filter(predicate).forEach(closeLogTab);
    },
    [closeLogTab],
  );
  const enqueueSourcePrompt = useCallback((prompt: SourceChangePrompt) => {
    setSourcePrompts((current) => mergeSourceChangePrompt(current, prompt, sameFilePath));
  }, []);

  const markSourceDeleted = useCallback(
    (path: string, subtree = false) => {
      const suppressedIndex = selfDeletedSources.current.findIndex((entry) =>
        entry.subtree
          ? sameFilePath(path, entry.path) || isPathInsideDirectory(path, entry.path)
          : sameFilePath(path, entry.path),
      );
      if (suppressedIndex >= 0) {
        return;
      }
      const affectedSources = new Set<string>();
      for (const [entryKey, tab] of Object.entries(tabsRef.current)) {
        const source = tabSourcePath(entryKey, tab);
        if (sameFilePath(source, path) || (subtree && isPathInsideDirectory(source, path))) {
          affectedSources.add(source);
        }
      }
      if (affectedSources.size === 0) return;
      updateTabs((current) => {
        const next = { ...current };
        for (const [id, tab] of Object.entries(current)) {
          const source = tabSourcePath(id, tab);
          if (!affectedSources.has(source)) continue;
          next[id] = {
            ...tab,
            sourceState: 'deleted',
            status: tab.session ? tab.status : 'error',
            error: tab.session
              ? tab.error
              : t('workspace.deletedMessage', { name: sourceName(source) }),
          };
        }
        return next;
      });
      for (const source of affectedSources) {
        const sessionEntries = Object.entries(tabsRef.current).filter(
          ([id, tab]) => tab.session && sameFilePath(tabSourcePath(id, tab), source),
        );
        const active = tabLayoutRef.current.active;
        const entryKey =
          sessionEntries.find(([id]) => id === active)?.[0] ?? sessionEntries[0]?.[0];
        enqueueSourcePrompt({ sourcePath: source, kind: 'deleted', entryKey });
      }
    },
    [enqueueSourcePrompt, t, updateTabs],
  );
  deletedSourceRef.current = markSourceDeleted;

  const checkSourceRevision = useCallback(
    (path: string) => {
      const source = Object.entries(tabsRef.current)
        .map(([id, tab]) => tabSourcePath(id, tab))
        .find((candidate) => sameFilePath(candidate, path));
      if (!source || pendingRevisionChecks.current.has(source)) return;
      pendingRevisionChecks.current.add(source);
      void api
        .fileRevision(source)
        .then((currentRevision) => {
          const matching = Object.entries(tabsRef.current).filter(
            ([id, tab]) =>
              tab.sourceState !== 'deleted' && sameFilePath(tabSourcePath(id, tab), source),
          );
          if (matching.length === 0) return;
          if (!currentRevision.exists) {
            markSourceDeleted(source);
            return;
          }
          const known = matching.map(([, tab]) => tab.sourceRevision).find(Boolean);
          if (!known) {
            updateTabs((tabs) =>
              Object.fromEntries(
                Object.entries(tabs).map(([id, tab]) => [
                  id,
                  sameFilePath(tabSourcePath(id, tab), source)
                    ? { ...tab, sourceRevision: currentRevision.revision }
                    : tab,
                ]),
              ),
            );
          } else if (currentRevision.revision && currentRevision.revision !== known) {
            enqueueSourcePrompt({
              sourcePath: source,
              kind: 'modified',
              revision: currentRevision.revision,
            });
          }
        })
        .finally(() => pendingRevisionChecks.current.delete(source));
    },
    [enqueueSourcePrompt, markSourceDeleted, updateTabs],
  );
  potentialSourceChangeRef.current = checkSourceRevision;

  useEffect(() => {
    if (!activeSourcePath || activeSourceState === 'deleted') return;
    const verifyActiveSource = () => checkSourceRevision(activeSourcePath);
    const timer = window.setInterval(verifyActiveSource, 2_000);
    window.addEventListener('focus', verifyActiveSource);
    return () => {
      window.clearInterval(timer);
      window.removeEventListener('focus', verifyActiveSource);
    };
  }, [activeSourcePath, activeSourceState, checkSourceRevision]);

  const reloadSource = useCallback(
    async (sourcePath: string, revision?: string) => {
      const ids = Object.keys(tabsRef.current).filter((id) =>
        sameFilePath(tabSourcePath(id, tabsRef.current[id]), sourcePath),
      );
      const active = tabLayoutRef.current.active;
      for (const id of ids) {
        if (id === active) continue;
        tabOpenGeneration.current.set(id, (tabOpenGeneration.current.get(id) ?? 0) + 1);
        const session = tabsRef.current[id]?.session;
        if (session) await api.closeLogSession(id, session.sessionId).catch(() => undefined);
      }
      updateTabs((current) => {
        const next = { ...current };
        for (const id of ids) {
          if (id === active) continue;
          next[id] = {
            ...next[id],
            session: null,
            status: 'dormant',
            error: undefined,
            sourceRevision: revision,
            sourceState: 'current',
          };
        }
        return next;
      });
      if (active && ids.includes(active)) await openEntry(active, undefined, { force: true });
      else {
        updateTabs((current) =>
          Object.fromEntries(
            Object.entries(current).map(([id, tab]) => [
              id,
              ids.includes(id) ? { ...tab, sourceRevision: revision, sourceState: 'current' } : tab,
            ]),
          ),
        );
      }
    },
    [openEntry, updateTabs],
  );

  useEffect(() => {
    if (restoredWorkspaceStarted.current) return;
    restoredWorkspaceStarted.current = true;
    const restoredIds = tabIds(initialWorkspace);
    if (restoredIds.length === 0) return;
    void (async () => {
      const revisions = new Map<string, Awaited<ReturnType<typeof api.fileRevision>>>();
      for (const source of new Set(restoredIds.map(sourcePathForEntryKey))) {
        try {
          revisions.set(source, await api.fileRevision(source));
        } catch {
          revisions.set(source, { exists: true });
        }
      }
      const missingIds = new Set(
        restoredIds.filter((id) => !revisions.get(sourcePathForEntryKey(id))?.exists),
      );
      let nextLayout = initialWorkspace;
      if (missingIds.size > 0) {
        nextLayout = removeWorkspaceTabs(initialWorkspace, missingIds);
        updateTabLayout(() => nextLayout);
        updateTabs((current) =>
          Object.fromEntries(Object.entries(current).filter(([id]) => !missingIds.has(id))),
        );
        for (const source of new Set([...missingIds].map(sourcePathForEntryKey))) {
          enqueueSourcePrompt({ sourcePath: source, kind: 'deletedWhileClosed' });
        }
      }
      updateTabs((current) =>
        Object.fromEntries(
          Object.entries(current).map(([id, tab]) => [
            id,
            {
              ...tab,
              sourceRevision: revisions.get(sourcePathForEntryKey(id))?.revision,
            },
          ]),
        ),
      );
      if (nextLayout.active && !missingIds.has(nextLayout.active))
        void openEntry(nextLayout.active);
    })();
  }, [enqueueSourcePrompt, initialWorkspace, openEntry, updateTabLayout, updateTabs]);

  const revealNewItem = useCallback(
    async (item: NewLogItem, options?: { openFile?: boolean }) => {
      const directories = revealDirectoryChain(treeRef.current, item.id);
      if (directories.length === 0) {
        markSeen(item.id);
        setRevealedTarget(null);
        alert(t('error.cannotLocateOutside'));
        return;
      }

      try {
        for (const directory of directories) await loadDirectory(directory);
      } catch {
        markSeen(item.id);
        setRevealedTarget(null);
        alert(t('error.cannotLocateMoved'));
        return;
      }

      if (!findTreeNode(treeRef.current, item.id)) {
        markSeen(item.id);
        setRevealedTarget(null);
        alert(t('error.cannotLocateDeleted'));
        return;
      }

      setRevealedTarget({ path: item.id, directories });
      if (item.kind === 'file') {
        if (options?.openFile === false) markSeen(item.id);
        else await openEntry(item.id, item.id);
      } else {
        setSelectedArchive(item.id);
        markSeen(item.id);
      }
    },
    [loadDirectory, markSeen, openEntry, t],
  );

  const handleDroppedPaths = useCallback(
    async (paths: readonly string[]) => {
      if (dropBusy.current) {
        alert(t('error.dropBusy'));
        return;
      }
      if (confirmationRef.current || updatePromptOpenRef.current || sourcePromptOpenRef.current) {
        alert(t('error.finishDialog'));
        return;
      }

      dropBusy.current = true;
      try {
        const path = singleDroppedPath(paths);
        const info: DroppedFileInfo = await api.inspectDroppedFile(path);
        const plan = planFileDrop(info);

        // 日志查看与监控添加互不依赖，检查通过后立即启动现有打开流程。
        if (plan.openPath) void openEntry(plan.openPath);

        if (plan.watchPathToAdd) {
          await api.addWatchPath(plan.watchPathToAdd);
          await refreshTree();
        }

        if (plan.locateInTree && info.kind !== 'directory') {
          await revealNewItem(
            {
              id: info.path,
              name: info.name,
              kind: info.kind,
              source: info.watchPath,
              age: 'now',
            },
            { openFile: false },
          );
        }
      } catch (error) {
        alert(t('error.dropFailed', { error: localizedError(error) }));
      } finally {
        dropBusy.current = false;
      }
    },
    [localizedError, openEntry, refreshTree, revealNewItem, t],
  );

  useEffect(() => {
    if (!isTauri) return;
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void getCurrentWebview()
      .onDragDropEvent((event) => {
        if (event.payload.type === 'drop') void handleDroppedPaths(event.payload.paths);
      })
      .then((stop) => {
        if (disposed) stop();
        else unlisten = stop;
      })
      .catch((error) => alert(t('error.dropUnavailable', { error: localizedError(error) })));
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [handleDroppedPaths, localizedError, t]);

  const finishReveal = useCallback(() => setRevealedTarget(null), []);

  const markAllRead = useCallback(() => {
    // 记住已读,避免重复事件把它们重新加回列表
    setNewItems((items) => {
      items.forEach((it) => seen.current.add(it.id));
      return [];
    });
  }, []);

  const renameNode = useCallback(
    async (node: TreeNode, newName: string) => {
      const path = node.path ?? node.id;
      const mutation = { path, subtree: node.kind === 'dir' };
      selfDeletedSources.current.push(mutation);
      try {
        if (node.kind === 'dir') await api.renameWatchDir(path, newName);
        else await api.renameFile(path, newName);
        // 旧路径已失效:移除指向旧路径的通知项,避免点开报错
        markSeen(node.id);
        closeTabsMatching((entryKey) => {
          const container = tabContainerPath(entryKey);
          return node.kind === 'dir'
            ? sameFilePath(container, path) || isPathInsideDirectory(container, path)
            : sameFilePath(container, path);
        });
        refreshTree();
      } catch (e) {
        selfDeletedSources.current = selfDeletedSources.current.filter((item) => item !== mutation);
        alert(t('error.renameFailed', { error: localizedError(e) }));
      } finally {
        window.setTimeout(() => {
          selfDeletedSources.current = selfDeletedSources.current.filter(
            (item) => item !== mutation,
          );
        }, 10_000);
      }
    },
    [closeTabsMatching, localizedError, refreshTree, markSeen, t],
  );

  const openPath = useCallback(
    async (node: TreeNode) => {
      try {
        await api.openPath(node.path ?? node.id);
      } catch (e) {
        alert(t('common.openFailed', { error: localizedError(e) }));
      }
    },
    [localizedError, t],
  );

  const removeWatch = useCallback(
    async (node: TreeNode) => {
      const path = node.path ?? node.id;
      try {
        await api.removeWatchDir(path);
        refreshTree();
      } catch (e) {
        alert(t('error.removeFailed', { error: localizedError(e) }));
      }
    },
    [localizedError, refreshTree, t],
  );

  const deleteDir = useCallback(
    (node: TreeNode) => {
      const path = node.path ?? node.id;
      setConfirmation({
        title: t('confirm.deleteDirectoryTitle', { name: node.name }),
        message: t('confirm.deleteDirectoryMessage'),
        confirmLabel: t('confirm.deleteDirectory'),
        action: async () => {
          const mutation = { path, subtree: true };
          selfDeletedSources.current.push(mutation);
          try {
            await api.deleteWatchDir(path);
            closeTabsMatching((entryKey) => {
              const container = tabContainerPath(entryKey);
              return sameFilePath(container, path) || isPathInsideDirectory(container, path);
            });
            // 移除该目录下所有失效的通知项(id 为完整路径,以目录路径为前缀)
            const prefixes = [path + '/', path + '\\'];
            setNewItems((items) =>
              items.filter((it) => {
                const stale = it.id === path || prefixes.some((p) => it.id.startsWith(p));
                if (stale) seen.current.add(it.id);
                return !stale;
              }),
            );
            refreshTree();
          } catch (e) {
            selfDeletedSources.current = selfDeletedSources.current.filter(
              (item) => item !== mutation,
            );
            alert(t('error.deleteFailed', { error: localizedError(e) }));
          } finally {
            window.setTimeout(() => {
              selfDeletedSources.current = selfDeletedSources.current.filter(
                (item) => item !== mutation,
              );
            }, 10_000);
          }
        },
      });
    },
    [closeTabsMatching, localizedError, refreshTree, t],
  );

  const deleteFile = useCallback(
    (node: TreeNode) => {
      const target = node.path ?? node.id;
      setConfirmation({
        title: t('confirm.deleteFileTitle', { name: node.name }),
        message: t('confirm.deleteFileMessage'),
        confirmLabel: t('confirm.deleteFile'),
        action: async () => {
          const mutation = { path: target, subtree: false };
          selfDeletedSources.current.push(mutation);
          try {
            await api.deleteFile(target);
            closeTabsMatching((entryKey) => sameFilePath(tabContainerPath(entryKey), target));
            markSeen(node.id);
            refreshTree();
          } catch (e) {
            selfDeletedSources.current = selfDeletedSources.current.filter(
              (item) => item !== mutation,
            );
            alert(t('error.deleteFailed', { error: localizedError(e) }));
          } finally {
            window.setTimeout(() => {
              selfDeletedSources.current = selfDeletedSources.current.filter(
                (item) => item !== mutation,
              );
            }, 10_000);
          }
        },
      });
    },
    [closeTabsMatching, localizedError, markSeen, refreshTree, t],
  );

  const sourcePrompt = sourcePrompts[0] ?? null;
  const closeSourcePrompt = useCallback(() => {
    setSourcePrompts((current) => current.slice(1));
  }, []);
  const keepSourceSnapshot = useCallback(() => {
    const prompt = sourcePrompts[0];
    if (!prompt) return;
    if (prompt.revision) {
      updateTabs((current) =>
        Object.fromEntries(
          Object.entries(current).map(([id, tab]) => [
            id,
            sameFilePath(tabSourcePath(id, tab), prompt.sourcePath)
              ? { ...tab, sourceRevision: prompt.revision }
              : tab,
          ]),
        ),
      );
    }
    closeSourcePrompt();
  }, [closeSourcePrompt, sourcePrompts, updateTabs]);
  const saveDeletedSnapshot = useCallback(
    async (prompt: SourceChangePrompt) => {
      if (!prompt.entryKey) return;
      try {
        const result = await api.saveSessionSnapshot(
          prompt.entryKey,
          tabTitle(prompt.entryKey),
          t('workspace.saveDialogTitle'),
        );
        if (!result) return;
        alert(t(result.complete ? 'workspace.snapshotSaved' : 'workspace.partialSnapshotSaved'));
      } catch (error) {
        alert(t('workspace.snapshotSaveFailed', { error: localizedError(error) }));
      }
    },
    [localizedError, t],
  );
  const sourcePromptCanSave = Boolean(
    sourcePrompt?.kind === 'deleted' &&
    sourcePrompt.entryKey &&
    tabs[sourcePrompt.entryKey]?.session,
  );

  const hasDirs = tree.length > 0;

  return (
    <div className="app">
      <TopBar
        theme={theme}
        onToggleTheme={() => setTheme((t) => (t === 'dark' ? 'light' : 'dark'))}
        count={count}
        newItems={newItems}
        onOpenItem={(item) => void revealNewItem(item)}
        onMarkAll={markAllRead}
        appVersion={appVersion}
        autoCheckUpdates={autoCheckUpdates}
        updateStatus={updateStatus}
        updateInfo={updateInfo}
        updateProgress={updateProgress}
        updateError={updateError}
        onAutoCheckUpdatesChange={changeAutoCheckUpdates}
        onCheckForUpdates={() => void checkForUpdates(false)}
        onSkipUpdate={skipUpdate}
        onDownloadUpdate={() => void downloadUpdate()}
      />

      {updatePromptOpen && updateInfo && (
        <UpdateDialog
          update={updateInfo}
          onSkip={skipUpdate}
          onDownload={() => void downloadUpdate()}
        />
      )}

      {confirmation ? (
        <ConfirmDialog
          title={confirmation.title}
          message={confirmation.message}
          confirmLabel={confirmation.confirmLabel}
          cancelLabel={confirmation.cancelLabel}
          showCancel={confirmation.showCancel}
          danger={confirmation.danger}
          onCancel={() => setConfirmation(null)}
          onConfirm={() => {
            const action = confirmation.action;
            setConfirmation(null);
            void action();
          }}
        />
      ) : sourcePrompt ? (
        <ConfirmDialog
          title={t(
            sourcePrompt.kind === 'modified' ? 'workspace.modifiedTitle' : 'workspace.deletedTitle',
          )}
          message={t(
            sourcePrompt.kind === 'modified'
              ? 'workspace.modifiedMessage'
              : sourcePrompt.kind === 'deletedWhileClosed'
                ? 'workspace.deletedWhileClosed'
                : sourcePromptCanSave
                  ? 'workspace.deletedSaveMessage'
                  : 'workspace.deletedMessage',
            { name: sourceName(sourcePrompt.sourcePath) },
          )}
          confirmLabel={
            sourcePrompt.kind === 'modified'
              ? t('workspace.reload')
              : sourcePromptCanSave
                ? t('workspace.saveSnapshot')
                : t('common.ok')
          }
          cancelLabel={
            sourcePrompt.kind === 'modified'
              ? t('workspace.keepSnapshot')
              : t('workspace.doNotSave')
          }
          showCancel={sourcePrompt.kind === 'modified' || sourcePromptCanSave}
          danger={false}
          onCancel={sourcePrompt.kind === 'modified' ? keepSourceSnapshot : closeSourcePrompt}
          onConfirm={() => {
            const prompt = sourcePrompt;
            closeSourcePrompt();
            if (prompt.kind === 'modified') {
              void reloadSource(prompt.sourcePath, prompt.revision);
            } else if (prompt.kind === 'deleted' && sourcePromptCanSave) {
              void saveDeletedSnapshot(prompt);
            }
          }}
        />
      ) : null}

      <div className="cols">
        <DirTree
          nodes={tree}
          activeKey={activeKey}
          selectedArchive={selectedArchive}
          revealPath={revealedTarget?.path ?? null}
          revealDirectories={revealedTarget?.directories ?? []}
          onRevealComplete={finishReveal}
          width={treeWidth}
          unreadIds={unreadIds}
          filter={filter}
          showAll={showAll}
          passesFilter={passesFilter}
          onFilterChange={(f) => {
            filterEdited.current = true;
            setFilter(f);
            void api.setFilter(f, showAll);
          }}
          onShowAllChange={(v) => {
            filterEdited.current = true;
            setShowAll(v);
            void api.setFilter(filter, v);
          }}
          onAddDir={addDir}
          onExpandDirectory={expandDirectory}
          onCollapseDirectory={collapseDirectory}
          onSelectArchive={(name, id) => {
            setSelectedArchive(name);
            if (id) markSeen(id);
          }}
          onOpenFile={(name, id) => openEntry(name, id)}
          onRename={renameNode}
          onDelete={deleteFile}
          onOpenPath={openPath}
          onRemoveWatch={removeWatch}
          onDeleteDir={deleteDir}
        />
        <div className="col-resizer" onMouseDown={startResize} />

        {hasDirs || tabIds(tabLayout).length > 0 ? (
          <div className="col col-content log-workspace">
            <LogTabs
              tabs={tabs}
              visibleIds={tabLayout.visible}
              overflowIds={tabLayout.overflow}
              activeId={activeKey}
              onActivate={activateLogTab}
              onClose={closeLogTab}
              onCapacityChange={handleCapacityChange}
            />
            <div className="log-panels">
              {tabIds(tabLayout).length === 0 ? (
                <LogContent session={null} activeKey={null} />
              ) : (
                tabIds(tabLayout).map((id) => {
                  const tab = tabs[id];
                  if (!tab) return null;
                  return (
                    <div
                      key={id}
                      className={'log-panel-slot' + (activeKey === id ? ' active' : '')}
                    >
                      <LogContent
                        session={tab.session}
                        activeKey={id}
                        status={tab.status}
                        error={tab.error}
                      />
                    </div>
                  );
                })
              )}
            </div>
          </div>
        ) : (
          <EmptyState onAddDir={addDir} />
        )}
      </div>
    </div>
  );
}

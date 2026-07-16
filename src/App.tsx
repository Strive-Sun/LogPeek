import { useCallback, useEffect, useRef, useState } from 'react';
import { api } from './api';
import type { NewLogItem, OpenSessionResult, TreeNode } from './api';
import { TopBar } from './components/TopBar';
import { DirTree } from './components/DirTree';
import { LogContent } from './components/LogContent';
import { EmptyState } from './components/EmptyState';

export function App() {
  const [theme, setTheme] = useState<'dark' | 'light'>('light');
  const [tree, setTree] = useState<TreeNode[]>([]);
  const [newItems, setNewItems] = useState<NewLogItem[]>([]);
  // 徽章数字直接由未读列表长度派生,保证徽章与列表始终一致
  const count = newItems.length;
  // 未读项 id 集合(id 即文件路径),用于左树高亮;不依赖后端 unread 标记
  const unreadIds = new Set(newItems.map((it) => it.id));
  const seen = useRef<Set<string>>(new Set());

  // 当前选中的压缩包(用于左侧树高亮)与当前查看的条目 key
  const [selectedArchive, setSelectedArchive] = useState<string | null>(null);
  const [session, setSession] = useState<OpenSessionResult | null>(null);
  const [activeKey, setActiveKey] = useState<string | null>(null);

  // 后缀筛选
  const [filter, setFilter] = useState<string[]>(['.log', '.txt', '.out']);
  const [showAll, setShowAll] = useState(false);

  // 左栏宽度(可拖动调整),持久化到 localStorage
  const [treeWidth, setTreeWidth] = useState<number>(() => {
    const saved = Number(localStorage.getItem('logpeek.treeWidth'));
    return saved >= 160 && saved <= 720 ? saved : 300;
  });

  const startResize = useCallback((e: React.MouseEvent) => {
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
  }, [treeWidth]);

  useEffect(() => {
    localStorage.setItem('logpeek.treeWidth', String(treeWidth));
  }, [treeWidth]);

  useEffect(() => {
    document.documentElement.dataset.theme = theme;
  }, [theme]);

  // 禁用 WebView 默认右键菜单(刷新/打印/检查等,对本应用无意义)
  useEffect(() => {
    const onCtx = (e: MouseEvent) => e.preventDefault();
    document.addEventListener('contextmenu', onCtx);
    return () => document.removeEventListener('contextmenu', onCtx);
  }, []);

  const refreshTree = useCallback(() => {
    api.listWatchDirs().then(setTree);
  }, []);

  useEffect(() => {
    refreshTree();
    api.newLogItems().then(setNewItems);
    // 订阅后端到达事件
    const unsub = api.subscribeNewLogs((item) => {
      // 已读过的项不再加回;同一 id 只保留一条,避免重复事件导致计数虚高
      if (seen.current.has(item.id)) return;
      setNewItems((prev) => (prev.some((p) => p.id === item.id) ? prev : [item, ...prev]));
      refreshTree();
    });
    return unsub;
  }, [refreshTree]);

  const addDir = useCallback(async () => {
    const ok = await api.addWatchDir();
    if (ok) refreshTree();
  }, [refreshTree]);

  const passesFilter = useCallback(
    (node: { name: string; kind: string; isLog?: boolean }) => {
      if (node.kind === 'dir' || node.kind === 'archive') return true;
      if (showAll) return true;
      const lower = node.name.toLowerCase();
      return filter.some((s) => lower.endsWith(s));
    },
    [filter, showAll],
  );

  const markSeen = useCallback((id: string) => {
    seen.current.add(id);
    setNewItems((items) => items.filter((it) => it.id !== id));
  }, []);

  const openEntry = useCallback(
    async (entryKey: string, unreadId?: string) => {
      setActiveKey(entryKey);
      setSession(null);
      try {
        const s = await api.openLogSession(entryKey);
        setSession(s);
        if (unreadId) markSeen(unreadId);
      } catch (e) {
        setSession(null);
        alert(String(e));
      }
    },
    [markSeen],
  );

  const markAllRead = useCallback(() => {
    // 记住已读,避免重复事件把它们重新加回列表
    setNewItems((items) => {
      items.forEach((it) => seen.current.add(it.id));
      return [];
    });
  }, []);

  const renameFile = useCallback(
    async (path: string, newName: string) => {
      try {
        await api.renameFile(path, newName);
        refreshTree();
      } catch (e) {
        alert('重命名失败:' + String(e));
      }
    },
    [refreshTree],
  );

  const deleteFile = useCallback(
    async (node: TreeNode) => {
      const target = node.path ?? node.id;
      if (!window.confirm(`确定删除「${node.name}」吗?\n文件将被移到系统回收站。`)) return;
      try {
        await api.deleteFile(target);
        // 若当前查看的正是被删文件(或被删压缩包内的条目),清空视图
        if (activeKey === node.name || activeKey?.startsWith(node.name + '::')) {
          setActiveKey(null);
          setSession(null);
          setSelectedArchive(null);
        }
        markSeen(node.id);
        refreshTree();
      } catch (e) {
        alert('删除失败:' + String(e));
      }
    },
    [activeKey, markSeen, refreshTree],
  );

  const hasDirs = tree.length > 0;

  return (
    <div className="app">
      <TopBar
        theme={theme}
        onToggleTheme={() => setTheme((t) => (t === 'dark' ? 'light' : 'dark'))}
        count={count}
        newItems={newItems}
        onOpenItem={(it) => {
          const key = it.kind === 'file' ? it.name : `${it.name}::`;
          if (it.kind === 'file') openEntry(it.name, it.id);
          else {
            setSelectedArchive(it.name);
            markSeen(it.id);
          }
          void key;
        }}
        onMarkAll={markAllRead}
      />

      <div className="cols">
        <DirTree
          nodes={tree}
          activeKey={activeKey}
          selectedArchive={selectedArchive}
          width={treeWidth}
          unreadIds={unreadIds}
          filter={filter}
          showAll={showAll}
          passesFilter={passesFilter}
          onFilterChange={(f) => {
            setFilter(f);
            void api.setFilter(f, showAll);
          }}
          onShowAllChange={(v) => {
            setShowAll(v);
            void api.setFilter(filter, v);
          }}
          onAddDir={addDir}
          onSelectArchive={(name, id) => {
            setSelectedArchive(name);
            if (id) markSeen(id);
          }}
          onOpenFile={(name, id) => openEntry(name, id)}
          onRename={renameFile}
          onDelete={deleteFile}
        />
        <div className="col-resizer" onMouseDown={startResize} />

        {hasDirs ? (
          <LogContent session={session} activeKey={activeKey} />
        ) : (
          <EmptyState onAddDir={addDir} />
        )}
      </div>
    </div>
  );
}

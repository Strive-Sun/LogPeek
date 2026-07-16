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
  const [count, setCount] = useState(0);
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

  const refreshTree = useCallback(() => {
    api.listWatchDirs().then(setTree);
  }, []);

  useEffect(() => {
    refreshTree();
    api.newLogItems().then((items) => {
      setNewItems(items);
      setCount(items.length);
    });
    // 订阅后端到达事件
    const unsub = api.subscribeNewLogs((item) => {
      setNewItems((prev) => (prev.some((p) => p.id === item.id) ? prev : [item, ...prev]));
      if (!seen.current.has(item.id)) setCount((c) => c + 1);
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
    if (seen.current.has(id)) return;
    seen.current.add(id);
    setCount((c) => Math.max(0, c - 1));
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
    setCount(0);
    setNewItems([]);
  }, []);

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

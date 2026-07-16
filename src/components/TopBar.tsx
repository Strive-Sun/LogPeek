import { useEffect, useRef, useState } from 'react';
import type { NewLogItem } from '../api';

interface Props {
  theme: 'dark' | 'light';
  onToggleTheme: () => void;
  count: number;
  newItems: NewLogItem[];
  onOpenItem: (item: NewLogItem) => void;
  onMarkAll: () => void;
  filter: string[];
  showAll: boolean;
  onFilterChange: (f: string[]) => void;
  onShowAllChange: (v: boolean) => void;
}

const SUFFIX_CHOICES = ['.log', '.txt', '.out', '.json'];

export function TopBar(props: Props) {
  const { count, newItems } = props;
  const [bellOpen, setBellOpen] = useState(false);
  const [filterOpen, setFilterOpen] = useState(false);
  const [ring, setRing] = useState(false);
  const prevCount = useRef(count);
  const [custom, setCustom] = useState('');

  // 计数增加时铃铛抖动
  useEffect(() => {
    if (count > prevCount.current) {
      setRing(true);
      const t = setTimeout(() => setRing(false), 400);
      return () => clearTimeout(t);
    }
    prevCount.current = count;
  }, [count]);

  const toggleSuffix = (s: string) => {
    props.onFilterChange(
      props.filter.includes(s) ? props.filter.filter((x) => x !== s) : [...props.filter, s],
    );
  };

  const addCustom = () => {
    let s = custom.trim();
    if (!s) return;
    if (!s.startsWith('.')) s = '.' + s;
    if (!props.filter.includes(s)) props.onFilterChange([...props.filter, s]);
    setCustom('');
  };

  return (
    <div className="topbar">
      <span className="brand">LogPeek</span>
      <span className="search" title="搜索将在 M4 提供">🔍 搜索</span>
      <span className="spacer" />

      <button className="icon-btn" onClick={() => setFilterOpen((v) => !v)} title="后缀筛选">
        后缀 ▾
      </button>
      <button className="icon-btn" onClick={props.onToggleTheme} title="切换主题">
        {props.theme === 'dark' ? '🌙' : '☀️'}
      </button>
      <button className="icon-btn" onClick={() => setBellOpen((v) => !v)} title="新日志提示">
        <span className={'bell' + (ring ? ' ring' : '')}>🔔</span>
        {count > 0 && <span className="badge">{count > 99 ? '99+' : count}</span>}
      </button>
      <button className="icon-btn" title="设置">⚙️</button>

      {filterOpen && (
        <>
          <div className="backdrop" onClick={() => setFilterOpen(false)} />
          <div className="pop filter-pop">
            <div className="pop-head">后缀筛选</div>
            {SUFFIX_CHOICES.map((s) => (
              <div className="filter-row" key={s}>
                <label>
                  <input
                    type="checkbox"
                    checked={props.filter.includes(s)}
                    onChange={() => toggleSuffix(s)}
                  />
                  {s}
                </label>
              </div>
            ))}
            <div className="filter-row">
              <label>
                <input
                  type="checkbox"
                  checked={props.showAll}
                  onChange={(e) => props.onShowAllChange(e.target.checked)}
                />
                显示全部(含非日志)
              </label>
            </div>
            <div className="filter-custom">
              <input
                placeholder=".trace"
                value={custom}
                onChange={(e) => setCustom(e.target.value)}
                onKeyDown={(e) => e.key === 'Enter' && addCustom()}
              />
              <button className="icon-btn" onClick={addCustom}>+</button>
            </div>
          </div>
        </>
      )}

      {bellOpen && (
        <>
          <div className="backdrop" onClick={() => setBellOpen(false)} />
          <div className="pop bell-pop">
            <div className="pop-head">
              <span>新日志 ({newItems.length})</span>
              <button
                className="mark-all"
                onClick={() => {
                  props.onMarkAll();
                  setBellOpen(false);
                }}
              >
                全部标记已读
              </button>
            </div>
            {newItems.length === 0 && (
              <div className="pop-item" style={{ color: 'var(--fg-dim)' }}>
                没有未读的新日志
              </div>
            )}
            {newItems.map((it) => (
              <div
                className="pop-item"
                key={it.id}
                onClick={() => {
                  props.onOpenItem(it);
                  setBellOpen(false);
                }}
              >
                <span>{it.kind === 'archive' ? '📦' : '📄'}</span>
                <span>{it.name}</span>
                <span className="src">
                  {it.source}/ {it.age}
                </span>
              </div>
            ))}
          </div>
        </>
      )}
    </div>
  );
}

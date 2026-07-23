import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { useVirtualizer } from '@tanstack/react-virtual';
import { api } from '../api';
import type {
  ArchiveEntry,
  FileSearchConfig,
  FileSearchFilter,
  FileSearchResult,
  FileSearchStatus,
} from '../api';
import { useI18n } from '../i18n/I18nProvider';
import { fmtNum, fmtSize } from '../util/format';
import {
  mergeSearchResults,
  planSearchOpen,
  searchProgressRefreshKey,
  shouldApplySearchResponse,
} from '../util/fileSearch';
import { ContextMenu } from './ContextMenu';

const PAGE_SIZE = 200;
const MAX_VISIBLE_RESULTS = 1_000;

interface Props {
  onClose: () => void;
  onOpenEntry: (entryKey: string) => void;
  onMonitorAdded: (item: FileSearchResult) => Promise<void>;
}

interface ArchiveView {
  stack: string[];
  entries: ArchiveEntry[];
  loading: boolean;
  error?: string;
}

function modifiedText(value: number | undefined, locale: string): string {
  if (!value) return '';
  return new Intl.DateTimeFormat(locale, {
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  }).format(new Date(value));
}

function highlighted(text: string, query: string): ReactNode {
  const terms = query
    .trim()
    .split(/\s+/)
    .filter(Boolean)
    .sort((left, right) => right.length - left.length);
  if (terms.length === 0) return text;
  const expression = new RegExp(
    `(${terms.map((term) => term.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')).join('|')})`,
    'gi',
  );
  return text
    .split(expression)
    .map((part, index) =>
      terms.some((term) => part.toLocaleLowerCase() === term.toLocaleLowerCase()) ? (
        <mark key={index}>{part}</mark>
      ) : (
        part
      ),
    );
}

function resultIcon(item: FileSearchResult) {
  return item.isArchive ? '📦' : item.isLog ? '📄' : '📃';
}

export function FileSearchPanel({ onClose, onOpenEntry, onMonitorAdded }: Props) {
  const { locale, t } = useI18n();
  const [status, setStatus] = useState<FileSearchStatus | null>(null);
  const [config, setConfig] = useState<FileSearchConfig | null>(null);
  const [query, setQuery] = useState('');
  const [filter, setFilter] = useState<FileSearchFilter>('all');
  const [items, setItems] = useState<FileSearchResult[]>([]);
  const [total, setTotal] = useState(0);
  const [elapsed, setElapsed] = useState(0);
  const [partial, setPartial] = useState(false);
  const [loading, setLoading] = useState(false);
  const [loadingMore, setLoadingMore] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [selected, setSelected] = useState(0);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [clearArmed, setClearArmed] = useState(false);
  const [menu, setMenu] = useState<{ x: number; y: number; item: FileSearchResult } | null>(null);
  const [archive, setArchive] = useState<ArchiveView | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const queryGeneration = useRef(0);
  const previousFocus = useRef<HTMLElement | null>(document.activeElement as HTMLElement | null);
  const progressRefreshKey = searchProgressRefreshKey(status);

  const virtualizer = useVirtualizer({
    count: items.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => 44,
    overscan: 12,
  });

  useEffect(() => {
    let active = true;
    const focusToRestore = previousFocus.current;
    void Promise.all([api.fileSearchStatus(), api.fileSearchConfig()])
      .then(([nextStatus, nextConfig]) => {
        if (!active) return;
        setStatus(nextStatus);
        setConfig(nextConfig);
      })
      .catch((reason) => active && setError(String(reason)));
    const unsubscribe = api.subscribeFileSearchStatus((next) => {
      setStatus(next);
      setConfig((current) =>
        current
          ? { ...current, enabled: next.phase !== 'disabled', exclusions: next.exclusions }
          : current,
      );
    });
    window.setTimeout(() => inputRef.current?.focus(), 0);
    return () => {
      active = false;
      unsubscribe();
      focusToRestore?.focus();
    };
  }, []);

  const executeQuery = useCallback(
    async (offset: number, append: boolean) => {
      const trimmed = query.trim();
      if (!trimmed) {
        queryGeneration.current += 1;
        setItems([]);
        setTotal(0);
        setPartial(status?.phase !== 'ready');
        setLoading(false);
        setLoadingMore(false);
        return;
      }
      const generation = append ? queryGeneration.current : queryGeneration.current + 1;
      if (!append) queryGeneration.current = generation;
      if (append) setLoadingMore(true);
      else setLoading(true);
      setError(null);
      try {
        const page = await api.searchFiles(trimmed, filter, offset, PAGE_SIZE);
        if (!shouldApplySearchResponse(generation, queryGeneration.current)) return;
        setItems((current) => mergeSearchResults(current, page.items, append, MAX_VISIBLE_RESULTS));
        setTotal(page.total);
        setPartial(page.partial);
        setElapsed(page.elapsedMs);
        if (!append) {
          setSelected(0);
          scrollRef.current?.scrollTo({ top: 0 });
        }
      } catch (reason) {
        if (shouldApplySearchResponse(generation, queryGeneration.current))
          setError(String(reason));
      } finally {
        if (shouldApplySearchResponse(generation, queryGeneration.current)) {
          setLoading(false);
          setLoadingMore(false);
        }
      }
    },
    [filter, query, status?.phase],
  );

  useEffect(() => {
    const timer = window.setTimeout(() => {
      if (progressRefreshKey) void executeQuery(0, false);
    }, 140);
    return () => window.clearTimeout(timer);
  }, [executeQuery, progressRefreshKey]);

  useEffect(() => {
    if (selected < 0 || selected >= items.length) return;
    virtualizer.scrollToIndex(selected, { align: 'auto' });
  }, [items.length, selected, virtualizer]);

  const openArchive = useCallback(async (path: string, stack?: string[]) => {
    const nextStack = stack ?? [path];
    setArchive({ stack: nextStack, entries: [], loading: true });
    try {
      const entries = await api.listArchiveEntries(path);
      setArchive({ stack: nextStack, entries, loading: false });
    } catch (reason) {
      setArchive({ stack: nextStack, entries: [], loading: false, error: String(reason) });
    }
  }, []);

  const openResult = useCallback(
    async (item: FileSearchResult) => {
      setError(null);
      try {
        const info = await api.inspectSearchResult(item.path);
        const action = planSearchOpen(info);
        if (action === 'archive') {
          await openArchive(item.path);
        } else if (action === 'log') {
          onOpenEntry(item.path);
          onClose();
        } else {
          await api.openPath(item.path);
        }
      } catch (reason) {
        setError(String(reason));
        void executeQuery(0, false);
      }
    },
    [executeQuery, onClose, onOpenEntry, openArchive],
  );

  const openArchiveEntry = useCallback(
    async (entry: ArchiveEntry) => {
      if (!archive) return;
      const current = archive.stack[archive.stack.length - 1];
      if (entry.isArchive) {
        const nested = `${current}::${entry.path}`;
        await openArchive(nested, [...archive.stack, nested]);
      } else if (entry.isLog && !entry.encrypted) {
        onOpenEntry(`${current}::${entry.path}`);
        onClose();
      }
    },
    [archive, onClose, onOpenEntry, openArchive],
  );

  const addMonitor = useCallback(
    async (item: FileSearchResult) => {
      setError(null);
      try {
        await api.addSearchResultParent(item.path);
        await onMonitorAdded(item);
      } catch (reason) {
        setError(String(reason));
        void executeQuery(0, false);
      }
    },
    [executeQuery, onMonitorAdded],
  );

  const statusText = useMemo(() => {
    if (!status) return t('search.querying');
    if (status.phase === 'scanning')
      return t('search.scanning', {
        discovered: fmtNum(status.scannedFiles),
        searchable: fmtNum(status.indexedFiles),
      });
    if (status.phase === 'finalizing') return t('search.finalizing');
    if (status.phase === 'paused')
      return t('search.paused', { count: fmtNum(status.scannedFiles) });
    if (status.phase === 'error')
      return t('search.failed', { error: status.error ?? t('common.unknown') });
    return t('search.ready', { count: fmtNum(status.indexedFiles) });
  }, [status, t]);

  const closeArchive = useCallback(() => {
    if (!archive) return;
    if (archive.stack.length <= 1) {
      setArchive(null);
      return;
    }
    const stack = archive.stack.slice(0, -1);
    void openArchive(stack[stack.length - 1], stack);
  }, [archive, openArchive]);

  const onKeyDown = (event: React.KeyboardEvent) => {
    if (event.key === 'Escape') {
      event.preventDefault();
      if (menu) setMenu(null);
      else if (settingsOpen) setSettingsOpen(false);
      else if (archive) closeArchive();
      else onClose();
      return;
    }
    if (archive || settingsOpen || items.length === 0) return;
    if (event.key === 'ArrowDown') {
      event.preventDefault();
      setSelected((value) => Math.min(items.length - 1, value + 1));
    } else if (event.key === 'ArrowUp') {
      event.preventDefault();
      setSelected((value) => Math.max(0, value - 1));
    } else if (event.key === 'Enter') {
      event.preventDefault();
      const item = items[selected];
      if (item) void openResult(item);
    }
  };

  if (!status || !config) {
    return (
      <section className="file-search-panel" onKeyDown={onKeyDown}>
        <div className="file-search-loading">{t('search.querying')}</div>
      </section>
    );
  }

  if (status.phase === 'disabled') {
    return (
      <section className="file-search-panel" onKeyDown={onKeyDown} aria-label={t('search.title')}>
        <div className="file-search-welcome">
          <div className="file-search-welcome-icon">🔍</div>
          <h2>{t('search.startTitle')}</h2>
          <p>{t('search.startDescription')}</p>
          <div className="file-search-root-list">
            {config.roots.map((root) => (
              <code key={root}>{root}</code>
            ))}
          </div>
          {error && <div className="file-search-error">{error}</div>}
          <div className="file-search-welcome-actions">
            <button className="settings-button" onClick={onClose}>
              {t('common.cancel')}
            </button>
            <button
              className="settings-button primary"
              onClick={() =>
                void api.startFileSearchIndex(true).catch((reason) => setError(String(reason)))
              }
            >
              {t('search.start')}
            </button>
          </div>
        </div>
      </section>
    );
  }

  const currentArchive = archive?.stack[archive.stack.length - 1];
  const archiveName = currentArchive?.split('::').pop()?.split(/[/\\]/).pop() ?? '';

  return (
    <section className="file-search-panel" onKeyDown={onKeyDown} aria-label={t('search.title')}>
      <header className="file-search-header">
        <div className="file-search-input-wrap">
          <span aria-hidden="true">🔍</span>
          <input
            ref={inputRef}
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            placeholder={t('search.placeholder')}
            aria-label={t('search.placeholder')}
            onFocus={(event) => event.currentTarget.select()}
          />
          {query && (
            <button
              className="file-search-input-clear"
              onClick={() => setQuery('')}
              aria-label={t('search.close')}
            >
              ×
            </button>
          )}
        </div>
        <div className="file-search-filters" role="group">
          {(['all', 'log', 'archive'] as const).map((value) => (
            <button
              key={value}
              className={filter === value ? 'active' : ''}
              onClick={() => setFilter(value)}
            >
              {t(`search.filter.${value}`)}
            </button>
          ))}
        </div>
        <button
          className={'icon-btn' + (settingsOpen ? ' active' : '')}
          onClick={() => setSettingsOpen((value) => !value)}
          title={t('search.settings')}
          aria-label={t('search.settings')}
        >
          ⚙️
        </button>
        <button
          className="icon-btn"
          onClick={onClose}
          title={t('search.close')}
          aria-label={t('search.close')}
        >
          ×
        </button>
      </header>

      <div className={'file-search-status ' + status.phase} role="status">
        <span>{statusText}</span>
        {status.phase === 'scanning' && (
          <button onClick={() => void api.pauseFileSearchIndex()}>{t('search.pause')}</button>
        )}
        {status.phase === 'paused' && (
          <button onClick={() => void api.startFileSearchIndex(false)}>{t('search.resume')}</button>
        )}
      </div>

      {settingsOpen && (
        <div className="file-search-settings">
          <h3>{t('search.settings')}</h3>
          <label>{t('search.roots')}</label>
          <div className="file-search-path-list">
            {config.roots.map((root) => (
              <code key={root}>{root}</code>
            ))}
          </div>
          <label>{t('search.exclusions')}</label>
          <div className="file-search-path-list">
            {config.exclusions.length === 0 && <span>{t('search.noExclusions')}</span>}
            {config.exclusions.map((path) => (
              <div key={path}>
                <code>{path}</code>
                <button
                  onClick={() =>
                    void api.setFileSearchExclusions(
                      config.exclusions.filter((value) => value !== path),
                    )
                  }
                  title={t('search.removeExclusion')}
                >
                  ×
                </button>
              </div>
            ))}
          </div>
          <div className="file-search-settings-actions">
            <button
              onClick={() =>
                void api.chooseFileSearchExclusion(t('search.addExclusion')).then((path) => {
                  if (path && !config.exclusions.includes(path))
                    void api.setFileSearchExclusions([...config.exclusions, path]);
                })
              }
            >
              {t('search.addExclusion')}
            </button>
            <button onClick={() => void api.startFileSearchIndex(true)}>
              {t('search.rebuild')}
            </button>
            <button
              className={clearArmed ? 'danger' : ''}
              onClick={() => {
                if (!clearArmed) {
                  setClearArmed(true);
                  window.setTimeout(() => setClearArmed(false), 3000);
                } else {
                  setClearArmed(false);
                  void api.clearFileSearchIndex();
                }
              }}
            >
              {t(clearArmed ? 'search.clearConfirm' : 'search.clear')}
            </button>
          </div>
        </div>
      )}

      {archive ? (
        <div className="file-search-archive">
          <div className="file-search-archive-head">
            <button onClick={closeArchive}>← {t('search.archiveBack')}</button>
            <strong>{t('search.archiveTitle', { name: archiveName })}</strong>
            <span>{t('search.archiveEntryCount', { count: archive.entries.length })}</span>
          </div>
          {archive.loading ? (
            <div className="file-search-empty">{t('search.querying')}</div>
          ) : archive.error ? (
            <div className="file-search-error">{archive.error}</div>
          ) : archive.entries.filter((entry) => entry.isLog || entry.isArchive).length === 0 ? (
            <div className="file-search-empty">{t('search.archiveEmpty')}</div>
          ) : (
            <div className="file-search-archive-list">
              {archive.entries
                .filter((entry) => entry.isLog || entry.isArchive)
                .map((entry) => (
                  <button
                    key={entry.path}
                    disabled={entry.encrypted || (!entry.isLog && !entry.isArchive)}
                    onDoubleClick={() => void openArchiveEntry(entry)}
                    onClick={() => entry.isArchive && void openArchiveEntry(entry)}
                  >
                    <span>{entry.isArchive ? '📦' : '📄'}</span>
                    <span>{entry.path}</span>
                    <span>{fmtSize(entry.size)}</span>
                  </button>
                ))}
            </div>
          )}
        </div>
      ) : (
        <>
          <div className="file-search-columns" aria-hidden="true">
            <span>{t('search.column.name')}</span>
            <span>{t('search.column.path')}</span>
            <span>{t('search.column.size')}</span>
            <span>{t('search.column.modified')}</span>
          </div>
          <div
            ref={scrollRef}
            className="file-search-results"
            role="listbox"
            aria-label={t('search.title')}
          >
            {!query.trim() ? (
              <div className="file-search-empty">{t('search.emptyPrompt')}</div>
            ) : loading ? (
              <div className="file-search-empty">{t('search.querying')}</div>
            ) : error ? (
              <div className="file-search-error">{error}</div>
            ) : items.length === 0 ? (
              <div className="file-search-empty">{t('search.noResults')}</div>
            ) : (
              <div className="file-search-virtual" style={{ height: virtualizer.getTotalSize() }}>
                {virtualizer.getVirtualItems().map((row) => {
                  const item = items[row.index];
                  return (
                    <div
                      key={item.path}
                      className={'file-search-row' + (selected === row.index ? ' selected' : '')}
                      style={{ transform: `translateY(${row.start}px)` }}
                      role="option"
                      aria-selected={selected === row.index}
                      title={item.path}
                      onClick={() => setSelected(row.index)}
                      onDoubleClick={() => void openResult(item)}
                      onContextMenu={(event) => {
                        event.preventDefault();
                        setSelected(row.index);
                        setMenu({ x: event.clientX, y: event.clientY, item });
                      }}
                    >
                      <span className="file-search-name">
                        <span aria-hidden="true">{resultIcon(item)}</span>
                        <span>{highlighted(item.name, query)}</span>
                      </span>
                      <span className="file-search-parent">{highlighted(item.parent, query)}</span>
                      <span>{fmtSize(item.size)}</span>
                      <span>{modifiedText(item.modifiedMs, locale)}</span>
                    </div>
                  );
                })}
              </div>
            )}
          </div>
          <footer className="file-search-footer">
            <span>
              {query.trim() && t('search.summary', { count: fmtNum(total), elapsed })}
              {partial && ` · ${t('search.partial')}`}
            </span>
            {items.length < total && items.length < MAX_VISIBLE_RESULTS && (
              <button disabled={loadingMore} onClick={() => void executeQuery(items.length, true)}>
                {loadingMore ? t('search.querying') : t('search.loadMore')}
              </button>
            )}
          </footer>
        </>
      )}

      {menu && (
        <ContextMenu
          x={menu.x}
          y={menu.y}
          onClose={() => setMenu(null)}
          items={[
            ...(menu.item.isLog || menu.item.isArchive
              ? [{ label: t('search.open'), onClick: () => void openResult(menu.item) }]
              : []),
            { label: t('search.showInManager'), onClick: () => void api.openPath(menu.item.path) },
            { label: t('search.addMonitor'), onClick: () => void addMonitor(menu.item) },
            {
              label: t('search.copyPath'),
              onClick: () => void navigator.clipboard.writeText(menu.item.path),
            },
          ]}
        />
      )}
    </section>
  );
}

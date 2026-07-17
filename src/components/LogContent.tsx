import { useEffect, useRef, useState } from 'react';
import { useVirtualizer } from '@tanstack/react-virtual';
import { api } from '../api';
import type { LogLine, OpenSessionResult } from '../api';
import { fmtNum, fmtSize } from '../util/format';
import { LogRow } from './LogRow';

interface Props {
  session: OpenSessionResult | null;
  activeKey: string | null;
}

const PAGE = 200;
const ENCODINGS = ['UTF-8', 'GBK', 'GB18030', 'UTF-16LE', 'UTF-16BE'];

export function LogContent({ session, activeKey }: Props) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const [percent, setPercent] = useState(100);
  const [indexedLines, setIndexedLines] = useState(0);
  const [totalLines, setTotalLines] = useState(0);
  const [indexing, setIndexing] = useState(false);
  const [effectiveEncoding, setEffectiveEncoding] = useState('Detecting');
  const [detectedEncoding, setDetectedEncoding] = useState('Detecting');
  const [encodingChanging, setEncodingChanging] = useState(false);
  const [encodingPercent, setEncodingPercent] = useState(0);
  // 行缓存:行号 → 内容
  const [cache, setCache] = useState<Map<number, LogLine>>(new Map());
  const [currentLine, setCurrentLine] = useState(1);
  const pending = useRef<Set<number>>(new Set());
  const encodingUnsub = useRef<() => void>(() => {});

  useEffect(
    () => () => {
      encodingUnsub.current();
    },
    [],
  );

  // 打开新条目:重置并按需订阅建索引进度
  useEffect(() => {
    if (!session || !activeKey) return;
    setCache(new Map());
    pending.current = new Set();
    const total = api.lineCount(activeKey);
    setTotalLines(total);
    setIndexedLines(total);
    setEffectiveEncoding(session.encoding);
    setDetectedEncoding(session.encoding);
    setEncodingChanging(false);
    encodingUnsub.current();
    scrollRef.current?.scrollTo({ top: 0 });

    if (session.indexing) {
      setIndexing(true);
      setPercent(0);
      const unsub = api.subscribeIndexProgress(
        activeKey,
        (p) => {
          setPercent(p.percent);
          setIndexedLines(p.indexedLines);
          setTotalLines(p.indexedLines);
          setDetectedEncoding(p.detectedEncoding);
          setEffectiveEncoding(p.effectiveEncoding);
        },
        (finalTotal) => {
          setIndexing(false);
          setPercent(100);
          setIndexedLines(finalTotal);
          setTotalLines(finalTotal);
        },
      );
      return unsub;
    } else {
      setIndexing(false);
      setPercent(100);
      setIndexedLines(total);
    }
  }, [session, activeKey]);

  const rowVirtualizer = useVirtualizer({
    count: totalLines,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => 18,
    overscan: 20,
  });

  // 按可视区批量拉取未缓存的行(窗口化加载)
  const items = rowVirtualizer.getVirtualItems();
  useEffect(() => {
    if (!activeKey || items.length === 0) return;
    const first = items[0].index;
    setCurrentLine(first + 1);
    const start = Math.floor(first / PAGE) * PAGE;
    const last = items[items.length - 1].index;
    const endPage = Math.floor(last / PAGE) * PAGE;
    for (let p = start; p <= endPage; p += PAGE) {
      const pageLast = Math.min(p + PAGE - 1, totalLines - 1);
      if (pending.current.has(p) || cache.has(pageLast)) continue;
      pending.current.add(p);
      api
        .readLines(activeKey, p, PAGE)
        .then((lines) => {
          setCache((prev) => {
            const next = new Map(prev);
            for (const l of lines) next.set(l.lineNo - 1, l);
            return next;
          });
        })
        .finally(() => pending.current.delete(p));
    }
  }, [items, activeKey, cache, totalLines]);

  async function changeEncoding(encoding: string) {
    if (!activeKey || encoding === effectiveEncoding) return;
    encodingUnsub.current();
    setEncodingChanging(true);
    setEncodingPercent(0);
    try {
      const generation = await api.setSessionEncoding(activeKey, encoding);
      encodingUnsub.current = api.subscribeEncodingProgress(activeKey, generation, (progress) => {
        setEncodingPercent(progress.percent);
        if (!progress.done) return;
        setEncodingChanging(false);
        if (progress.failed) {
          alert(`切换编码失败: ${progress.error ?? '未知错误'}`);
          return;
        }
        setEffectiveEncoding(progress.encoding);
        setTotalLines(progress.lineCount);
        setIndexedLines(progress.lineCount);
        setCache(new Map());
        pending.current = new Set();
        scrollRef.current?.scrollTo({ top: 0 });
      });
    } catch (error) {
      setEncodingChanging(false);
      alert(`切换编码失败: ${String(error)}`);
    }
  }

  if (!session && !activeKey) {
    return (
      <div className="col col-content">
        <div className="empty-state">
          <div className="big">📄</div>
          <div className="desc">从左侧选择一个日志条目查看内容</div>
        </div>
      </div>
    );
  }

  return (
    <div className="col col-content">
      <div className="content-head">
        {session ? (
          <>
            {session.entryPath.split(' › ').map((p, i, arr) => (
              <span key={i}>
                <span className={i === arr.length - 1 ? 'crumb-file' : ''}>{p}</span>
                {i < arr.length - 1 && <span className="crumb-sep"> › </span>}
              </span>
            ))}
          </>
        ) : (
          <span>打开中…</span>
        )}
      </div>

      {indexing && (
        <div className="index-bar">
          <span>解压+建索引 {percent}%</span>
          <div className="track">
            <div className="fill" style={{ width: `${percent}%` }} />
          </div>
          <span>已可读 1–{fmtNum(indexedLines)} 行</span>
        </div>
      )}

      {encodingChanging && (
        <div className="index-bar">
          <span>重建编码索引 {encodingPercent}%</span>
          <div className="track">
            <div className="fill" style={{ width: `${encodingPercent}%` }} />
          </div>
          <span>{effectiveEncoding}</span>
        </div>
      )}

      <div className="log-view" ref={scrollRef}>
        <div
          style={{
            height: rowVirtualizer.getTotalSize(),
            position: 'relative',
            minWidth: 'max-content',
          }}
        >
          {items.map((vi) => {
            const line = cache.get(vi.index);
            const ready = vi.index < indexedLines || !indexing;
            return (
              <LogRow
                key={vi.index}
                top={vi.start}
                lineNo={vi.index + 1}
                line={line}
                ready={ready}
              />
            );
          })}
        </div>
      </div>

      <div className="col-foot" style={{ display: 'flex', gap: 16 }}>
        <select
          className="encoding-select"
          value={effectiveEncoding}
          disabled={indexing || encodingChanging}
          title={`自动检测: ${detectedEncoding}`}
          onChange={(event) => void changeEncoding(event.target.value)}
        >
          {effectiveEncoding === 'Detecting' && <option value="Detecting">检测中…</option>}
          {ENCODINGS.map((encoding) => (
            <option key={encoding} value={encoding}>
              {encoding}
            </option>
          ))}
        </select>
        <span>
          行 {fmtNum(currentLine)}/{fmtNum(totalLines)}
        </span>
        <span style={{ marginLeft: 'auto' }}>{fmtSize(session?.size)}</span>
      </div>
    </div>
  );
}

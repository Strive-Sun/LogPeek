import { useEffect, useState } from 'react';
import { api } from '../api';
import type { ArchiveEntry } from '../api';
import { fmtSize } from '../util/format';

interface Props {
  archive: string | null;
  activeKey: string | null;
  passesFilter: (n: { name: string; kind: string; isLog?: boolean }) => boolean;
  onOpenEntry: (archive: string, entryPath: string) => void;
}

export function EntryList(props: Props) {
  const { archive } = props;
  const [entries, setEntries] = useState<ArchiveEntry[]>([]);
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    if (!archive) {
      setEntries([]);
      return;
    }
    setLoading(true);
    let alive = true;
    api.listArchiveEntries(archive).then((es) => {
      if (alive) {
        setEntries(es);
        setLoading(false);
      }
    });
    return () => {
      alive = false;
    };
  }, [archive]);

  const visible = entries.filter((e) =>
    props.passesFilter({ name: e.path, kind: 'file', isLog: e.isLog }),
  );
  const logCount = visible.filter((e) => e.isLog).length;

  return (
    <div className="col col-entries">
      <div className="col-head">{archive ?? '条目列表'}</div>
      <div className="col-body">
        {!archive && (
          <div className="entry-row" style={{ color: 'var(--fg-dim)', cursor: 'default' }}>
            选择左侧压缩包查看条目
          </div>
        )}
        {loading && (
          <div className="entry-row" style={{ color: 'var(--fg-dim)', cursor: 'default' }}>
            读取中央目录…
          </div>
        )}
        {visible.map((e) => {
          const key = `${archive}::${e.path}`;
          return (
            <div
              key={key}
              className={'entry-row' + (props.activeKey === key ? ' selected' : '')}
              onClick={() => e.isLog && archive && props.onOpenEntry(archive, e.path)}
              title={e.encrypted ? '加密条目(暂不支持)' : e.isLog ? '' : '非日志文件'}
            >
              <span className="ico">{e.isLog ? '📄' : '⬡'}</span>
              <span className={'name' + (e.isLog ? '' : ' notlog')}>{e.path}</span>
              <span className="size">{fmtSize(e.size)}</span>
            </div>
          );
        })}
      </div>
      {archive && (
        <div className="col-foot">
          {visible.length} 个条目 · {logCount} 个日志
        </div>
      )}
    </div>
  );
}

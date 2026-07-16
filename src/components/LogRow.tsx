import { memo } from 'react';
import type { LogLine } from '../api';
import { detectLevel } from '../util/format';

interface Props {
  top: number;
  lineNo: number;
  line: LogLine | undefined;
  ready: boolean;
}

/** 单行日志:行号固定在左、内容可横向滚动、级别着色、截断标记 */
export const LogRow = memo(function LogRow({ top, lineNo, line, ready }: Props) {
  const lvl = line ? detectLevel(line.content) : null;
  return (
    <div
      className="log-line"
      style={{ position: 'absolute', top, left: 0, right: 0, height: 18 }}
    >
      <span className="ln">{lineNo}</span>
      <span className="txt">
        {line ? (
          <span className={lvl ? `lvl-${lvl}` : undefined}>{line.content}</span>
        ) : ready ? (
          <span style={{ color: 'var(--fg-faint)' }}>加载中…</span>
        ) : (
          <span style={{ color: 'var(--fg-faint)' }}>解压中…(该行尚未建索引)</span>
        )}
        {line?.truncated && <span className="trunc-tag">已截断</span>}
      </span>
    </div>
  );
});

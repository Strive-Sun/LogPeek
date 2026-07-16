/** 人类可读的字节大小 */
export function fmtSize(bytes?: number): string {
  if (bytes === undefined) return '';
  const units = ['B', 'KB', 'MB', 'GB'];
  let v = bytes;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  const s = v >= 100 || i === 0 ? v.toFixed(0) : v.toFixed(1);
  return `${s}${units[i]}`;
}

/** 千分位 */
export function fmtNum(n: number): string {
  return n.toLocaleString('en-US');
}

/** 从日志行中提取日志级别(用于着色) */
export function detectLevel(line: string): string | null {
  const m = line.match(/\b(ERROR|WARN|INFO|DEBUG|TRACE|FATAL)\b/);
  return m ? m[1] : null;
}

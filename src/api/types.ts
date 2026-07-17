// 与技术设计文档 4.3 / 4.6 的后端契约对齐的前端类型

/** 归档内的一个条目(不含内容,仅元信息) */
export interface ArchiveEntry {
  /** 包内路径 */
  path: string;
  /** 解压后大小(字节) */
  size: number;
  /** 是否日志/文本 */
  isLog: boolean;
  /** 是否加密条目(M1 不支持,列出但标记) */
  encrypted: boolean;
}

/** 一行返回给前端的内容 */
export interface LogLine {
  /** 行号(从 1 起) */
  lineNo: number;
  /** 已解码为 UTF-8 的行内容(可能被截断) */
  content: string;
  /** 是否因超过阈值被后端截断 */
  truncated: boolean;
}

/** 监控目录树中的节点类型 */
export type NodeKind = 'dir' | 'archive' | 'file';

/** 目录树节点 */
export interface TreeNode {
  id: string;
  name: string;
  kind: NodeKind;
  /** 文件/条目大小(字节),目录为 undefined */
  size?: number;
  /** 是否日志文件(archive 节点恒为 true;file 视扩展名/采样) */
  isLog?: boolean;
  /** 磁盘绝对路径(用于重命名/删除等文件操作) */
  path?: string;
  /** 来源监控目录路径 */
  watchDir?: string;
  /** 是否为未读的新到达项 */
  unread?: boolean;
  /** 子节点;archive 节点在展开时惰性填充 */
  children?: TreeNode[];
}

/** 新日志提示项 */
export interface NewLogItem {
  id: string;
  name: string;
  kind: 'archive' | 'file';
  /** 来源目录短名 */
  source: string;
  /** 到达距今(如 "2m") */
  age: string;
}

/** 建索引进度事件载荷 */
export interface IndexProgress {
  sessionId: string;
  percent: number;
  indexedLines: number;
  done: boolean;
  failed: boolean;
  detectedEncoding: string;
  effectiveEncoding: string;
  error?: string;
}

/** 手动编码重建进度事件载荷 */
export interface EncodingProgress {
  sessionId: string;
  generation: number;
  percent: number;
  encoding: string;
  lineCount: number;
  done: boolean;
  failed: boolean;
  error?: string;
}

/** 打开会话的结果 */
export interface OpenSessionResult {
  sessionId: string;
  /** 条目路径(用于面包屑) */
  entryPath: string;
  /** 解压后大小 */
  size: number;
  /** 是否需要后台解压/建索引(压缩条目 / 大文件) */
  indexing: boolean;
  /** 检测到的编码 */
  encoding: string;
}

/** 后端到达检测事件载荷 */
export interface DetectedItem {
  path: string;
  name: string;
  kind: 'archive' | 'file';
  size: number;
  source: string;
}

/** 后缀筛选规则 */
export interface FilterRule {
  /** 勾选启用的后缀 */
  suffixes: string[];
  /** 是否显示全部(含非日志) */
  showAll: boolean;
}

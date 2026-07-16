import { createContext, useContext, useState } from 'react';
import { api } from '../api';
import type { ArchiveEntry, TreeNode } from '../api';
import { fmtSize } from '../util/format';
import { SuffixFilter } from './SuffixFilter';
import { ContextMenu } from './ContextMenu';

interface Props {
  nodes: TreeNode[];
  activeKey: string | null;
  selectedArchive: string | null;
  width?: number;
  unreadIds: Set<string>;
  filter: string[];
  showAll: boolean;
  passesFilter: (n: { name: string; kind: string; isLog?: boolean }) => boolean;
  onFilterChange: (f: string[]) => void;
  onShowAllChange: (v: boolean) => void;
  onSelectArchive: (name: string, unreadId?: string) => void;
  onOpenFile: (name: string, unreadId?: string) => void;
  onAddDir: () => void;
  /** 重命名磁盘文件(同目录);成功后应触发刷新 */
  onRename: (path: string, newName: string) => Promise<void>;
  /** 删除文件(移入回收站);由上层弹确认并刷新 */
  onDelete: (node: TreeNode) => void;
}

// 让深层 TreeItem 能触发右键菜单与重命名,而无需逐层透传
interface TreeCtx {
  openMenu: (e: React.MouseEvent, node: TreeNode) => void;
  renamingId: string | null;
  startRename: (node: TreeNode) => void;
  commitRename: (node: TreeNode, name: string) => void;
  cancelRename: () => void;
}
const TreeContext = createContext<TreeCtx | null>(null);

/** 节点是否为未读的新到达项(id 即文件路径) */
function isUnread(node: TreeNode, unreadIds: Set<string>): boolean {
  return unreadIds.has(node.id);
}

/** 递归判断:目录(或其子孙)是否含未读的新到达项 */
function hasUnreadDescendant(node: TreeNode, unreadIds: Set<string>): boolean {
  return !!node.children?.some(
    (c) => isUnread(c, unreadIds) || hasUnreadDescendant(c, unreadIds),
  );
}

export function DirTree(props: Props) {
  const [menu, setMenu] = useState<{ x: number; y: number; node: TreeNode } | null>(null);
  const [renamingId, setRenamingId] = useState<string | null>(null);

  const ctx: TreeCtx = {
    openMenu: (e, node) => {
      e.preventDefault();
      e.stopPropagation();
      setMenu({ x: e.clientX, y: e.clientY, node });
    },
    renamingId,
    startRename: (node) => setRenamingId(node.id),
    commitRename: (node, name) => {
      setRenamingId(null);
      const trimmed = name.trim();
      if (trimmed && trimmed !== node.name) void props.onRename(node.path ?? node.id, trimmed);
    },
    cancelRename: () => setRenamingId(null),
  };

  return (
    <div className="col col-tree" style={props.width ? { width: props.width } : undefined}>
      <div className="col-head">
        <span>监控目录</span>
        <SuffixFilter
          filter={props.filter}
          showAll={props.showAll}
          onFilterChange={props.onFilterChange}
          onShowAllChange={props.onShowAllChange}
        />
      </div>
      <TreeContext.Provider value={ctx}>
        <div className="col-body">
          {props.nodes.map((n) => (
            <TreeItem key={n.id} node={n} depth={0} {...props} />
          ))}
        </div>
      </TreeContext.Provider>
      <button className="add-dir-btn" onClick={props.onAddDir}>
        + 添加监控目录
      </button>
      {menu && (
        <ContextMenu
          x={menu.x}
          y={menu.y}
          onClose={() => setMenu(null)}
          items={[
            { label: '重命名', onClick: () => ctx.startRename(menu.node) },
            { label: '删除', danger: true, onClick: () => props.onDelete(menu.node) },
          ]}
        />
      )}
    </div>
  );
}

function TreeItem(props: Props & { node: TreeNode; depth: number }) {
  const { node, depth } = props;
  const tree = useContext(TreeContext);
  const [open, setOpen] = useState(node.kind === 'dir');
  // 压缩包展开时惰性拉取的子条目
  const [entries, setEntries] = useState<ArchiveEntry[] | null>(null);
  const [loading, setLoading] = useState(false);

  const pad = { paddingLeft: 10 + depth * 14 };

  const toggleArchive = async () => {
    const next = !open;
    setOpen(next);
    props.onSelectArchive(node.name, props.unreadIds.has(node.id) ? node.id : undefined);
    if (next && entries === null) {
      setLoading(true);
      const es = await api.listArchiveEntries(node.name); // 只读中央目录,不解压
      setEntries(es);
      setLoading(false);
    }
  };

  const unread = isUnread(node, props.unreadIds);
  const renaming = tree?.renamingId === node.id;

  // 重命名中显示内联输入框,否则显示文件名
  const renderLabel = (extraClass = '') =>
    renaming ? (
      <input
        className="rename-input"
        autoFocus
        defaultValue={node.name}
        onClick={(e) => e.stopPropagation()}
        onKeyDown={(e) => {
          if (e.key === 'Enter') tree?.commitRename(node, (e.target as HTMLInputElement).value);
          else if (e.key === 'Escape') tree?.cancelRename();
        }}
        onBlur={(e) => tree?.commitRename(node, e.target.value)}
      />
    ) : (
      <span className={'label' + extraClass}>{node.name}</span>
    );

  if (node.kind === 'dir') {
    const dirHasNew = hasUnreadDescendant(node, props.unreadIds);
    return (
      <div>
        <div
          className={'tree-node' + (dirHasNew ? ' new-dir' : '')}
          style={pad}
          onClick={() => setOpen((v) => !v)}
        >
          <span className="twisty">{open ? '▾' : '▸'}</span>
          <span className="ico">📁</span>
          <span className="label">{node.name}</span>
        </div>
        {open &&
          node.children?.map((c) => <TreeItem key={c.id} {...props} node={c} depth={depth + 1} />)}
      </div>
    );
  }

  if (node.kind === 'archive') {
    return (
      <div>
        <div
          className={
            'tree-node' +
            (props.selectedArchive === node.name ? ' selected' : '') +
            (unread ? ' new-file' : '')
          }
          style={pad}
          onClick={toggleArchive}
          onContextMenu={(e) => tree?.openMenu(e, node)}
        >
          <span className="twisty">{open ? '▾' : '▸'}</span>
          <span className="ico">📦</span>
          {renderLabel()}
          {unread && !renaming && <span className="dot-unread" />}
        </div>
        {open && loading && (
          <div className="tree-node" style={{ paddingLeft: 10 + (depth + 1) * 14, color: 'var(--fg-dim)' }}>
            读取清单…
          </div>
        )}
        {open &&
          entries
            ?.filter((e) => props.passesFilter({ name: e.path, kind: 'file', isLog: e.isLog }))
            .map((e) => {
              const key = `${node.name}::${e.path}`;
              return (
                <div
                  key={key}
                  className={'tree-node' + (props.activeKey === key ? ' selected' : '')}
                  style={{ paddingLeft: 10 + (depth + 1) * 14 }}
                  onClick={() => e.isLog && props.onOpenFile(key)}
                  title={e.encrypted ? '加密条目(暂不支持)' : e.isLog ? '' : '非日志文件'}
                >
                  <span className="twisty" />
                  <span className="ico">{e.isLog ? '📄' : '⬡'}</span>
                  <span className={'label' + (e.isLog ? '' : ' notlog')}>{e.path}</span>
                  <span className="size">{fmtSize(e.size)}</span>
                </div>
              );
            })}
      </div>
    );
  }

  // 裸文本文件:叶子节点
  return (
    <div
      className={
        'tree-node' +
        (props.activeKey === node.name ? ' selected' : '') +
        (unread ? ' new-file' : '')
      }
      style={pad}
      onClick={() => props.onOpenFile(node.name, unread ? node.id : undefined)}
      onContextMenu={(e) => tree?.openMenu(e, node)}
    >
      <span className="twisty" />
      <span className="ico">{node.isLog === false ? '⬡' : '📄'}</span>
      {renderLabel(node.isLog === false ? ' notlog' : '')}
      {unread && !renaming && <span className="dot-unread" />}
    </div>
  );
}

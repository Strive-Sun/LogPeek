import { useState } from 'react';
import { api } from '../api';
import type { ArchiveEntry, TreeNode } from '../api';
import { fmtSize } from '../util/format';

interface Props {
  nodes: TreeNode[];
  activeKey: string | null;
  selectedArchive: string | null;
  passesFilter: (n: { name: string; kind: string; isLog?: boolean }) => boolean;
  onSelectArchive: (name: string, unreadId?: string) => void;
  onOpenFile: (name: string, unreadId?: string) => void;
  onAddDir: () => void;
}

export function DirTree(props: Props) {
  return (
    <div className="col col-tree">
      <div className="col-head">监控目录</div>
      <div className="col-body">
        {props.nodes.map((n) => (
          <TreeItem key={n.id} node={n} depth={0} {...props} />
        ))}
      </div>
      <button className="add-dir-btn" onClick={props.onAddDir}>
        + 添加监控目录
      </button>
    </div>
  );
}

function TreeItem(props: Props & { node: TreeNode; depth: number }) {
  const { node, depth } = props;
  const [open, setOpen] = useState(node.kind === 'dir');
  // 压缩包展开时惰性拉取的子条目
  const [entries, setEntries] = useState<ArchiveEntry[] | null>(null);
  const [loading, setLoading] = useState(false);

  const pad = { paddingLeft: 10 + depth * 14 };

  const toggleArchive = async () => {
    const next = !open;
    setOpen(next);
    props.onSelectArchive(node.name, node.unread ? node.id : undefined);
    if (next && entries === null) {
      setLoading(true);
      const es = await api.listArchiveEntries(node.name); // 只读中央目录,不解压
      setEntries(es);
      setLoading(false);
    }
  };

  if (node.kind === 'dir') {
    return (
      <div>
        <div className="tree-node" style={pad} onClick={() => setOpen((v) => !v)}>
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
          className={'tree-node' + (props.selectedArchive === node.name ? ' selected' : '')}
          style={pad}
          onClick={toggleArchive}
        >
          <span className="twisty">{open ? '▾' : '▸'}</span>
          <span className="ico">📦</span>
          <span className="label">{node.name}</span>
          {node.unread && <span className="dot-unread" />}
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
      className={'tree-node' + (props.activeKey === node.name ? ' selected' : '')}
      style={pad}
      onClick={() => props.onOpenFile(node.name, node.unread ? node.id : undefined)}
    >
      <span className="twisty" />
      <span className="ico">{node.isLog === false ? '⬡' : '📄'}</span>
      <span className={'label' + (node.isLog === false ? ' notlog' : '')}>{node.name}</span>
      {node.unread && <span className="dot-unread" />}
    </div>
  );
}

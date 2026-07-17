import type { DirectoryChangeBatch, TreeNode } from '../api/types';

function compareNodes(a: TreeNode, b: TreeNode): number {
  if (a.kind === 'dir' && b.kind !== 'dir') return -1;
  if (a.kind !== 'dir' && b.kind === 'dir') return 1;
  const byName = a.name.toLocaleLowerCase().localeCompare(b.name.toLocaleLowerCase());
  return byName || a.id.localeCompare(b.id);
}

function prepareNode(node: TreeNode, directory: TreeNode, previous?: TreeNode): TreeNode {
  return {
    ...previous,
    ...node,
    id: node.path ?? node.id,
    watchDir: directory.name,
    children: node.children ?? previous?.children,
  };
}

/** 将一个后端变化批次应用到单个监控目录，未受影响目录保持引用不变。 */
export function applyDirectoryChanges(
  tree: readonly TreeNode[],
  batch: DirectoryChangeBatch,
): TreeNode[] {
  function update(directory: TreeNode): TreeNode {
    if (directory.id !== batch.watchDir && directory.path !== batch.watchDir) {
      if (!directory.children) return directory;
      let childChanged = false;
      const children = directory.children.map((child) => {
        if (child.kind !== 'dir') return child;
        const next = update(child);
        if (next !== child) childChanged = true;
        return next;
      });
      return childChanged ? { ...directory, children } : directory;
    }
    let children = [...(directory.children ?? [])];

    for (const change of batch.changes) {
      if (change.type === 'rescan') {
        const previous = new Map(children.map((node) => [node.id, node]));
        children = change.nodes.map((node) =>
          prepareNode(node, directory, previous.get(node.path ?? node.id)),
        );
        continue;
      }
      if (change.type === 'remove') {
        children = children.filter((node) => node.id !== change.path && node.path !== change.path);
        continue;
      }
      if (change.type === 'rename') {
        children = children.filter(
          (node) => node.id !== change.oldPath && node.path !== change.oldPath,
        );
      }
      const incoming = change.node;
      const id = incoming.path ?? incoming.id;
      const index = children.findIndex((node) => node.id === id || node.path === id);
      const node = prepareNode(incoming, directory, index >= 0 ? children[index] : undefined);
      if (index >= 0) children[index] = node;
      else children.push(node);
    }

    children.sort(compareNodes);
    return { ...directory, children };
  }

  return tree.map(update);
}

function findDirectory(tree: readonly TreeNode[], path: string): TreeNode | undefined {
  for (const node of tree) {
    if (node.id === path || node.path === path) return node;
    const nested = node.children ? findDirectory(node.children, path) : undefined;
    if (nested) return nested;
  }
  return undefined;
}

/** 返回一个批次应用后从受影响目录消失的旧顶层节点。 */
export function removedDirectoryNodes(
  before: readonly TreeNode[],
  after: readonly TreeNode[],
  watchDir: string,
): TreeNode[] {
  const oldDir = findDirectory(before, watchDir);
  const newDir = findDirectory(after, watchDir);
  const remaining = new Set((newDir?.children ?? []).map((node) => node.id));
  return (oldDir?.children ?? []).filter((node) => !remaining.has(node.id));
}

export function passesDirectoryFilter(
  node: { name: string; kind: string },
  suffixes: readonly string[],
  showAll: boolean,
): boolean {
  if (node.kind === 'dir' || node.kind === 'archive' || showAll) return true;
  const lower = node.name.toLocaleLowerCase();
  return suffixes.some((suffix) => lower.endsWith(suffix.toLocaleLowerCase()));
}

import type { TabLayout } from './logTabs';

export const WORKSPACE_STORAGE_KEY = 'logcrate.workspace.v1';
const WORKSPACE_VERSION = 1;
const MAX_WORKSPACE_TABS = 50;
const MAX_ENTRY_KEY_LENGTH = 4096;

interface StorageLike {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

interface WorkspaceSnapshot {
  version: number;
  visible: string[];
  overflow: string[];
  active: string | null;
}

export interface SourceChangePrompt {
  sourcePath: string;
  kind: 'modified' | 'deleted' | 'deletedWhileClosed';
  revision?: string;
  /** 删除时优先导出的、仍持有后端会话缓存的选项卡。 */
  entryKey?: string;
}

export function mergeSourceChangePrompt(
  queue: readonly SourceChangePrompt[],
  prompt: SourceChangePrompt,
  samePath: (left: string, right: string) => boolean,
): SourceChangePrompt[] {
  const index = queue.findIndex((item) => samePath(item.sourcePath, prompt.sourcePath));
  if (index < 0) return [...queue, prompt];
  const next = [...queue];
  const existing = next[index];
  if (prompt.kind !== 'modified' || existing.kind === 'modified') next[index] = prompt;
  return next;
}

function validKey(value: unknown): value is string {
  return typeof value === 'string' && value.length > 0 && value.length <= MAX_ENTRY_KEY_LENGTH;
}

export function sourcePathForEntryKey(entryKey: string): string {
  return entryKey.split('::', 1)[0];
}

export function resolvedSourcePath(entryKey: string, actualSourcePath?: string): string {
  return actualSourcePath || sourcePathForEntryKey(entryKey);
}

export function loadWorkspace(storage: StorageLike, capacity = 4): TabLayout {
  try {
    const raw = storage.getItem(WORKSPACE_STORAGE_KEY);
    if (!raw) return { visible: [], overflow: [], active: null, capacity };
    const parsed = JSON.parse(raw) as Partial<WorkspaceSnapshot>;
    if (parsed.version !== WORKSPACE_VERSION) {
      return { visible: [], overflow: [], active: null, capacity };
    }
    const seen = new Set<string>();
    const collect = (values: unknown) => {
      if (!Array.isArray(values)) return [];
      const result: string[] = [];
      for (const value of values) {
        if (!validKey(value) || seen.has(value) || seen.size >= MAX_WORKSPACE_TABS) continue;
        seen.add(value);
        result.push(value);
      }
      return result;
    };
    const visible = collect(parsed.visible);
    const overflow = collect(parsed.overflow);
    const active =
      validKey(parsed.active) && seen.has(parsed.active)
        ? parsed.active
        : (visible[0] ?? overflow[0] ?? null);
    return { visible, overflow, active, capacity: Math.max(1, capacity) };
  } catch {
    return { visible: [], overflow: [], active: null, capacity: Math.max(1, capacity) };
  }
}

export function saveWorkspace(
  storage: StorageLike,
  layout: TabLayout,
  canRestore: (entryKey: string) => boolean = () => true,
): void {
  try {
    const seen = new Set<string>();
    const filter = (values: readonly string[]) =>
      values.filter((value) => {
        if (
          !validKey(value) ||
          seen.has(value) ||
          seen.size >= MAX_WORKSPACE_TABS ||
          !canRestore(value)
        ) {
          return false;
        }
        seen.add(value);
        return true;
      });
    const visible = filter(layout.visible);
    const overflow = filter(layout.overflow);
    const active =
      layout.active && seen.has(layout.active)
        ? layout.active
        : (visible[0] ?? overflow[0] ?? null);
    storage.setItem(
      WORKSPACE_STORAGE_KEY,
      JSON.stringify({ version: WORKSPACE_VERSION, visible, overflow, active }),
    );
  } catch {
    // Storage can be unavailable in private WebViews; workspace persistence is best effort.
  }
}

export function removeWorkspaceTabs(layout: TabLayout, removedIds: ReadonlySet<string>): TabLayout {
  const visible = layout.visible.filter((id) => !removedIds.has(id));
  const overflow = layout.overflow.filter((id) => !removedIds.has(id));
  const active =
    layout.active && !removedIds.has(layout.active)
      ? layout.active
      : (visible[0] ?? overflow[0] ?? null);
  return { ...layout, visible, overflow, active };
}

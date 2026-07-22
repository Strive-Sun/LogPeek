export interface TabLayout {
  visible: string[];
  overflow: string[];
  active: string | null;
  capacity: number;
}

export function emptyTabLayout(capacity = 1): TabLayout {
  return { visible: [], overflow: [], active: null, capacity: Math.max(1, capacity) };
}

export function activateTab(layout: TabLayout, id: string): TabLayout {
  if (layout.visible.includes(id)) return { ...layout, active: id };
  const overflowIndex = layout.overflow.indexOf(id);
  if (overflowIndex < 0) return layout;

  const visible = [...layout.visible];
  const overflow = [...layout.overflow];
  const displaced = visible.pop();
  visible.push(id);
  overflow.splice(overflowIndex, 1, ...(displaced ? [displaced] : []));
  return { ...layout, visible, overflow, active: id };
}

export function openTab(layout: TabLayout, id: string): TabLayout {
  if (layout.visible.includes(id) || layout.overflow.includes(id)) return activateTab(layout, id);
  if (layout.visible.length < layout.capacity) {
    return { ...layout, visible: [...layout.visible, id], active: id };
  }

  const visible = [...layout.visible];
  const displaced = visible.pop();
  visible.push(id);
  return {
    ...layout,
    visible,
    overflow: displaced ? [...layout.overflow, displaced] : layout.overflow,
    active: id,
  };
}

export function resizeTabs(layout: TabLayout, requestedCapacity: number): TabLayout {
  const capacity = Math.max(1, requestedCapacity);
  const visible = [...layout.visible];
  const overflow = [...layout.overflow];

  while (visible.length > capacity) {
    let index = visible.length - 1;
    while (index >= 0 && visible[index] === layout.active) index--;
    if (index < 0) break;
    overflow.push(visible.splice(index, 1)[0]);
  }
  while (visible.length < capacity && overflow.length > 0) {
    visible.push(overflow.shift()!);
  }
  // 布局与容量均未变化时返回原对象,保持引用稳定,避免触发无谓的重渲染循环
  if (
    layout.capacity === capacity &&
    sameOrder(layout.visible, visible) &&
    sameOrder(layout.overflow, overflow)
  ) {
    return layout;
  }
  return { ...layout, capacity, visible, overflow };
}

function sameOrder(a: readonly string[], b: readonly string[]): boolean {
  return a.length === b.length && a.every((value, index) => value === b[index]);
}

export function closeTab(layout: TabLayout, id: string): TabLayout {
  const visibleIndex = layout.visible.indexOf(id);
  if (visibleIndex < 0) {
    if (!layout.overflow.includes(id)) return layout;
    return { ...layout, overflow: layout.overflow.filter((tabId) => tabId !== id) };
  }

  const visible = [...layout.visible];
  const overflow = [...layout.overflow];
  visible.splice(visibleIndex, 1);
  let active = layout.active;
  if (active === id) active = visible[visibleIndex] ?? visible[visibleIndex - 1] ?? null;
  if (visible.length < layout.capacity && overflow.length > 0) {
    const promoted = overflow.shift()!;
    visible.push(promoted);
    if (!active) active = promoted;
  }
  return { ...layout, visible, overflow, active };
}

export function tabIds(layout: TabLayout): string[] {
  return [...layout.visible, ...layout.overflow];
}

export function markEvictedSessions<
  T extends { session: { sessionId: string } | null; status: string },
>(tabs: Record<string, T>, evictedSessionIds: readonly string[]): Record<string, T> {
  const evicted = new Set(evictedSessionIds);
  let changed = false;
  const next = { ...tabs };
  for (const [id, tab] of Object.entries(tabs)) {
    if (!tab.session || !evicted.has(tab.session.sessionId)) continue;
    next[id] = { ...tab, session: null, status: 'dormant' } as T;
    changed = true;
  }
  return changed ? next : tabs;
}

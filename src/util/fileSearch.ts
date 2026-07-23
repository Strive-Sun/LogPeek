import type { DroppedFileInfo, FileSearchResult, FileSearchStatus } from '../api';

export type SearchOpenAction = 'archive' | 'log' | 'reveal';

export function planSearchOpen(info: DroppedFileInfo): SearchOpenAction {
  if (info.kind === 'archive') return 'archive';
  if (info.isLog) return 'log';
  return 'reveal';
}

export function shouldApplySearchResponse(requestGeneration: number, currentGeneration: number) {
  return requestGeneration === currentGeneration;
}

export function searchProgressRefreshKey(status: FileSearchStatus | null): string {
  if (!status) return 'loading';
  const indexedBucket = Math.floor(status.indexedFiles / 100_000);
  const providers = status.providers.map((item) => `${item.root}:${item.phase}`).join('|');
  return `${status.phase}:${indexedBucket}:${providers}`;
}

export function mergeSearchResults(
  current: readonly FileSearchResult[],
  incoming: readonly FileSearchResult[],
  append: boolean,
  maximum: number,
): FileSearchResult[] {
  const source = append ? [...current, ...incoming] : [...incoming];
  const seen = new Set<string>();
  return source
    .filter((item) => {
      const key = navigatorPlatformPathKey(item.path);
      if (seen.has(key)) return false;
      seen.add(key);
      return true;
    })
    .slice(0, Math.max(0, maximum));
}

function navigatorPlatformPathKey(path: string): string {
  return /^[a-z]:[/\\]/i.test(path) ? path.replace(/\//g, '\\').toLocaleLowerCase() : path;
}

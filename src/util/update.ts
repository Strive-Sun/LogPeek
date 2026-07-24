import type { AppUpdateInfo } from '../api/types';

export const AUTO_CHECK_STORAGE_KEY = 'logcrate.update.autoCheck';
export const SKIPPED_VERSION_STORAGE_KEY = 'logcrate.update.skippedVersion';

export type UpdateStatus =
  | 'idle'
  | 'checking'
  | 'up-to-date'
  | 'available'
  | 'downloading'
  | 'installing'
  | 'installed'
  | 'error';

export function loadAutoCheck(storage: Pick<Storage, 'getItem'>): boolean {
  try {
    return storage.getItem(AUTO_CHECK_STORAGE_KEY) !== 'false';
  } catch {
    return true;
  }
}

export function loadSkippedVersion(storage: Pick<Storage, 'getItem'>): string | null {
  try {
    const value = storage.getItem(SKIPPED_VERSION_STORAGE_KEY)?.trim();
    return value || null;
  } catch {
    return null;
  }
}

export function saveAutoCheck(storage: Pick<Storage, 'setItem'>, enabled: boolean): void {
  try {
    storage.setItem(AUTO_CHECK_STORAGE_KEY, String(enabled));
  } catch {
    // WebView 存储不可用时仍保留当前进程内设置。
  }
}

export function saveSkippedVersion(storage: Pick<Storage, 'setItem'>, version: string): void {
  try {
    storage.setItem(SKIPPED_VERSION_STORAGE_KEY, version);
  } catch {
    // WebView 存储不可用时仍在当前进程内抑制重复提示。
  }
}

export function downloadPercent(downloadedBytes: number, totalBytes?: number): number | undefined {
  if (!totalBytes || totalBytes <= 0) return undefined;
  return Math.min(99, Math.max(0, Math.round((downloadedBytes / totalBytes) * 100)));
}

export function shouldPromptAutomatically(
  update: AppUpdateInfo,
  skippedVersion: string | null,
): boolean {
  return update.version !== skippedVersion;
}

export function classifyUpdateCheck(
  update: AppUpdateInfo | null,
  automatic: boolean,
  skippedVersion: string | null,
): 'up-to-date' | 'skipped' | 'available' {
  if (!update) return 'up-to-date';
  if (automatic && !shouldPromptAutomatically(update, skippedVersion)) return 'skipped';
  return 'available';
}

export function errorMessage(error: unknown): string {
  if (error instanceof Error) return error.message;
  return String(error);
}

export function updateFailureMessage(phase: 'downloading' | 'installing', error: unknown): string {
  void phase;
  return errorMessage(error);
}

export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

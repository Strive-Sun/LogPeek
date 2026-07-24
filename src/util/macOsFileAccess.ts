const ONBOARDING_KEY = 'logcrate.macosFileAccessOnboarding';

export function shouldShowMacOsFileAccessOnboarding(
  storage: Pick<Storage, 'getItem'>,
  version: number,
): boolean {
  return storage.getItem(ONBOARDING_KEY) !== String(version);
}

export function markMacOsFileAccessOnboardingSeen(
  storage: Pick<Storage, 'setItem'>,
  version: number,
): void {
  storage.setItem(ONBOARDING_KEY, String(version));
}

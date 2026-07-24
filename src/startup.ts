import { invoke } from '@tauri-apps/api/core';

export type StartupStage = 'react-mounted' | 'interactive-frame';

let resolveStartupInteractive: (() => void) | undefined;
const startupInteractive = new Promise<void>((resolve) => {
  resolveStartupInteractive = resolve;
});

export function whenStartupInteractive(): Promise<void> {
  return startupInteractive;
}

type StartupHandoffOptions = {
  document?: Document;
  requestFrame?: (callback: FrameRequestCallback) => number;
  cancelFrame?: (handle: number) => void;
  setTimer?: (callback: () => void, delay: number) => ReturnType<typeof setTimeout>;
  clearTimer?: (handle: ReturnType<typeof setTimeout>) => void;
  markStage?: (stage: StartupStage) => Promise<unknown>;
};

function isTauriRuntime(): boolean {
  return (
    typeof window !== 'undefined' && ('__TAURI_INTERNALS__' in window || '__TAURI__' in window)
  );
}

async function markTauriStage(stage: StartupStage): Promise<void> {
  if (!isTauriRuntime()) return;
  await invoke('mark_startup_stage', { stage });
}

/**
 * Starts after React committed the application tree. The interactive milestone is reported only
 * after two paint opportunities and after the HTML loading layer no longer covers the main page.
 */
export function beginStartupHandoff(options: StartupHandoffOptions = {}): () => void {
  const currentDocument = options.document ?? document;
  const requestFrame = options.requestFrame ?? requestAnimationFrame;
  const cancelFrame = options.cancelFrame ?? cancelAnimationFrame;
  const setTimer = options.setTimer ?? setTimeout;
  const clearTimer = options.clearTimer ?? clearTimeout;
  const markStage = options.markStage ?? markTauriStage;
  const bootLoader = currentDocument.getElementById('boot-loader');
  const frameHandles: number[] = [];
  let timer: ReturnType<typeof setTimeout> | undefined;
  let disposed = false;
  let interactiveReported = false;

  const report = (stage: StartupStage) => {
    void markStage(stage).catch(() => undefined);
  };
  const reportInteractive = () => {
    if (disposed || interactiveReported) return;
    interactiveReported = true;
    resolveStartupInteractive?.();
    resolveStartupInteractive = undefined;
    report('interactive-frame');
  };
  const afterPaint = () => {
    frameHandles.push(requestFrame(reportInteractive));
  };
  const removeLoader = () => {
    if (disposed) return;
    if (timer !== undefined) {
      clearTimer(timer);
      timer = undefined;
    }
    bootLoader?.remove();
    afterPaint();
  };

  report('react-mounted');
  frameHandles.push(
    requestFrame(() => {
      frameHandles.push(
        requestFrame(() => {
          if (!bootLoader) {
            afterPaint();
            return;
          }
          bootLoader.classList.add('done');
          const handleTransitionEnd = (event: Event) => {
            if (
              event.target === bootLoader &&
              event instanceof TransitionEvent &&
              event.propertyName === 'opacity'
            ) {
              bootLoader.removeEventListener('transitionend', handleTransitionEnd);
              removeLoader();
            }
          };
          bootLoader.addEventListener('transitionend', handleTransitionEnd);
          timer = setTimer(() => {
            bootLoader.removeEventListener('transitionend', handleTransitionEnd);
            removeLoader();
          }, 250);
        }),
      );
    }),
  );

  return () => {
    disposed = true;
    frameHandles.forEach(cancelFrame);
    if (timer !== undefined) clearTimer(timer);
  };
}

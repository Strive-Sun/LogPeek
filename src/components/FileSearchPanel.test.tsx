import assert from 'node:assert/strict';
import test, { afterEach, before } from 'node:test';
import { JSDOM } from 'jsdom';
import type { FileSearchPage, FileSearchResult, FileSearchStatus } from '../api/types';

const dom = new JSDOM('<!doctype html><html><body></body></html>', {
  url: 'http://localhost/',
  pretendToBeVisual: true,
});

class TestResizeObserver {
  constructor(private readonly callback: ResizeObserverCallback) {}

  observe(target: Element) {
    this.callback(
      [
        {
          target,
          contentRect: target.getBoundingClientRect(),
          borderBoxSize: [],
          contentBoxSize: [],
          devicePixelContentBoxSize: [],
        } as unknown as ResizeObserverEntry,
      ],
      this as unknown as ResizeObserver,
    );
  }

  disconnect() {}
  unobserve() {}
}

before(() => {
  const globals = {
    window: dom.window,
    document: dom.window.document,
    navigator: dom.window.navigator,
    HTMLElement: dom.window.HTMLElement,
    Element: dom.window.Element,
    Node: dom.window.Node,
    MutationObserver: dom.window.MutationObserver,
    getComputedStyle: dom.window.getComputedStyle,
    localStorage: dom.window.localStorage,
    ResizeObserver: TestResizeObserver,
    requestAnimationFrame: dom.window.requestAnimationFrame.bind(dom.window),
    cancelAnimationFrame: dom.window.cancelAnimationFrame.bind(dom.window),
    IS_REACT_ACT_ENVIRONMENT: true,
  };
  for (const [key, value] of Object.entries(globals)) {
    Object.defineProperty(globalThis, key, { configurable: true, writable: true, value });
  }
  Object.defineProperty(dom.window, 'ResizeObserver', {
    configurable: true,
    value: TestResizeObserver,
  });
  Object.defineProperty(dom.window.HTMLElement.prototype, 'scrollTo', {
    configurable: true,
    value() {},
  });
  Object.defineProperty(dom.window.HTMLElement.prototype, 'getBoundingClientRect', {
    configurable: true,
    value() {
      return {
        x: 0,
        y: 0,
        top: 0,
        left: 0,
        right: 900,
        bottom: 600,
        width: 900,
        height: 600,
        toJSON() {},
      };
    },
  });
});

const status: FileSearchStatus = {
  phase: 'ready',
  scannedFiles: 2,
  skippedDirectories: 0,
  indexedFiles: 2,
  indexBytes: 128,
  roots: ['C:\\'],
  exclusions: [],
  providers: [{ root: 'C:\\', provider: 'windowsNtfs', phase: 'ready' }],
};

const result: FileSearchResult = {
  path: 'C:\\Logs\\debug.log',
  name: 'debug.log',
  parent: 'C:\\Logs',
  kind: 'log',
  size: 42,
  modifiedMs: 1_700_000_000_000,
  isLog: true,
  isArchive: false,
};

const page: FileSearchPage = {
  items: [result],
  total: 1,
  partial: false,
  elapsedMs: 2,
};

afterEach(async () => {
  const { cleanup } = await import('@testing-library/react');
  cleanup();
});

async function renderPanel(overrides?: {
  inspect?: () => Promise<{
    path: string;
    name: string;
    watchPath: string;
    kind: 'file';
    isLog: boolean;
    alreadyMonitored: boolean;
  }>;
}) {
  const { fireEvent, render, screen, waitFor } = await import('@testing-library/react');
  const React = await import('react');
  const { api } = await import('../api');
  const { FileSearchPanel } = await import('./FileSearchPanel');
  const i18n = await import('../i18n/I18nProvider');
  let searchCalls = 0;
  let monitorCalls = 0;
  const original = {
    fileSearchStatus: api.fileSearchStatus,
    fileSearchConfig: api.fileSearchConfig,
    subscribeFileSearchStatus: api.subscribeFileSearchStatus,
    searchFiles: api.searchFiles,
    inspectSearchResult: api.inspectSearchResult,
    addSearchResultParent: api.addSearchResultParent,
  };
  api.fileSearchStatus = async () => status;
  api.fileSearchConfig = async () => ({
    version: 1,
    enabled: true,
    roots: status.roots,
    exclusions: [],
  });
  api.subscribeFileSearchStatus = () => () => {};
  api.searchFiles = async () => {
    searchCalls += 1;
    return page;
  };
  api.inspectSearchResult =
    overrides?.inspect ??
    (async () => ({
      path: result.path,
      name: result.name,
      watchPath: result.parent,
      kind: 'file' as const,
      isLog: true,
      alreadyMonitored: false,
    }));
  api.addSearchResultParent = async () => {
    monitorCalls += 1;
    return result.parent;
  };

  let closed = 0;
  const opened: string[] = [];
  let monitorAdded = 0;
  render(
    React.createElement(
      i18n.I18nProvider,
      null,
      React.createElement(FileSearchPanel, {
        onClose: () => {
          closed += 1;
        },
        onOpenEntry: (path: string) => opened.push(path),
        onMonitorAdded: async () => {
          monitorAdded += 1;
        },
        virtualizeResults: false,
      }),
    ),
  );
  const input = await screen.findByRole('textbox');
  fireEvent.change(input, { target: { value: 'debug.log' } });
  await waitFor(() => assert.ok(searchCalls > 0));
  const rowText = await screen.findByText('debug.log');
  const row = rowText.closest('[role="option"]');
  assert.ok(row);

  return {
    api,
    fireEvent,
    original,
    row,
    screen,
    waitFor,
    calls: {
      get closed() {
        return closed;
      },
      get monitor() {
        return monitorCalls;
      },
      get monitorAdded() {
        return monitorAdded;
      },
      get opened() {
        return opened;
      },
      get search() {
        return searchCalls;
      },
    },
  };
}

test('搜索面板查询后双击日志并复用 LogCrate 打开链路', async () => {
  const harness = await renderPanel();
  harness.fireEvent.doubleClick(harness.row);
  await harness.waitFor(() => assert.deepEqual(harness.calls.opened, [result.path]));
  assert.equal(harness.calls.closed, 1);
  Object.assign(harness.api, harness.original);
});

test('搜索结果右键可将所在目录加入监控', async () => {
  const harness = await renderPanel();
  harness.fireEvent.contextMenu(harness.row, { clientX: 10, clientY: 10 });
  harness.fireEvent.click(await harness.screen.findByText('Add containing folder to monitoring'));
  await harness.waitFor(() => assert.equal(harness.calls.monitorAdded, 1));
  assert.equal(harness.calls.monitor, 1);
  Object.assign(harness.api, harness.original);
});

test('双击已失效结果显示错误并重新查询', async () => {
  const harness = await renderPanel({
    inspect: async () => {
      throw new Error('文件已被删除或移动');
    },
  });
  const before = harness.calls.search;
  harness.fireEvent.doubleClick(harness.row);
  await harness.screen.findByText(/文件已被删除或移动/);
  await harness.waitFor(() => assert.ok(harness.calls.search > before));
  assert.deepEqual(harness.calls.opened, []);
  Object.assign(harness.api, harness.original);
});

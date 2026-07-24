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
      [{ target, contentRect: target.getBoundingClientRect() } as unknown as ResizeObserverEntry],
      this as unknown as ResizeObserver,
    );
  }

  disconnect() {}
  unobserve() {}
}

before(() => {
  for (const [key, value] of Object.entries({
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
  })) {
    Object.defineProperty(globalThis, key, { configurable: true, writable: true, value });
  }
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

afterEach(async () => {
  const { cleanup } = await import('@testing-library/react');
  cleanup();
});

const status: FileSearchStatus = {
  phase: 'ready',
  scannedFiles: 1,
  skippedDirectories: 0,
  indexedFiles: 1,
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

test('搜索页面隐藏后保留结果且再次显示不重复初始化或查询', async () => {
  const { fireEvent, render, screen, waitFor } = await import('@testing-library/react');
  const { api } = await import('../api');
  const { FileSearchPanel } = await import('./FileSearchPanel');
  const { I18nProvider } = await import('../i18n/I18nProvider');
  let statusCalls = 0;
  let configCalls = 0;
  let searchCalls = 0;
  const original = {
    fileSearchStatus: api.fileSearchStatus,
    fileSearchConfig: api.fileSearchConfig,
    subscribeFileSearchStatus: api.subscribeFileSearchStatus,
    searchFiles: api.searchFiles,
  };
  api.fileSearchStatus = async () => {
    statusCalls += 1;
    return status;
  };
  api.fileSearchConfig = async () => {
    configCalls += 1;
    return { version: 1, enabled: true, roots: status.roots, exclusions: [] };
  };
  api.subscribeFileSearchStatus = () => () => {};
  api.searchFiles = async () => {
    searchCalls += 1;
    return page;
  };

  const panel = (active: boolean) => (
    <I18nProvider>
      <div hidden={!active} data-testid="search-keep-alive">
        <FileSearchPanel
          active={active}
          onClose={() => undefined}
          onOpenEntry={() => undefined}
          onMonitorAdded={async () => undefined}
          virtualizeResults={false}
        />
      </div>
    </I18nProvider>
  );

  try {
    const view = render(panel(true));
    const input = await screen.findByRole('textbox');
    fireEvent.change(input, { target: { value: 'debug.log' } });
    await screen.findByText('debug.log');
    await waitFor(() => assert.equal(searchCalls, 1));
    assert.equal(statusCalls, 1);
    assert.equal(configCalls, 1);
    const results = screen.getByRole('listbox');
    results.scrollTop = 123;

    view.rerender(panel(false));
    assert.equal(screen.getByTestId('search-keep-alive').hidden, true);
    view.rerender(panel(true));

    const restoredInput = await screen.findByRole('textbox');
    assert.equal(restoredInput, input);
    assert.equal((restoredInput as HTMLInputElement).value, 'debug.log');
    assert.ok(screen.getByText('debug.log'));
    assert.equal(screen.getByRole('listbox'), results);
    assert.equal(results.scrollTop, 123);
    assert.equal(statusCalls, 1);
    assert.equal(configCalls, 1);
    assert.equal(searchCalls, 1);
  } finally {
    Object.assign(api, original);
  }
});

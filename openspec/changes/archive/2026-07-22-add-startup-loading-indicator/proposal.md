# Change: 启动进度条提供加载反馈

## Why

应用启动时 WebView2 冷启动约需数秒。WebView 解析 `index.html` 之前，现有主窗口只能显示空白客户区，因此仅靠 HTML/CSS 加载页仍会先出现约两秒白屏。

启动反馈必须早于 WebView：主窗口先由原生窗口层创建并直接绘制品牌与进度条，WebView 在同一窗口内以不可见的小尺寸后台初始化。页面加载完成后，WebView 扩展覆盖原生启动画面。这样窗口从首次出现起就有可见反馈，并且交接过程中不需要隐藏或替换主窗口。

同时，缓存维护、应用状态初始化、目录 watcher 恢复等非 UI 工作不应占用 Tauri UI 线程。它们在后台完成，前端命令异步等待状态就绪。

## What Changes

- 不再由配置自动创建 WebviewWindow；在 Tauri `setup` 中立即创建同尺寸的原生主窗口。
- 使用 Tauri 已依赖的 `softbuffer` 在原生主窗口客户区绘制并持续更新品牌与进度条，不依赖 WebView/JS。
- 在后台请求为同一主窗口创建 1x1 WebView；页面加载完成后将其扩展到整个客户区并停止原生绘制。
- 保留 HTML/CSS 加载页作为 WebView 页面内部的二阶段反馈及开发环境兜底。
- 将应用状态初始化、陈旧缓存清理和 watcher 恢复移到后台；依赖状态的 Tauri 命令改为异步等待初始化完成。

## Impact

- Affected specs: application-lifecycle
- Affected code:
  - `src-tauri/tauri.conf.json` — 禁止自动创建 WebviewWindow
  - `src-tauri/src/startup.rs` — 原生启动画面绘制与进度动画
  - `src-tauri/src/lib.rs` — 原生窗口/WebView 交接与后台初始化
  - `src-tauri/Cargo.toml` — 启用 Tauri 原生窗口 API并直接声明已有 softbuffer 依赖
  - `index.html`, `src/main.tsx` — WebView 内部加载页兜底

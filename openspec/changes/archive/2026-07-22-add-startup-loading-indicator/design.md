## Context

WebView2 初始化完成前无法解析或绘制 `index.html`。现有配置会先显示 WebviewWindow，导致标题栏已经出现但客户区保持白色。HTML 加载页只能覆盖 bundle 加载阶段，覆盖不了 WebView 运行时冷启动阶段。

## Goals / Non-Goals

- Goals: 主窗口首次可见时已有动态启动反馈；使用同一主窗口完成原生画面到 WebView 的交接；非 UI 初始化不阻塞 UI 线程；保持 Windows/macOS 支持。
- Non-Goals: 展示真实任务百分比；引入完整原生 GUI 框架；改变应用主界面的 React 实现。

## Decisions

- Decision: 创建无 WebView 的 Tauri 原生主窗口，并用 `softbuffer` 直接绘制客户区。`softbuffer` 已是 Tauri/tao 依赖图的一部分，直接声明不会引入完整 GUI 框架。
- Decision: WebView 以 1x1 尺寸附加到同一窗口，在 `PageLoadEvent::Finished` 后扩展到完整客户区并启用自动缩放。
- Decision: 原生进度为非确定性动画，最多推进到 90%；WebView 接管代表启动完成。
- Decision: 应用业务状态通过一次性异步就绪门发布。Tauri 命令异步等待该门，避免 UI 线程同步做文件系统和 watcher 初始化。

Alternatives considered:

- HTML/CSS 启动页：无法覆盖 WebView 冷启动，已由实机测试否决。
- 隐藏主窗口直到页面加载：消除白屏但启动期间没有可见反馈，不满足需求。
- 独立原生 splash 进程：可以保持动画独立，但窗口替换更复杂；同一原生窗口附加 WebView能提供更连续的交接。
- Slint/egui 等原生 GUI 框架：功能过重，会增加编译时间和安装包体积。

## Risks / Trade-offs

- Tauri 的原生 Window/child WebView API 需要 `unstable` feature → 限定在启动模块内并通过双平台 CI 编译验证。
- 软件缓冲区在 WebView 加载期间由绘制线程更新 → WebView 完成后用原子标记立即停止，避免继续覆盖或消耗 CPU。
- 前端可能早于后台状态初始化发出命令 → 命令异步等待一次性就绪门，不在 UI 线程阻塞。

## Migration Plan

1. 将配置窗口标记为不自动创建。
2. `setup` 立即创建原生主窗口并启动绘制。
3. 后台附加 WebView并初始化业务状态。
4. 页面完成后停止绘制、展开 WebView并恢复窗口缩放。
5. 若 child WebView 创建失败，保留原生启动画面而不是露出白屏。

## Open Questions

- 原生画面到 WebView 的视觉交接需要实机确认是否需要额外淡入。

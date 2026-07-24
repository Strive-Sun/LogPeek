# Change: 将主页面启动时间优化到约三秒

## Why

当前发布版从用户启动 LogCrate 到进入主页面约需二十秒。虽然原生启动画面能够立即显示进度条，但它只提供等待反馈，没有缩短进入可交互界面的时间。当前启动路径也缺少阶段计时，无法区分进程装载、WebView 创建、页面加载、React 首帧和后台服务恢复各自的耗时。

LogCrate 的主页面 UI 不应等待监控 watcher 恢复、搜索数据库、更新检查、缓存维护或其他可延后任务。目标是在常规受支持设备上约三秒内进入可交互主页面，并用可重复的发布版启动基准防止性能回退。

## What Changes

- 定义“主页面可交互”的统一启动里程碑：React 主界面已完成首帧，主导航和监控目录区域能够响应用户输入。
- 为进程入口、原生首帧、WebView 创建与导航、React 挂载、主页面可交互、核心状态发布及后台模块调度增加本地阶段计时。
- 缩短原生启动画面到 WebView 主界面的关键路径，优先创建和加载主 WebView，避免等待与首屏无关的插件、托盘、持久化恢复和后台任务。
- 将监控目录数据恢复、watcher 建立、搜索初始化、更新检查和缓存维护从主页面显示条件中解耦；主页面先显示骨架或已知轻量状态，数据就绪后增量更新。
- 降低启动动画和前端初始 bundle 对 WebView 初始化的资源竞争，并按实际阶段数据决定是否进一步拆分主进程中的重型搜索依赖。
- 增加 Windows 与 macOS 发布构建的启动基准和回归记录。

## Impact

- Affected specs: application-lifecycle
- Affected code:
  - `src-tauri/src/main.rs`、`src-tauri/src/lib.rs`、`src-tauri/src/startup.rs`：启动阶段计时、WebView 关键路径和后台任务调度
  - `src-tauri/tauri.conf.json`：窗口与 WebView 创建策略
  - `src/main.tsx`、`src/App.tsx`、`index.html`：可交互里程碑、首屏挂载和加载层交接
  - `vite.config.ts` 及前端模块：必要时拆分非首屏代码
  - 启动基准脚本与测试：发布版端到端计时和生命周期顺序验证

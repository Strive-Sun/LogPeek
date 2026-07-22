## 1. 原生启动画面

- [x] 1.1 配置主窗口不自动创建 WebView，启动时立即创建原生主窗口
- [x] 1.2 使用软件缓冲区绘制 LogCrate 品牌、轨道与持续推进的进度条
- [x] 1.3 在同一窗口后台创建最小尺寸 WebView，加载完成后扩展覆盖原生画面
- [x] 1.4 保留 HTML/CSS 二阶段加载页并在 React 挂载后平滑移除

## 2. UI 线程瘦身

- [x] 2.1 应用状态、缓存目录准备在后台线程初始化，前端命令异步等待就绪
- [x] 2.2 陈旧嵌套归档缓存后台清理，当前进程使用独立缓存目录避免竞态
- [x] 2.3 已配置目录 watcher 在状态发布后后台恢复

## 3. 关联缺陷修复

- [x] 3.1 `resizeTabs` 在布局与容量均未变化时返回原对象
- [x] 3.2 稳定 `LogTabs.onCapacityChange` 回调，避免 ResizeObserver effect 反复重跑
- [x] 3.3 加载层仅处理自身 opacity 的 transitionend，忽略子进度条冒泡事件

## 4. 验证

- [x] 4.1 前端 build、lint、变更文件 format、test 全部通过
- [x] 4.2 Rust fmt、clippy、check、test 全部通过
- [x] 4.3 Windows Release 编译验证通过
- [ ] 4.4 macOS 编译验证通过
- [x] 4.5 Windows 实机确认窗口首次出现即显示动态进度条，随后在同一窗口进入应用界面
- [ ] 4.6 macOS 实机确认原生启动画面与 WebView 交接
- [x] 4.7 `openspec validate add-startup-loading-indicator --strict` 通过

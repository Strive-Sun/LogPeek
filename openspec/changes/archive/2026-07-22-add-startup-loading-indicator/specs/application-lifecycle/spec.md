## ADDED Requirements

### Requirement: 原生启动进度反馈

系统 SHALL 在 WebView 尚未初始化时创建可见的原生主窗口，并在其客户区绘制持续推进的启动进度条。该反馈 SHALL 不依赖 WebView、HTML 或 JavaScript。WebView SHALL 在同一主窗口内后台加载，并在页面就绪后覆盖原生启动画面。

#### Scenario: WebView 冷启动期间立即显示反馈

- **WHEN** 用户启动 LogCrate 且 WebView 运行时尚未完成初始化
- **THEN** 原生主窗口已经可见并显示持续推进的品牌进度画面，而不是空白客户区

#### Scenario: 同一窗口进入应用界面

- **WHEN** WebView 页面完成加载
- **THEN** WebView 扩展到主窗口完整客户区并覆盖原生启动画面，不隐藏或替换主窗口

#### Scenario: 前端运行时不是启动反馈的前置条件

- **WHEN** HTML 和 JavaScript 尚未开始执行
- **THEN** 原生进度条仍可显示并更新

### Requirement: 启动初始化不阻塞 UI 线程

系统 SHALL 将业务状态构建、陈旧缓存清理和目录 watcher 恢复等非 UI 启动工作放到后台执行。依赖业务状态的前端命令 SHALL 异步等待初始化完成，而不是阻塞 UI 线程。

#### Scenario: 后台初始化期间窗口保持响应

- **WHEN** 缓存或目录 watcher 初始化耗时较长
- **THEN** 原生启动进度仍持续更新，主窗口消息处理不被这些任务阻塞

#### Scenario: 初始化完成前收到前端命令

- **WHEN** WebView 在业务状态发布前调用依赖状态的命令
- **THEN** 命令异步等待状态就绪后继续执行，不返回未初始化数据且不阻塞 UI 线程

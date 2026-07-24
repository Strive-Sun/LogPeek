# application-lifecycle Specification

## Purpose
TBD - created by archiving change add-close-to-tray. Update Purpose after archive.
## Requirements
### Requirement: 关闭主窗口时保持后台运行

系统 SHALL 在用户点击主窗口关闭按钮时阻止窗口销毁并将其隐藏到系统托盘，同时 MUST 保持应用进程、目录监控和已启动后台任务继续运行；普通最小化按钮 SHALL 保持平台默认行为。

#### Scenario: 点击窗口关闭按钮

- **WHEN** 用户点击主窗口标题栏关闭按钮
- **THEN** 主窗口从桌面和任务栏隐藏，LogCrate 进程继续运行且系统托盘保留 LogCrate 图标

#### Scenario: 隐藏期间检测新日志

- **WHEN** 主窗口已隐藏到托盘且监控目录出现新日志
- **THEN** 系统继续完成目录同步和新日志检测，恢复窗口后可看到隐藏期间产生的最新状态

#### Scenario: 使用最小化按钮

- **WHEN** 用户点击主窗口最小化按钮
- **THEN** 系统按平台默认方式最小化窗口，不将其解释为关闭或退出

### Requirement: 从系统托盘恢复主窗口

系统 SHALL 提供托盘“显示主窗口”操作，并在平台支持时允许点击托盘图标恢复同一个主窗口实例；恢复操作 SHALL 显示、取消最小化并聚焦窗口，且 MUST NOT 重建或清空现有前端状态。

#### Scenario: 通过托盘菜单恢复

- **WHEN** 主窗口隐藏且用户选择托盘菜单“显示主窗口”
- **THEN** 系统显示并聚焦原主窗口，目录树、查看会话和未读状态保持不变

#### Scenario: 重复显示已可见窗口

- **WHEN** 主窗口已经可见且用户再次触发托盘显示操作
- **THEN** 系统聚焦现有窗口且不创建第二个窗口实例

### Requirement: 通过系统托盘退出应用

系统 SHALL 在托盘菜单提供“退出 LogCrate”，该操作 SHALL 完整结束应用进程；关闭到托盘逻辑 MUST NOT 阻断托盘退出、自动更新重启或操作系统会话结束。

#### Scenario: 托盘菜单退出

- **WHEN** 用户选择托盘菜单“退出 LogCrate”
- **THEN** 系统结束应用进程并停止目录监控、索引任务和托盘图标

#### Scenario: 自动更新后重启

- **WHEN** updater 完成更新并请求应用重启
- **THEN** 系统允许当前进程退出并启动新版本，不把更新退出误处理为隐藏窗口

#### Scenario: 操作系统结束会话

- **WHEN** 操作系统关机、注销或明确终止应用进程
- **THEN** 系统不以关闭到托盘行为阻止会话结束

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

### Requirement: 核心功能优先于可选搜索初始化

系统 SHALL 将 UI、监控目录恢复和日志会话状态视为启动核心路径，并在可选搜索后端的数据库与索引初始化之前使这些核心功能可用。搜索后端初始化 MUST 在后台执行，且 MUST NOT 延迟主窗口响应、监控 watcher 启动或核心应用状态发布。

#### Scenario: 核心启动路径完成

- **WHEN** 应用正在恢复持久化状态
- **THEN** 系统先发布可用的核心应用状态并启动监控目录 watcher，再调度可选搜索后端初始化

#### Scenario: 搜索初始化耗时较长

- **WHEN** 搜索数据库恢复或查询索引打开需要较长时间
- **THEN** 主窗口、监控目录和日志查看功能保持可响应且无需等待搜索初始化

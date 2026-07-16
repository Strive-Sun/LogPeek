# Project Context

## Purpose
LogPeek 是一款免解压的跨平台桌面日志阅读器，面向"下载日志压缩包 → 免解压 → 直接查看"这一高频排查场景。它监控用户指定的目录，自动发现新到达的日志压缩包（zip / tar.gz / 7z / rar），免解压流式读取包内日志，在顶栏提示新日志并支持流畅查看 GB 级大文件。目标平台为 Windows 与 macOS 双端。

详细技术方案见 `docs/technical-design.md`。

## Tech Stack

### 核心框架
- Tauri 2.x - 跨平台桌面应用框架（Rust 后端 + Web 前端）
- Rust - 后端语言（目录监控、免解压读取、行索引）
- TypeScript - 前端语言
- React 18 - 前端 UI 框架
- Radix UI / shadcn 风格基元 - 轻量 UI 组件(控制包体积)
- @tanstack/react-virtual - 日志正文虚拟滚动
- 三栏式布局 + 铃铛角标提示 + 深浅色主题(详见 docs/technical-design.md 第 6 章)

### 关键 Rust crate
- `notify` - 跨平台目录监控（封装 FSEvents / ReadDirectoryChangesW）
- `zip` - zip 免解压随机访问
- `tar` + `flate2` - tar.gz 流式读取
- `sevenz-rust` - 7z 读取（试点）
- `memmap2` - 大文件内存映射
- `tokio` - 异步运行时（后台索引 / 监控）

### 构建与包管理
- Cargo - Rust 构建
- pnpm / npm - 前端包管理
- Vite - 前端构建
- tauri-cli - 打包分发（Windows + macOS）

## Project Conventions

### Code Style
- Rust: 遵循 `rustfmt` 默认规则，`clippy` 零告警
- 前端: Prettier 格式化（单引号、行宽 100、LF 换行）
- 命名: Rust 用 snake_case，前端用 camelCase / PascalCase
- 提交信息使用中文，遵循 Conventional Commits（feat/fix/docs/refactor 等前缀）

### Architecture Patterns

#### 前后端分层
- 前端（WebView）只负责 UI 与交互，不做重 IO / 解压逻辑
- Rust 后端负责目录监控、归档读取、行索引与窗口化加载
- 通信: 前端通过 `invoke` 调用命令，后端通过 `emit` 推送事件

#### 归档读取抽象
- 定义统一的 `ArchiveReader` trait 屏蔽格式差异（zip / tar.gz / 7z / rar 各自实现）
- 免解压含义: 内容不落地到磁盘，以流的形式交给行索引层按需消费
- 顺序流格式（如 tar.gz）大文件跳转采用透明内部临时缓存，用完即清理

#### 大文件处理
- 行偏移索引 + 窗口化加载，前端虚拟滚动只请求可视区行范围
- 后台建索引，通过进度事件反馈

### Testing Strategy
- Rust 单元测试: `cargo test`，覆盖归档读取、行索引核心逻辑
- 前端组件测试: Vitest
- 端到端: 后续引入（Tauri 测试工具链）

### Git Workflow
- 主分支: `main`
- 功能分支: `feat/`、`fix/`、`refactor/` 前缀
- OpenSpec 驱动: 新能力先创建 change 提案并通过审批，再实施
- 提交前运行 `cargo fmt` + `cargo clippy` + 前端 lint

## Domain Context

### 日志排查领域
- 日志压缩包（archive）: 用户下载的、包含一个或多个日志文件的压缩文件
- 归档条目（entry）: 压缩包内的单个文件，可能是 .log / .txt 或其他
- 免解压（in-place read）: 不将压缩包展开到磁盘，直接流式读取内容
- 行索引（line index）: 记录每行字节偏移的数组，用于大文件随机跳转
- 窗口化加载（windowed loading）: 只加载当前可视区域的行

### 与竞品的关系
- LogViewPlus: 商业闭源、仅 Windows，功能接近但缺双端与"压缩包丢入监控目录自动识别"衔接
- lnav: 开源、命令行、免解压，但无 GUI
- klogg: 开源、跨平台、大文件强，但不监控目录、不解压
- LogPeek 差异点: 把"监控目录 → 压缩包自动识别为日志 → 顶栏提示 → 免解压查看"链路在跨平台 GUI 里打通

## Important Constraints

### 技术约束
- 必须支持 Windows 与 macOS 双端
- 安装包体积目标 ≤ 15MB（Windows 采用默认在线引导 WebView2，保持 ≤ 10MB）；详见 `docs/technical-design.md` 3.4/3.5
- GB 级日志不得整包进内存，必须行索引 + 窗口化加载
- 免解压: zip 等可随机访问格式做到真正零落地；顺序流格式允许透明内部临时缓存
- 新压缩包到达时先做稳定性检测（大小稳定 / 关闭事件），避免下载中文件误判

### 非目标（v1 明确不做）
- 手机端（iOS / Android）
- 日志服务器端聚合 / 上传 / 检索
- 日志格式语义解析（结构化字段）
- 实时远程 tail（SSH / 网络流）

## External Dependencies
- 无云服务依赖（纯本地工具）
- rar 解压依赖 `unrar`（专利闭源算法），授权受限，v1 可延后

# Change: 新增全局本地文件搜索

## Why

用户常知道日志或文件的部分名称，却不知道它位于哪个目录，而现有目录树只能浏览已加入监控的路径。顶栏“搜索”目前仅是不可交互的占位符，无法帮助用户找到监控范围之外的本地文件。

## What Changes

- 将顶栏搜索入口改为可交互的全局文件搜索面板，可按文件名和路径搜索尚未加入监控的本地文件。
- 在 Windows/NTFS 上增加 LogCrate 本地索引服务，通过 MFT 批量枚举建立初始索引，并通过 USN Journal 恢复和维护增量变化；主程序继续以普通用户权限运行。
- 在 macOS、非 NTFS 卷或 Windows 索引服务不可用时保留兼容目录扫描 provider，并明确展示 provider、性能差异与降级原因。
- 在 Rust 后台建立持久化文件元数据索引，首次索引与增量更新均不阻塞 UI，已索引数据提供类 Everything 的即时查询体验。
- 将 SQLite 保留为卷状态与文件关系存储，将搜索热路径替换为从 GPLv3 项目 Orange 移植并适配 Tantivy 0.22 的索引架构，包括文件名切词、简繁/拼音扩展、精确路径键、类型/扩展名字段、TopDocs 查询和定时提交。
- 将多词查询统一为 AND 语义，并按完整文件名、文件名前缀、文件名包含关系和稳定路径依次排序，避免通用扩展名结果占满首页；已在 C/D 双卷真实索引中验证同一查询可同时返回多个卷的结果。
- 分开呈现 MFT 已发现记录数与 Tantivy 已可搜索文件数，使建立索引期间的进度与当前可查询范围保持一致。
- 将 Tantivy 查询快照完成标记与 SQLite 后台持久化状态分离，避免开发热重载或持久化中断错误清空已完成索引；同时为开发构建启用搜索热路径定向优化。
- 默认搜索可读的本地固定卷，允许用户排除卷或目录；网络与可移动卷不默认建索引。
- 搜索结果支持双击打开 LogCrate 可识别的裸日志或归档，复用现有查看会话、归档读取和索引进度链路。
- 搜索结果右键菜单提供“将所在目录加入监控”，复用现有监控根去重、归并与持久化行为。
- 明确索引服务安装与卸载、IPC、最小权限、MFT/USN 失效恢复、符号链接/联接点、大规模索引和文件名隐私边界。

## Impact

- Affected specs: `file-search` (new)
- Affected code: `src/components/TopBar.tsx`, new search UI components, `src/App.tsx`, `src/api/*`, `src-tauri/src/lib.rs`, new Rust search index/provider modules, Windows service entrypoint and IPC, installer configuration, configuration and localization
- New persistence: local file metadata index and search-scope configuration
- Security impact: Windows 安装器在用户批准后安装按需启动的只读索引服务；服务可读取本机 NTFS 文件名但不得读取文件内容
- Platform impact: Windows NTFS 使用 MFT/USN 快速 provider；Windows 非 NTFS 与 macOS 使用兼容扫描 provider
- License impact: 搜索模块包含 Orange GPLv3 衍生代码；分发包含该模块的 LogCrate 时必须遵守 GPLv3，并提供许可证及对应源代码
- Verified baseline: 当前 Windows C/D 双卷索引包含 5,039,359 个可搜索文件；约 251.7 万文件的 D 盘首次索引在定向优化后的开发构建中约为 49.16 秒，release 构建约为 34.79 秒

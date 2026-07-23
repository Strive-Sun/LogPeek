## Context

LogCrate 当前仅保存已监控目录的惰性目录树库存，启动时特意不递归扫描未展开子树。全局文件发现必须覆盖监控根之外的路径，因此不能依赖现有 React 目录树，也不能在每次查询时现场全盘遍历。

Everything 在 Windows/NTFS 上通过 MFT 与 USN Journal 获得极低建索引成本，macOS 没有相同接口。实机验证表明，即使把 SQLite 首次建库从 224.95 秒优化到 28.94 秒，逐目录遍历仍使 LogCrate 在 913,408 个文件、运行超过两分钟后未完成，而同机 Everything 后启动仍在一分钟内完成并返回结果。因此目录遍历只能作为兼容 provider，不能继续作为 Windows NTFS 的主路径。

LogCrate 必须保持独立实现，不依赖用户安装 Everything；Windows 主程序不得为搜索而始终以管理员权限运行。为获得卷级元数据读取权限，安装器可在用户批准系统提权时安装独立的本地索引服务。

## Goals / Non-Goals

- Goals:
  - 在可读本地固定卷中按文件名和路径查找文件，包括监控根之外的文件。
  - 首次建索引后提供亚秒级首页结果，文件变化后增量更新。
  - Windows 固定 NTFS 卷使用 MFT 建立初始文件名/路径索引，并用 USN Journal 在重启后只追赶变化。
  - 在指定的百万文件 Windows/NTFS 参考数据集上，60 秒内完成可搜索索引，并在 10 秒内提供首批已发现结果。
  - 日志可直接打开预览，任意文件的父目录可加入监控。
  - 扫描、写索引和查询均在 Rust 后台执行，UI 线程只管理输入和有界结果列表。
- Non-Goals:
  - 搜索文件内容、日志正文或未打开归档内的虚拟条目。
  - 完整复制 Everything 的全部查询语法、文件内容索引、跨机器搜索或复用 Everything 的闭源实现。
  - 为非 NTFS 文件系统虚构 MFT/USN 能力；这些范围允许明确降级到目录扫描。
  - 默认索引网络共享、可移动卷、文件内容或上传任何路径信息。

## Decisions

- Decision: 使用应用自管的持久化元数据索引，而不将 Everything 或 Spotlight 设为必需运行时依赖。发现索引最少保存规范化路径、显示名、MFT/文件类型属性和所属搜索范围；大小、修改时间、可读性与内容采样允许仅对可见结果按需补齐。
- Decision: Windows 固定 NTFS 卷默认使用 `WindowsNtfsProvider`。按需启动的 `LogCrate Index Service` 以提升后的服务身份打开卷句柄，通过 `FSCTL_ENUM_USN_DATA` 流式枚举 MFT 记录；客户端按 file reference number 与 parent reference number 重建路径。Windows NTFS 主路径 MUST NOT 递归打开目录或逐文件 `stat` 才能发现名称。
- Decision: Windows 主程序始终以普通用户运行。服务仅提供卷标识、MFT 名称/父引用记录和 USN 变化，不读取文件内容、不执行打开/复制/删除/重命名等任意路径操作，也不访问网络。搜索结果的存在性、可读性、类型、大小和时间由普通用户进程对可见结果按需校验。
- Decision: 服务使用本机版本化 named-pipe IPC，限制为本机已认证交互用户，设置消息大小、批次和并发上限并校验所有长度与枚举值。协议握手不兼容、服务缺失或权限被撤销时，客户端显示可操作错误，不静默信任未知服务。
- Decision: 服务启动类型为按需启动。应用关闭期间由 NTFS 自身维护 USN Journal；每个卷持久化 volume identity、journal ID 和 next USN。下次启动只重放断点后的变化；journal 被重建、断点早于 `FirstUsn` 或卷身份变化时，仅重建受影响卷的 MFT 索引。
- Decision: Windows 服务安装必须经过安装器提权与用户确认；首次启用搜索时再次说明服务能够枚举本机 NTFS 文件名。服务不持久化文件名，用户级索引仍保存在当前用户的 LogCrate 数据目录。卸载必须停止并移除服务。
- Decision: SQLite 保存卷状态、USN 断点、文件引用关系和可恢复的规范路径元数据；热查询层从 Orange commit `09cfcdeba08ce718a978c6dadbf9b5d8f41b658b` 的 GPLv3 `IdxStore` 移植，适配 Tantivy 0.22 和 LogCrate API。保留 Orange 的文件名驼峰切词、Jieba、简繁转换、拼音/首字母、精确 path、类型/扩展名字段、全量 add-only、增量先删后增和 5/2 秒提交节奏。
- Decision: 查询使用 Orange 的 QueryParser、字段 boost、BooleanQuery 和有界 TopDocs，并将多个切词设为默认 AND，避免 `debug.log` 退化成命中任意 `log` 的宽泛 OR 查询。候选结果按完整文件名、前缀、包含关系和稳定路径顺序重排，返回可见结果后再补齐文件 metadata；显式路径片段仍可由 SQLite 回退。查询不执行全库 `COUNT(*)`。
- Decision: `FolderScanProvider` 的首次扫描和完整重建采用两阶段写入：扫描阶段关闭逐行 FTS 触发器，以较大的事务批次只写元数据表，并用普通子串查询提供部分结果；扫描结束后一次性重建 trigram FTS，再恢复增量触发器。重建标记持久化，异常退出后自动丢弃不完整数据并安全重建。
- Decision: 默认范围为操作系统报告的本地固定卷，用户可在搜索设置中排除卷或目录。Windows NTFS provider 根据 MFT 属性跳过 reparse point 的跨范围展开；兼容扫描 provider 不跟随符号链接、Windows junction 或 macOS alias，以避免循环和跨卷重复。
- Decision: `FolderScanProvider` 保留用于 Windows FAT/exFAT/服务不可用场景和 macOS。UI 必须展示当前 provider 和预计性能；Windows NTFS 快速 provider 不可用时，用户可选择修复/安装服务、继续兼容扫描或排除该卷，系统不得把慢扫描描述为 Everything 级性能。
- Decision: 首次扫描过程中允许查询已索引部分，UI 分别显示 MFT 已发现记录数与 Tantivy 已可搜索文件数，不得把枚举记录数描述为已完成文件索引数。新查询使用 generation/cancellation 废弃旧结果，避免快速输入时乱序覆盖。
- Decision: Tantivy 全卷搜索快照完成后独立持久化 `query_snapshot_complete` 标记。若进程在后续 SQLite/USN 快照写入期间退出，下一次启动 SHALL 立即复用完整 Tantivy 索引并仅在后台修复持久化状态，不得清空搜索索引重新分词；迁移时可用“旧版 bulk 未完成、SQLite 已有持久化文件、Tantivy 非空”识别旧版已完成查询阶段的中断状态。
- Decision: `FolderScanProvider` 初始扫描完成后由原生文件系统事件维护增量一致性；`WindowsNtfsProvider` 使用 USN Journal。兼容 watcher 事件溢出或索引版本变更时安排低优先级差异校验，不在 UI 调用链路执行全盘重扫。
- Decision: 双击结果时先由后端重新校验路径与文件身份。可识别裸文本日志直接打开查看会话；受支持归档进入现有归档浏览链路；其它文件不尝试解析。打开搜索结果不隐式加入监控。
- Decision: 右键“将所在目录加入监控”调用现有 `add_watch_dir`，成功后刷新目录树并定位原结果；被已有监控根覆盖时只定位，不创建重复根。

Alternatives considered:

- 强制安装并通过 IPC 调用 Everything：Windows 体验最接近原产品，但引入外部程序/服务依赖，且 macOS 无对等后端。
- Windows GUI 进程直接读取 MFT/USN：省去服务 IPC，但要求应用始终以管理员身份运行，扩大归档解析和 WebView 的权限边界，因此拒绝。
- Windows 本地索引服务读取 MFT/USN：实现与安装复杂度更高，但能在主程序保持普通权限的同时达到目标性能，因此采用。
- 每次输入都递归遍历磁盘：实现简单，但无法提供即时反馈，会重复产生大量 IO。
- 只搜索已监控目录：可复用现有库存，但无法解决“找不到文件，找到后再加入监控”的核心场景。

## Risks / Trade-offs

- Orange 衍生代码受 GPLv3 约束 → 源文件标注来源与 commit；发布 LogCrate 时随附 GPLv3 许可证并提供完整对应源代码，不能仅以第三方声明替代。
- Windows 服务能看到当前用户原本无权浏览目录中的文件名 → 首次启用前明确披露，只传输名称/父引用等元数据，普通用户进程仍按原权限打开文件，并支持排除和清除用户索引。
- 服务或 named pipe 增加本地攻击面 → 最小命令面、版本握手、严格 ACL、长度检查、有界批次、fuzz/畸形消息测试，服务不接受任意路径读写命令。
- USN Journal 可能截断、删除或重建 → 校验 volume identity、journal ID 与 `FirstUsn`，只对失效卷重新枚举 MFT，不错误宣称索引最新。
- MFT 不提供搜索结果展示所需的全部大小和时间信息 → 首次发现只索引名称/父关系/属性，可见结果由普通用户进程有界并发按需补齐，补齐失败不阻塞其它结果。
- 非 NTFS 或服务不可用时目录扫描可能持续数分钟并产生可观 IO → 明确标注兼容模式，使用低优先级有界 worker、批量事务、可暂停/排除范围，并在 UI 中展示进度。
- 数百万路径会占用额外磁盘 → 仅保存搜索所需元数据，分页返回结果，提供重建/清理索引操作与大小统计。
- 受保护目录不可读 → 按目录跳过并累计诊断，不要求提权，不因局部失败中断整个索引。
- 兼容 provider 的全卷 watcher 在事件风暴或溢出时可能落后 → 有界合并、记录脏范围并在后台校验，查询结果操作前始终重新 stat。
- 用户可能不希望应用保存敏感路径 → 索引只存于本地应用数据目录，不上传，支持排除和一键清除。

## Migration Plan

1. 将搜索 provider/schema 升级到新版本；旧目录扫描索引可保留到快速 provider 首次快照成功，失败时仍可回退，不删除用户文件或监控配置。
2. Windows 安装器在用户批准提权后安装版本匹配、按需启动的 LogCrate Index Service；便携版或拒绝服务安装时使用兼容 provider。
3. 用户首次打开搜索时显示索引范围、provider、文件名隐私和服务权限说明，再开始索引。
4. 快速 provider 为每个 NTFS 卷建立 MFT 快照并记录 volume identity、journal ID、next USN；完成后原子切换并清理旧索引。
5. 升级时先完成服务协议握手再迁移索引；不兼容时保留旧索引并提示修复。卸载时停止并删除服务，用户索引按现有卸载数据策略处理。
6. macOS 继续使用兼容扫描 provider，后续可在不改变前端查询协议的情况下增加 Spotlight provider。

## Open Questions

- Windows 服务二进制复用主可执行文件的 service mode，还是构建最小独立服务；以攻击面、签名、安装包体积和升级原子性决定。

## Performance Baseline

- 2026-07-22 在 Windows release 构建中使用 1,000,000 条合成路径验证 SQLite FTS5 trigram：批量建库 224.95 秒，首屏查询 32.12 毫秒，数据库与 WAL 合计 486,456,512 字节，命中 1,111 条。
- 结论：首屏查询延迟满足即时搜索目标，选择 bundled SQLite FTS5 trigram。首次建库吞吐和约 464 MiB/百万路径的本地存储成本属于已知权衡；扫描保持后台、分批、可暂停，后续可在不改变查询协议的情况下增加平台专用索引 provider。
- 该基线暴露出逐行维护 FTS 是首次建库的主要写放大来源，因此后续基线改用“两阶段批量元数据 + 一次性 FTS rebuild”与上述结果对比；只有确认建库时间显著下降且查询结果一致后才替换原路径。
- 2026-07-22 两阶段实现使用相同 1,000,000 条合成路径复测：完整建库 28.94 秒，首页查询 22.75 毫秒，数据库与 WAL 合计 453,398,528 字节。相对原路径建库快约 7.8 倍、存储减少约 6.8%，查询结果数量一致，因此采用两阶段实现。
- 搜索 schema 版本提升到 2；旧版可能处于未完成扫描的索引会在升级后自动清理，并通过新的两阶段路径重建，避免把旧的部分索引误判为可用索引。
- 2026-07-23 对运行中的 5,039,359 文档 C/D 双卷 Orange 索引执行 `debug AND log` 只读验证：完整候选中 C 盘命中 226 条、D 盘命中 150 条、精确 `debug.log` 共 65 条；经生产搜索入口排序分页后，实际首页 200 条包含 C 盘 75 条和 D 盘 125 条。原 UI 只显示 D 盘是默认 OR 语义令通用 `log` 词项占满有界结果页，并非 C 盘未索引。
- 2026-07-23 在 D 盘约 2,517,335 个文件上复测：默认无优化 test 构建的可搜索时间为 103.56 秒；为 LogCrate 与 Orange/Tantivy 相关包启用定向 dev 优化后为 49.16 秒，查询 4 毫秒；release 基线为 34.79 秒。`tauri:dev` 首次会为这些包进行一次优化编译，后续增量开发仍保留 dev profile。
- 2026-07-23 使用当前 5,039,359 文档真实索引连续两次启动并在 SQLite 修复中途终止：两次启动后 Tantivy 文档数均保持 5,039,359，主窗口可响应，验证热重载不会再次清空完整搜索索引。

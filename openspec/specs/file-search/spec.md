# file-search Specification

## Purpose
TBD - created by archiving change add-global-file-search. Update Purpose after archive.
## Requirements
### Requirement: 全局本地文件发现

系统 SHALL 允许用户按文件名和路径搜索本地固定卷中的文件，搜索范围 SHALL 包含尚未加入 LogCrate 监控的目录。系统 SHALL 允许用户排除卷或目录，且 MUST NOT 默认索引网络共享或可移动卷。系统 SHALL 为每个卷展示正在使用的索引 provider 及其健康状态。

#### Scenario: 找到未监控目录中的文件

- **WHEN** 用户输入一个文件名或路径片段，且匹配文件位于尚未加入监控的已索引目录
- **THEN** 系统在搜索结果中返回该文件及其完整路径

#### Scenario: 排除搜索范围

- **WHEN** 用户将某个卷或目录加入搜索排除列表
- **THEN** 后续建索引和查询不包含该范围内的文件

#### Scenario: 兼容扫描遇到不可读系统目录

- **WHEN** 兼容扫描 provider 遇到当前用户无权读取的目录
- **THEN** 系统跳过该目录、继续处理其它范围并显示有界的跳过诊断，不要求应用以管理员权限重启

#### Scenario: 展示索引 provider

- **WHEN** 用户查看搜索状态或索引设置
- **THEN** 系统按卷显示 Windows NTFS 快速索引、兼容目录扫描或其它实际 provider，且服务降级时显示原因与修复入口

### Requirement: 持久化后台元数据索引

系统 SHALL 在 Rust 后台建立并持久化只包含搜索所需文件元数据的本地索引。MFT 枚举、兼容扫描、批量写入、增量更新与查询 MUST NOT 阻塞 UI 线程，MUST NOT 将全部路径或查询结果一次性加载到前端。索引 provider SHALL 允许名称和路径先变为可搜索，再对可见结果有界并发地补齐大小、修改时间和可读性。

#### Scenario: 首次建立索引

- **WHEN** 用户首次启用全局文件搜索
- **THEN** 系统为各卷选择最快的可用 provider，在后台建立索引、持续发布 provider 与进度且主窗口保持响应

#### Scenario: 建索引期间查询

- **WHEN** 首次扫描尚未完成且用户提交查询
- **THEN** 系统返回已索引范围内的匹配项，并明确标记结果仍在补全

#### Scenario: 区分发现进度与可搜索进度

- **WHEN** Windows NTFS provider 已枚举 MFT 记录但仍在写入查询索引
- **THEN** 系统分别展示已发现的 MFT 记录数和已可搜索文件数，不将目录记录或尚未提交的记录误报为已索引文件

#### Scenario: 搜索快照完成后后台持久化被中断

- **WHEN** 所有卷已写入可查询搜索索引，但进程在 SQLite 与 USN 快照持久化完成前退出或被开发热重载
- **THEN** 下次启动立即复用完整搜索索引并在后台修复持久化状态，不清空查询索引或重新执行全量文件名分词

#### Scenario: 文件系统增量变化

- **WHEN** 已索引范围中的文件被创建、删除、重命名或移动
- **THEN** 系统使用该 provider 的原生变化源增量更新对应记录，不因单个事件重扫所有卷

#### Scenario: 索引数据仅保存在本地

- **WHEN** 系统建立、查询或清除文件索引
- **THEN** 文件名、路径和元数据只存储在本地应用数据目录，不上传至任何服务

### Requirement: Windows NTFS MFT 快速索引

系统 SHALL 在受支持的 Windows 本地固定 NTFS 卷上使用 LogCrate Index Service 通过 `FSCTL_ENUM_USN_DATA` 批量枚举 MFT 名称、文件引用、父引用和属性记录，并在普通用户进程中重建规范化路径。Windows NTFS 快速 provider MUST NOT 以递归打开目录或逐文件读取 metadata 作为发现文件名的主路径，LogCrate GUI MUST NOT 因此以管理员权限运行。

#### Scenario: 首次建立 NTFS 快速索引

- **WHEN** 用户已批准并启用版本兼容的 LogCrate Index Service，且搜索范围包含本地固定 NTFS 卷
- **THEN** 服务流式返回有界批次的 MFT 记录，普通用户进程重建路径并持续发布可查询的部分结果，无需递归遍历该卷目录

#### Scenario: 百万路径端到端性能门槛

- **WHEN** 在项目指定的 Windows/NTFS 本地 SSD 参考数据集上执行至少 1,000,000 个文件和目录的全新索引
- **THEN** 系统在 10 秒内提供首批可搜索结果、60 秒内完成可搜索索引，且 UI 输入响应的 p95 不超过 100 毫秒

#### Scenario: 主程序保持普通权限

- **WHEN** 用户在已安装索引服务的 Windows 设备上启动或使用 LogCrate
- **THEN** GUI、WebView、归档解析和结果打开操作继续在当前用户权限下运行，仅索引服务持有读取卷元数据所需权限

#### Scenario: 非 NTFS 或快速 provider 不可用

- **WHEN** 搜索范围不是受支持的 NTFS 卷，或索引服务缺失、版本不兼容、被禁用或无法启动
- **THEN** 系统明确显示降级原因，并允许用户修复服务、使用兼容目录扫描或排除该卷，不将兼容扫描描述为 Everything 级性能

### Requirement: Windows USN 增量恢复

系统 SHALL 为每个 NTFS 卷持久化 volume identity、USN journal ID 与 next USN，并通过索引服务读取 USN Journal 维护创建、删除、重命名和移动变化。应用关闭期间 MUST NOT 依赖 LogCrate 进程持续运行来保留变化。

#### Scenario: 热启动只追赶变化

- **WHEN** 已有完整卷快照，volume identity 与 journal ID 未变化，且保存的 next USN 仍位于有效 journal 范围
- **THEN** 系统加载快照并仅重放 next USN 之后的变化，不重新枚举该卷 MFT 或递归扫描目录

#### Scenario: USN 断点失效

- **WHEN** journal 被删除或重建、保存的 next USN 早于 `FirstUsn`，或 volume identity 发生变化
- **THEN** 系统将该卷标记为需要重建，仅重新枚举受影响卷，并在重建期间明确标记结果可能不完整

#### Scenario: 重命名与移动保持一致

- **WHEN** USN Journal 报告文件或目录的重命名、移动、创建或删除记录
- **THEN** 系统按文件引用与父引用更新受影响路径及后代关系，不因单个变化重建所有卷

### Requirement: Windows 索引服务安全与生命周期

系统 SHALL 仅在用户通过安装器明确批准系统提权后安装按需启动的 LogCrate Index Service。服务 SHALL 只读取本地卷的 MFT/USN 元数据并通过版本化本机 IPC 返回有界记录，MUST NOT 读取文件内容、访问网络或提供任意路径的打开、复制、修改、删除和重命名能力。卸载 LogCrate SHALL 停止并移除该服务。

#### Scenario: 首次启用前披露文件名访问

- **WHEN** 用户首次启用 Windows NTFS 快速搜索
- **THEN** 系统说明索引服务能够枚举本机 NTFS 文件名、索引仅存于当前用户本地且打开文件仍受当前用户权限约束，并在用户确认后继续

#### Scenario: IPC 客户端与消息校验

- **WHEN** 客户端连接索引服务或发送版本握手、卷枚举、MFT/USN 读取请求
- **THEN** 服务仅接受本机已认证交互用户，校验协议版本、消息长度、批次、卷标识和并发上限，并拒绝畸形或越权请求

#### Scenario: 服务不提升文件内容权限

- **WHEN** 搜索结果来自当前用户原本无权读取的目录或文件
- **THEN** 服务最多返回文件名元数据，预览、定位和加入监控仍由普通用户进程重新校验并按原权限成功或失败

#### Scenario: 服务升级与卸载

- **WHEN** LogCrate 升级、修复或卸载
- **THEN** 安装器原子替换版本匹配且已签名的服务，协议握手失败时不使用不兼容服务，卸载时停止并删除服务注册与二进制

### Requirement: 即时文件名与路径查询

系统 SHALL 使用 Orange 的文件名切词架构提供不区分大小写查询，支持 ASCII 驼峰拆词、中文 Jieba 切词、简繁转换、拼音与拼音首字母，并允许按文件类型或后缀过滤。显式完整路径片段 SHALL 使用路径查询回退。查询 SHALL 分页返回有界结果，MUST NOT 为每次输入同步扫描全部元数据或计算全库精确命中总数，快速连续输入时只有最新查询可更新 UI。

#### Scenario: 按部分文件名即时搜索

- **WHEN** 索引已就绪且用户输入文件名中的完整词项、驼峰组成词、中文词或拼音
- **THEN** 系统在不遍历文件系统的情况下返回排序后的首页匹配结果

#### Scenario: 多词路径匹配

- **WHEN** 用户输入多个空格分隔词项
- **THEN** 系统使用 Orange QueryParser 对所有切词执行 AND 有界查询，完整文件名匹配优先，并返回匹配文件的完整路径

#### Scenario: 快速更改查询

- **WHEN** 用户在旧查询尚未返回时继续输入新字符
- **THEN** 系统取消或废弃旧查询结果，仅展示最新查询对应的文件

### Requirement: 搜索结果预览日志

系统 SHALL 对搜索结果重新校验存在性、可读性与文件类型。用户双击 LogCrate 可识别的裸文本日志时，系统 SHALL 复用现有查看会话直接打开预览；双击受支持归档时 SHALL 允许用户浏览并打开其日志条目。预览 MUST NOT 隐式将父目录加入监控。

#### Scenario: 双击搜索到的裸日志

- **WHEN** 用户双击一个经重新校验为可读文本日志的搜索结果
- **THEN** 系统立即创建或激活查看选项卡，在后台建立行索引并显示已就绪内容

#### Scenario: 双击搜索到的受支持归档

- **WHEN** 用户双击一个 ZIP、7z、RAR、TAR 或其它 LogCrate 受支持归档
- **THEN** 系统使用现有归档读取链路展示可浏览条目，不先将整个归档解压到用户目录

#### Scenario: 搜索结果已失效

- **WHEN** 用户双击的结果已被删除、移动或变为不可读
- **THEN** 系统显示可读错误并从索引移除或更新过期记录，不创建无效查看会话

### Requirement: 从搜索结果加入监控

系统 SHALL 在搜索结果右键菜单提供“将所在目录加入监控”。该操作 SHALL 使用结果的父目录调用现有监控根规范化、覆盖去重、watcher 恢复与持久化行为，成功后 SHALL 在目录树定位原文件。

#### Scenario: 将结果所在目录加入监控

- **WHEN** 用户右键一个未被监控根覆盖的搜索结果并选择“将所在目录加入监控”
- **THEN** 系统将该文件的父目录加入监控、刷新目录树并定位该文件

#### Scenario: 父目录已被监控

- **WHEN** 搜索结果已位于某个现有监控根的任意深度子目录
- **THEN** 系统不创建重复监控根，只展开目录树并定位原文件

#### Scenario: 文件在加入监控前失效

- **WHEN** 搜索结果的文件在用户选择右键操作前已被删除或移动
- **THEN** 系统不添加过期父目录，显示可读错误并更新搜索结果

### Requirement: 搜索面板交互与可访问性

系统 SHALL 将顶栏搜索占位符替换为可聚焦、可点击的搜索入口，并提供键盘快捷键打开搜索。搜索面板 SHALL 支持键盘选择结果、Enter 打开、Escape 关闭、双击打开与右键菜单，并为输入、进度、结果数、结果列表和错误状态提供中英文可访问标签。

#### Scenario: 通过顶栏打开搜索

- **WHEN** 用户点击顶栏搜索入口或按下搜索快捷键
- **THEN** 系统打开搜索面板并将焦点放入搜索输入框

#### Scenario: 用键盘打开结果

- **WHEN** 用户用上下方向键选中结果并按 Enter
- **THEN** 系统执行与双击该结果相同的打开行为

#### Scenario: 关闭搜索后恢复焦点

- **WHEN** 用户按 Escape 或使用关闭按钮关闭搜索面板
- **THEN** 系统将焦点恢复到打开搜索前的界面元素，不改变当前日志选项卡与滚动位置

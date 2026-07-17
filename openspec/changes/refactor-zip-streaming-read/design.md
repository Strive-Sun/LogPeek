## Context

`ZipArchiveReader::open_entry` 当前通过 `read_to_end` 把条目完整复制到 `Vec<u8>`,再返回 `Cursor<Vec<u8>>`。这规避了 `ZipFile` 借用 `ZipArchive` 的生命周期问题,但使内存峰值与条目大小线性相关,并导致后台索引开始前存在一段不可观测的整条目预读取。刚完成的增量索引管线要求归档首批字节尽快进入 `SessionManager::index`。

## Goals / Non-Goals

- Goals:
  - Deflate 条目以固定缓冲顺序解压,不整条目驻留内存。
  - Stored 条目提供受条目边界约束的字节 seek。
  - 保持实际读取字节上限、加密错误、取消与缓存清理行为。
  - 首批字节可立即参与增量索引和进度反馈。
- Non-Goals:
  - 不为 Deflate 实现通用随机解压。
  - 不承诺在行索引建立前按未知行号随机跳转。
  - 不在归档读取层创建磁盘缓存。

## Decisions

- Decision: 将条目能力显式建模为顺序读取与可 seek 读取,而不是让调用方通过具体 zip 类型猜测。
  - `ArchiveReader` 可返回能力枚举或统一包装类型；Stored 变体实现 `Read + Seek`,Deflate 变体实现顺序 `Read`。
  - Alternatives considered: 始终返回 `Box<dyn Read>` 并另加布尔标志。该方案无法让调用方安全调用 seek,容易再次依赖向下转型或格式判断。
- Decision: Deflate 读取器的所有者必须与条目借用处于同一后台任务生命周期内。
  - 优先让后台闭包持有 `ZipArchiveReader`,在闭包内部取得借用的 `ZipFile` 并同步消费到索引完成,避免自引用结构和整条目复制。
  - 若 zip crate 的具体 API 无法满足该生命周期,再引入拥有底层文件和解压状态的专用读取器,但不得回退为完整 `Vec<u8>`。
- Decision: Stored seek 以条目数据起点为基准,所有 seek/read 均夹在 `[0, uncompressed_size]` 范围内。
  - 直接读取归档文件时必须记录条目 `data_start` 与长度,防止越界读到下一个条目或中央目录。
- Decision: 行号跳转仍由查看层 offsets 决定。
  - Stored 字节 seek 能加速已知字节范围读取,但行号到字节偏移的映射仍需增量行索引。
- Decision: 针对性测试先于实现替换。
  - 测试覆盖 Stored/Deflate 内容、固定缓冲消费、条目边界、安全熔断、加密错误与取消清理。

## Risks / Trade-offs

- zip crate 的 `ZipFile` 借用模型可能限制 trait object 的生命周期 → 保持“创建流并消费流”位于同一后台闭包,避免跨线程返回借用对象。
- Stored 直接读取与查看层缓存可能形成两套路径 → 首版允许查看层继续统一缓存,但归档层仍暴露正确能力；只有在能保持边建边读与清理语义时才绕过缓存。
- 高频小块读取可能降低吞吐 → 使用固定大小缓冲并以基准测试选择容量,不通过整条目缓冲换取吞吐。
- 取消只能在下一次 `Read` 返回后生效 → 控制单次缓冲大小,在块间检查取消标志。

## Migration Plan

1. 为现有行为和新能力增加针对性测试。
2. 扩展归档条目能力契约,保持裸文本 reader 兼容。
3. 替换 Deflate 的 `read_to_end` 路径并验证增量进度。
4. 增加 Stored 有界 seek reader。
5. 验证内存峰值、安全熔断、会话取消和跨平台构建。

## Open Questions

- zip crate 当前版本是否稳定暴露 Stored 条目的 `data_start`;若不可用,是否需要在归档层解析本地文件头。
- 查看层首版是否立即利用 Stored seek 绕过缓存,还是仅先暴露能力并在后续优化随机读取路径。

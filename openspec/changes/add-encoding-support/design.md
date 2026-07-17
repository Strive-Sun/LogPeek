## Context

当前缓存保存原始字节,`Session` 保存字节 offsets 与一个 `encoding_rs::Encoding`。初始索引先按单字节 `\n` 扫描,之后才根据样本更新编码；这对 UTF-8/GB18030 基本成立,但对 UTF-16LE/BE 不成立。`prepare` 又必须立即返回,因此真实检测编码只能在后台取得并通过事件或查询同步给前端。手动切换到不同编码时,既要改变解码器,也可能必须重建行偏移。

## Goals / Non-Goals

- Goals:
  - 正确检测、索引和解码 UTF-8、GB18030、UTF-16LE/BE。
  - 保持流式读取和固定大小采样,不整文件载入内存。
  - 手动编码切换不阻塞 UI,且不会产生旧任务覆盖新选择的竞态。
  - 编码控件展示检测值与当前生效值。
- Non-Goals:
  - 不实现任意遗留编码全集。
  - 不做语言识别或复杂统计式编码分类器。
  - 不在前端解码日志字节。

## Decisions

- Decision: 初始索引先读取有限前缀(例如 4KB)完成编码检测,再把该前缀与后续 reader 串接为同一输入流。
  - 前缀必须参与缓存写入和索引,不得丢失或重复。
  - Alternatives considered: 索引结束后再检测。该方案会让 UTF-16 offsets 从一开始就错误,不可接受。
- Decision: 换行扫描按编码族实现。
  - UTF-8/GB18030 为 ASCII 兼容编码,可按字节 `0x0A` 建索引并在读取时去除 `0x0D`。
  - UTF-16LE/BE 按偶数字节边界和对应字节序识别 `U+000A`/`U+000D`,BOM 不进入首行正文。
- Decision: Session 同时记录 `detected_encoding`、`effective_encoding` 与 generation。
  - 自动检测更新 detected/effective；手动选择只覆盖 effective,保留 detected 供 UI 展示“自动检测”结果。
- Decision: 后台索引进度载荷或专用查询返回检测值和生效值。
  - `prepare` 可返回“检测中”占位；首批采样完成后事件更新前端。
- Decision: 手动切换编码在后台扫描现有原始缓存并构建新的 offsets,完成前继续保留旧 encoding/offsets 可读。
  - 新 offsets 完成后在一个临界区原子替换 encoding 与 offsets,随后通知前端清空行缓存。
  - 对每次切换递增 generation；任务提交结果前必须确认 generation 仍为当前值。
- Decision: 编码切换复用会话生命周期取消机制。
  - 会话关闭/LRU 回收后,重建任务停止且不得重新创建已释放资源。

## Risks / Trade-offs

- 自动检测可能无法区分短小 ASCII 与 GB18030 → ASCII 内容按 UTF-8 展示等价；用户仍可手动覆盖。
- UTF-16 无 BOM 时检测不可靠 → 首版只保证含 BOM 的 UTF-16,无 BOM 可通过手动选择处理。
- 大文件手动切换需要重新扫描缓存 → 后台执行并显示进度,旧视图在原子切换前保持可用。
- 编码进度复用 `index-progress` 可能混淆初始索引状态 → 事件载荷需区分 operation/generation,或使用独立 `encoding-progress`；实现时选择较小且类型清晰的方案。

## Migration Plan

1. 增加各编码与换行组合的失败测试。
2. 将初始采样前移到 offsets 发布之前,实现编码感知换行扫描。
3. 扩展会话编码状态及前端同步协议。
4. 实现后台手动重建、generation 防竞态与缓存刷新。
5. 验证大文件切换响应性及会话关闭清理。

## Open Questions

- 采用扩展 `index-progress` 还是独立 `encoding-progress` 事件；以避免前端状态机歧义为首要标准。
- `encoding_rs` 对 GB18030 四字节序列的支持范围是否满足样本；若不满足,是否引入轻量替代 crate,并评估安装包体积影响。

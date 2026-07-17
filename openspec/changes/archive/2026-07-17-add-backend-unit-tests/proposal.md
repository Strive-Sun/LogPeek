# Change: 补齐后端单元测试覆盖

## Why
后端自动化覆盖仍明显不足：当前只有 `index.rs` 中 2 个增量索引/边界读取测试,归档读取、目录监控、完整行索引矩阵、编码与缓存生命周期基本没有回归保护。CI 已在 Windows 与 macOS 执行 `cargo test`,但现有用例不足以守护核心解析逻辑。需要在 zip 流式读取和编码支持各自交付针对性测试的基础上,补齐跨模块测试矩阵并消除重复夹具。

## What Changes
- 汇总并补齐 `archive-reading` 单元测试:列条目仅读清单不解压、打开条目、passthrough、非文本条目标记、实际读取字节熔断
- 为 `directory-monitoring` 增加单元测试:类型判定(zip/裸文本)、后缀筛选、配置持久化读写、失效目录跳过
- 为 `log-viewing` 增加单元测试:行偏移索引正确性、边界行读取、CRLF/LF 切分、编码解码(UTF-8/GBK/UTF-16 BOM)、超大单行截断、会话关闭与缓存清理
- 复用 `refactor-zip-streaming-read` 与 `add-encoding-support` 已交付的针对性用例,不重复实现同一测试夹具
- 确认既有跨平台 CI 执行完整测试集,测试暴露的缺陷回到所属 change 修复,避免本 change 吞并功能范围

## Impact
- Affected specs: `backend-testing`(ADDED:后端测试与 CI 质量能力；不把工程过程要求混入产品能力 spec)
- Affected code:
  - `src-tauri/src/archive/`(新增 `#[cfg(test)]` 模块与测试夹具)
  - `src-tauri/src/watcher.rs`
  - `src-tauri/src/index.rs`
  - `.github/workflows/ci.yml`(确保 `cargo test` 执行)

## Sequencing
- 在 `refactor-zip-streaming-read`、`add-frontend-lint-tooling`、`add-encoding-support` 之后收口
- 前置功能 change 必须各自先带针对性回归测试；本 change 负责补齐矩阵、共享夹具和跨模块边界,不是把测试推迟到最后

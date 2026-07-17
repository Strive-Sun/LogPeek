## ADDED Requirements
### Requirement: 后端核心模块测试覆盖
后端 SHALL 具备自动化测试套件,覆盖归档读取、目录监控与日志查看的核心正确性、安全边界和资源生命周期。测试 SHALL 优先复用各功能 change 已建立的针对性夹具,避免同一行为由互不一致的重复夹具验证。

#### Scenario: 归档读取回归矩阵
- **WHEN** 运行后端测试套件
- **THEN** 测试覆盖列条目不解压、Stored/Deflate 流式读取、Stored 有界 seek、裸文本 passthrough、非文本判定、加密错误与实际字节熔断

#### Scenario: 目录监控回归矩阵
- **WHEN** 运行后端测试套件
- **THEN** 测试覆盖文件类型判定、后缀筛选、配置持久化回环与失效目录处理

#### Scenario: 日志查看回归矩阵
- **WHEN** 运行后端测试套件
- **THEN** 测试覆盖增量行索引、空行和尾行边界、LF/CRLF、受支持编码、超长行截断、会话关闭与 LRU 缓存清理

### Requirement: 后端测试持续集成
后端测试套件 SHALL 在 Windows 与 macOS CI 中自动执行；新增用例 SHALL 进入既有 `cargo test` 步骤,测试失败 SHALL 阻止变更合入。

#### Scenario: 跨平台 CI 执行
- **WHEN** 代码被推送到主分支或创建 pull request
- **THEN** CI 在 Windows 与 macOS 矩阵运行完整 `cargo test`,任一平台失败则 CI 失败

#### Scenario: 功能 change 提供针对性测试
- **WHEN** zip 流式读取、编码处理或其他核心后端行为发生变化
- **THEN** 对应功能 change 同时更新其针对性测试,后端测试收口 change 只补齐共享矩阵而不延后关键回归保护

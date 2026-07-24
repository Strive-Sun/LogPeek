## ADDED Requirements

### Requirement: 开发阶段品牌标识统一

系统 SHALL 在首次正式发布前统一使用 LogCrate 品牌标识。Tauri bundle identifier MUST 为 `com.logcrate.app`，Cargo package、Rust library、npm package、缓存目录、本地存储键、内部 ID 及运行时临时文件前缀 MUST 使用 `logcrate`，且新构建 MUST NOT 为未发布的 LogPeek 开发版本保留配置迁移逻辑。

#### Scenario: 编译应用

- **WHEN** 开发者编译桌面应用或索引服务
- **THEN** Cargo 和 npm 构建输出使用 `logcrate`，不再显示 `logpeek`

#### Scenario: 首次启动开发版本

- **WHEN** 用户启动尚未正式发布的 LogCrate 构建
- **THEN** 应用使用 `com.logcrate.app`、`logcrate-cache` 和 `logcrate` 本地存储键创建全新的开发期数据

## REMOVED Requirements

### Requirement: 改名升级兼容

**Reason**: LogCrate 尚未正式发布且没有 LogPeek 用户需要原位升级，保留旧标识会与当前产品品牌冲突。

**Migration**: 不迁移开发机上的 LogPeek 配置；首次正式发布仅使用 LogCrate 标识和存储位置。

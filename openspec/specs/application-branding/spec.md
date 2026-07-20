# application-branding Specification

## Purpose
TBD - created by archiving change rename-product-to-logcrate. Update Purpose after archive.
## Requirements
### Requirement: LogCrate 产品身份

系统 SHALL 在当前用户可见的应用元数据、主窗口、顶栏、托盘、安装包、正式 Release 和文档中使用产品名 `LogCrate`，并以归档日志阅读器作为用途说明。历史版本记录 MAY 保留发布时使用的旧名称 `LogPeek`。

#### Scenario: 查看当前产品名称

- **WHEN** 用户启动应用、查看窗口/托盘、安装包、当前 README 或正式 Release
- **THEN** 系统显示 `LogCrate`，且主要文档说明其用于监控目录和免手动解压阅读归档日志

#### Scenario: 保留历史记录

- **WHEN** 用户查看旧版本 CHANGELOG、归档规格或旧 tag
- **THEN** 系统允许这些不可变历史内容继续使用当时的 `LogPeek` 名称

### Requirement: 可辨识的归档日志图标

系统 SHALL 使用同一矢量母版生成平台图标，图形 MUST 在常用尺寸下同时表达归档容器和文本日志，不依赖文字识别。Windows、macOS、README、任务栏、托盘和安装器 SHALL 使用同一品牌图形。

#### Scenario: 查看正常尺寸图标

- **WHEN** 用户在 README、应用列表或安装器中查看 128px 及以上图标
- **THEN** 图标清晰显示打开的归档箱和日志行，颜色与边界完整

#### Scenario: 查看小尺寸图标

- **WHEN** 系统在任务栏、托盘或文件列表以 16px–32px 显示图标
- **THEN** 图标仍能辨认出箱体和内容线，不出现无法区分的细碎文字或模糊元素

### Requirement: 改名升级兼容

系统 MUST 在品牌与仓库改名时保留已有 bundle identifier、用户配置与本地存储键、updater 签名信任和正式更新源，使已安装 LogPeek/LogCrate 旧版本的用户能够原位升级。新构建 SHALL 使用 `Strive-Sun/LogCrate` 作为规范仓库地址；旧仓库路径仅用于已发布客户端的重定向兼容。迁移 MUST NOT 清空监控目录、筛选、语言、布局或其它持久化设置。

#### Scenario: 从旧品牌升级

- **WHEN** 已安装 LogPeek 或使用旧仓库 endpoint 的 LogCrate 用户通过内置 updater 安装新版本
- **THEN** 系统将现有应用原位升级，仅保留一个应用实例，并继续使用原监控配置和用户设置

#### Scenario: 新品牌继续检查更新

- **WHEN** 新构建的 LogCrate 启动自动检查或用户手动检查更新
- **THEN** 系统继续信任原 updater 公钥并直接从 `Strive-Sun/LogCrate` 的正式 Release endpoint 获取签名更新

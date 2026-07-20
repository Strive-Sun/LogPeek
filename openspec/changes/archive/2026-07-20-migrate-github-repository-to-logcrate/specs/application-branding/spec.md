## MODIFIED Requirements

### Requirement: 改名升级兼容

系统 MUST 在品牌与仓库改名时保留已有 bundle identifier、用户配置与本地存储键、updater 签名信任和正式更新源，使已安装 LogPeek/LogCrate 旧版本的用户能够原位升级。新构建 SHALL 使用 `Strive-Sun/LogCrate` 作为规范仓库地址；旧仓库路径仅用于已发布客户端的重定向兼容。迁移 MUST NOT 清空监控目录、筛选、语言、布局或其它持久化设置。

#### Scenario: 从旧品牌升级

- **WHEN** 已安装 LogPeek 或使用旧仓库 endpoint 的 LogCrate 用户通过内置 updater 安装新版本
- **THEN** 系统将现有应用原位升级，仅保留一个应用实例，并继续使用原监控配置和用户设置

#### Scenario: 新品牌继续检查更新

- **WHEN** 新构建的 LogCrate 启动自动检查或用户手动检查更新
- **THEN** 系统继续信任原 updater 公钥并直接从 `Strive-Sun/LogCrate` 的正式 Release endpoint 获取签名更新

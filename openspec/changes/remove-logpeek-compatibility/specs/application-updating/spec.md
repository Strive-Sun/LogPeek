## MODIFIED Requirements

### Requirement: 正式版本与签名发布

系统 SHALL 仅通过固定的官方 LogCrate GitHub Releases updater endpoint 获取最新正式版本。新构建 MUST 使用 `Strive-Sun/LogCrate` 规范路径。Release 流程 MUST 为 Windows 与 macOS 更新包生成签名和更新清单，且签名私钥 MUST NOT 存储在仓库或应用包内。

#### Scenario: 正式版本可更新

- **WHEN** `vX.Y.Z` 正式 tag 的 Release workflow 成功完成
- **THEN** LogCrate 仓库的 Release 同时包含 updater 清单、签名以及 Windows/macOS 可安装更新包，并使用当前 LogCrate 品牌展示

#### Scenario: 签名配置缺失

- **WHEN** Release workflow 缺少 updater 私钥或必需密码
- **THEN** 发布在生成不完整 Release 前明确失败

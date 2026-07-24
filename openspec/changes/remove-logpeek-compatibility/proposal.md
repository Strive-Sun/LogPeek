# Change: 移除开发阶段的 LogPeek 兼容标识

## Why

LogCrate 尚处于开发阶段，没有已安装用户需要从 LogPeek 原位升级。继续保留旧 bundle identifier、缓存目录、本地存储键和内部命名会造成构建输出与产品品牌不一致，也增加后续正式发布前的迁移成本。

## What Changes

- 将 Tauri identifier 从 `com.logpeek.app` 改为 `com.logcrate.app`。
- 将缓存目录、本地存储键、内部 ID、临时文件及测试前缀统一为 `logcrate`。
- 将 Cargo package、Rust library 和 npm package 名称统一为 LogCrate。
- 删除对尚未发布的 LogPeek 客户端、旧配置键和旧 updater 路径的兼容要求。

## Impact

- Affected specs: application-branding, application-updating
- Affected code: Tauri/Cargo/npm 配置、缓存和存储键、测试与发布脚本
- Breaking impact: 开发机上的旧 LogPeek 配置不会迁移；项目尚未发布，因此无需数据迁移

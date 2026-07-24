## 1. 品牌标识清理

- [x] 1.1 将 Cargo package、Rust library 和 npm package 名称改为 LogCrate
- [x] 1.2 将 Tauri identifier 改为 `com.logcrate.app`
- [x] 1.3 将缓存目录、本地存储键、内部 ID 和临时文件前缀改为 `logcrate`
- [x] 1.4 更新相关测试、发布脚本和代码注释

## 2. 验证

- [x] 2.1 非历史归档代码和配置中不再残留 `logpeek`
- [x] 2.2 前端 build、test、lint 与 format 通过
- [x] 2.3 Rust check、test、clippy 与 fmt 通过
- [x] 2.4 OpenSpec strict 校验通过

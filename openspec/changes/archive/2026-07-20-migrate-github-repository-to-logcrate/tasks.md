## 1. 仓库与文档地址

- [x] 1.1 将 README、README_ZH 的徽章、Actions、下载、反馈和当前仓库链接改为 `Strive-Sun/LogCrate`
- [x] 1.2 将本地 git `origin` 更新为 `https://github.com/Strive-Sun/LogCrate.git` 并验证 fetch/push 目标
- [x] 1.3 扫描非历史文件中的旧仓库 URL，确认仅保留明确的兼容说明与测试引用

## 2. 更新兼容

- [x] 2.1 将新构建的 Tauri updater endpoint 改为 LogCrate 仓库地址，保持公钥、identifier 与存储键不变
- [x] 2.2 更新品牌兼容检查，要求当前配置使用新 endpoint 并保护现有签名公钥和 legacy 数据键
- [x] 2.3 验证旧 LogPeek endpoint 永久重定向到新地址，且最终 latest.json、更新包与签名可访问
- [ ] 2.4 从已安装 v1.0.7 手动检查并安装下一测试版本，确认更新成功且配置保留

## 3. 文档与验证

- [x] 3.1 在 CHANGELOG Unreleased 逐条记录仓库与 updater 地址迁移
- [x] 3.2 运行品牌/发布检查、前端格式/测试/lint/构建、Rust 检查与 OpenSpec 严格校验

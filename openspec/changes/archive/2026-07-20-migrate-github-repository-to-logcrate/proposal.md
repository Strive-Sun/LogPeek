# Change: 将 GitHub 仓库地址迁移到 LogCrate

## Why

产品已经从 LogPeek 更名为 LogCrate，GitHub 仓库也已迁移到 `Strive-Sun/LogCrate`，但 README、状态徽章、下载入口、git remote 与新构建的 updater endpoint 仍引用旧仓库。继续保留旧地址会让当前品牌与官方分发入口不一致，也会长期依赖 GitHub 重定向。

已验证旧 updater 地址会以 HTTP 301 永久重定向到新仓库，且新仓库的 latest endpoint 已指向 `v1.0.7/latest.json`。因此可以让新构建直接使用新地址，同时让无法修改的旧安装版本通过 GitHub 重定向继续更新。

## What Changes

- 将当前 README、README_ZH、徽章、Actions、下载和问题反馈链接统一迁移到 `https://github.com/Strive-Sun/LogCrate`。
- 将 Tauri updater endpoint 改为新仓库的 `latest.json`，继续保留现有 updater 公钥、bundle identifier 与用户设置键。
- 更新品牌兼容检查，使其要求新构建使用 LogCrate 规范地址，并记录旧 LogPeek endpoint 仅用于旧客户端兼容验证。
- 将本地 git `origin` 更新为新仓库地址；tag、Release workflow、签名协议和 `latest.json` 格式保持不变。
- 在 CHANGELOG Unreleased 中逐条记录仓库地址与更新源迁移。
- 验证旧 endpoint 的永久重定向与新 endpoint 的签名更新清单可用，确保 v1.0.7 及更早安装版不会失去更新路径。

## Impact

- Affected specs: `application-branding`、`application-updating`
- Affected code/config: `src-tauri/tauri.conf.json`、`scripts/brand-check.mjs`、README、README_ZH、git remote、CHANGELOG
- Compatibility: 旧安装版继续请求 `Strive-Sun/LogPeek`，依赖 GitHub 301 重定向到 `Strive-Sun/LogCrate`；新安装版直接请求新地址
- Security: updater 公钥和签名私钥不变，不建立新的信任链

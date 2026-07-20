## Context

GitHub 仓库从 `Strive-Sun/LogPeek` 改名为 `Strive-Sun/LogCrate`。当前源码中的 updater endpoint 和文档链接仍使用旧地址；已经发布的桌面客户端则无法修改其内置 endpoint。GitHub 当前对旧 Release 路径返回 301，并将请求转发到同一仓库的新名称。

## Goals / Non-Goals

- Goals:
  - 当前文档、开发 remote 和新构建统一使用 `Strive-Sun/LogCrate`。
  - 已安装旧版本仍能通过原 endpoint 获取同一签名信任链下的更新。
  - updater 公钥、bundle identifier、用户数据与 Release 格式保持不变。
- Non-Goals:
  - 不修改历史 CHANGELOG、归档 OpenSpec、旧 tag 或旧 Release Notes 中的仓库名称。
  - 不引入自建 CDN、镜像更新源或新的签名密钥。
  - 不重写已经发布客户端中的 endpoint。

## Decisions

### 新客户端使用规范地址

`tauri.conf.json` 只配置 `https://github.com/Strive-Sun/LogCrate/releases/latest/download/latest.json`。新客户端不再额外请求旧地址，避免重复检查和长期依赖重定向。

### 旧客户端依赖 GitHub 仓库改名重定向

旧客户端继续访问 `Strive-Sun/LogPeek`。发布前验证该地址返回到新 endpoint 的永久重定向，并验证最终 `latest.json` 可下载。重定向是旧客户端唯一可行的无重新分发迁移路径。

### 保持签名身份不变

仓库地址不是 updater 的签名身份。继续使用现有公钥和 GitHub Actions secrets，使新仓库中的更新包可被旧客户端验证；不得重新生成密钥或修改 bundle identifier。

### 历史边界

只更新当前文档与配置。旧版本记录保留当时的 URL，既避免无意义改写，也能解释旧客户端为何仍请求旧路径。

## Risks / Trade-offs

- GitHub 将来若停止旧仓库重定向，旧客户端会失去更新入口 → 每次正式发布前检查旧 endpoint；若重定向异常，在 GitHub 仓库设置层恢复旧名称重定向或提供兼容仓库。
- 新仓库 Actions secrets 可能未随改名保留 → 发布前确认签名 Release 已生成 `latest.json`、更新包与 `.sig`。
- 文档遗漏旧链接 → 使用全仓扫描排除历史目录后核对所有剩余引用。

## Migration Plan

1. 验证旧、新 updater endpoint 及 v1.0.7 Release 资产。
2. 更新源码、文档、校验脚本和 git remote。
3. 运行品牌检查、发布检查、构建测试和 OpenSpec 严格校验。
4. 发布下一补丁版本，从 v1.0.7 手动检查并安装更新，确认签名链与用户配置保持正常。
5. 若失败，恢复新构建中的旧 endpoint；旧客户端本身不受源码回滚影响。

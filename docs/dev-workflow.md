# 开发协作流程

本文档记录 LogPeek 在使用 AI 助手(Claude)+ OpenSpec + Codex 审阅时的标准往返流程。

Codex 审阅通过 Claude Code 官方插件 [openai/codex-plugin-cc](https://github.com/openai/codex-plugin-cc) 的 `/codex:review` 命令完成(底层走 Codex 原生 review target,上下文独立、省 token),不再使用 `codex mcp-server`。

## 核心原则

- **规格先行**:功能/架构/破坏性改动先有 OpenSpec change 提案,再编码。
- **编码后必审**:任何代码改动完成后,在汇报前调用 codex 审阅未提交改动。
- **人工确认闭环**:每个 change 由用户实测确认后,才存档 openspec 并提交。

## 单个 OpenSpec change 的往返流程

以下步骤对每个 active change 依次执行,一次只推进一个 change:

1. **提交前置改动**
   开工前确保工作区干净;把上一轮已确认的改动提交,避免混入。

2. **编码**
   按 change 的 `proposal.md` / `design.md` / `tasks.md` 实现,逐项完成。

3. **自检**
   - 后端改动:`cargo check`(必要时 `cargo test`)
   - 前端改动:`tsc --noEmit` + `npm run build`
   - 自检失败先修复,不带病提交审阅。

4. **codex 审阅未提交改动**
   - 运行 `/codex:review`(codex-plugin-cc 提供),默认审阅**未提交的改动**(staged + unstaged + untracked);多文件改动用 `/codex:review --background` + `/codex:status` + `/codex:result`。
   - 将发现按严重程度汇总;对合理问题先修复再汇报,或说明为何不修。
   - 若审阅不可用(插件未装/未登录/超时),如实告知并继续,不静默跳过。

5. **汇报**
   向用户说明:实现了什么、自检结果、codex 审阅结论与处理。

6. **等待用户实测**
   用户在真实环境验证。**在收到用户确认前,不存档、不提交。**

7. **存档 + 提交**(收到用户确认后)
   - 将 change 的 `tasks.md` 全部标记为 `- [x]`(反映真实完成状态)。
   - `openspec archive <change-id> --yes` 归档,更新 `specs/` 基线。
   - `openspec validate --strict` 确认基线通过。
   - 提交 commit(含代码改动与 openspec 归档)。

8. **进入下一个 change**,回到步骤 1。

## 当前待推进的 change 顺序

1. `add-backend-suffix-filter` — 后端通知应用后缀筛选(小、修实际 bug)
2. `add-index-progress-events` — 后台索引 + 进度事件 + 边建边读
3. `add-encoding-support` — 编码检测(GB18030/UTF-16BE)+ 手动指定
4. `refactor-zip-streaming-read` — zip 条目真流式读取,消除整条目入内存
5. `add-backend-unit-tests` — 后端单元测试覆盖
6. `add-frontend-lint-tooling` — 前端 lint/format 工具链

> 顺序可按需调整;后端功能类先行,测试与工具链收尾。

## 相关约定

- Codex 审阅约定与插件安装步骤见 `CLAUDE.md`「代码审阅约定」。
- OpenSpec 规范见 `openspec/AGENTS.md`。
- git 安全:默认不改 `main` 直推;提交只在用户明确要求时进行。

## 发布版本号

- 应用版本的唯一人工维护来源是 `src-tauri/Cargo.toml` 中的 `[package].version`。
- `tauri.conf.json` 不重复声明版本；Tauri 2 会自动使用 Cargo package version。
- 根目录 npm package 是私有前端工程,不声明发布版本。
- 修改 `Cargo.toml` 后运行 `cargo check`,由 Cargo 自动同步 `Cargo.lock`,不要手工修改 lockfile。

### 发布步骤

1. 在 `src-tauri/Cargo.toml` 中修改 `[package].version`。
2. 在 `CHANGELOG.md` 中新增 `## [版本号] - YYYY-MM-DD` 章节。
3. 按“新增 / 优化 / 修复 / 工程质量”等类别组织内容；没有内容的类别可以省略。
4. 每项变化单独写一条 `- ` bullet,描述具体的用户可见变化或工程改进；禁止使用“修复若干问题”“待补充”等模糊占位描述。
5. 将 `Unreleased` 中已完成的条目移动到新版本章节,不要在两个章节重复保留。
6. 运行 `cargo check` 更新 `Cargo.lock`。
7. 运行 `npm run release:check`,确认 Cargo 版本、CHANGELOG 版本章节及更新列表一致。
8. 提交版本变更后创建 `v版本号` tag 并推送。

Release 工作流会再次校验 tag、Cargo 版本与 CHANGELOG 章节一致,并自动把该版本的逐条更新内容写入 GitHub Release Notes；任一项缺失或不一致都会停止发布。

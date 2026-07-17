# Change: 建立前端 lint/format 工具链

## Why
前端目前没有 Prettier/ESLint 配置与 lint 脚本；现有 CI 已执行 `tsc --noEmit` 与 build,但代码风格、React Hooks 用法和常见低级错误缺乏自动守护。该工具链应在后续编码下拉等前端交互开发前建立,避免功能改动与首次全仓格式化混在一起。`project.md` 已明确沿用手写轻量组件、暂不引入 Radix/shadcn,本 change 只核对并固化该策略,不重复引入 UI 依赖。

## What Changes
- 引入 Prettier 配置与 `format`/`format:check` 脚本
- 引入 ESLint(TypeScript + React Hooks 规则)与 `lint` 脚本
- 在现有 CI 前端 job 中增加 lint 与 format 检查步骤
- 保留并核对既有 UI 基元策略:沿用手写轻量组件,暂不引入 Radix;若未来交互复杂度上升再评估

## Impact
- Affected specs: `frontend-tooling`(新增能力)
- Affected code:
  - `package.json`(devDependencies + scripts)
  - `.prettierrc` / `.eslintrc`(或等价配置)
  - `.github/workflows/ci.yml`(lint/format 步骤)

## Sequencing
- 在 `add-encoding-support` 的前端编码选择控件之前执行
- 首次格式化独立成清晰变更,避免与功能逻辑修改混杂

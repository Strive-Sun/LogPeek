# Change: 实现建索引进度事件与边建边读

## Why
基线要求"建索引期间反馈进度、打开后首屏即时可见、支持边建边读",但当前后端 `open_log_session` 为同步阻塞,索引全部建完才返回,且从不 emit `index-progress` 事件——前端进度条组件因此是死代码,大文件打开时界面会假死。需要真正落地后台建索引 + 进度事件 + 已索引行数上界的边建边读。

## What Changes
- 后端 `open_log_session` 改为**后台**建索引(不阻塞命令返回),立即返回 `session_id`
- 后端在建索引期间周期性 emit `index-progress` 事件(含当前已索引行数上界、是否完成)
- `line_count`/`read_lines` SHALL 支持读取"当前已索引"范围(边建边读),不要求全部索引完成
- 前端订阅 `index-progress`,进度条随真实进度更新;首屏在索引未完成时即可呈现已就绪行

## Impact
- Affected specs: `log-viewing`(MODIFIED:建索引进度反馈、打开与浏览响应性)
- Affected code:
  - `src-tauri/src/index.rs`(后台索引线程、进度上报、已索引上界读取)
  - `src-tauri/src/lib.rs`(emit `index-progress`;命令改为立即返回)
  - `src/api/tauri.ts`(`subscribeIndexProgress` 接真实事件)
  - `src/components/LogContent.tsx`(进度条与边建边读接线)

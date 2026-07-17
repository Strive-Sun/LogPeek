# Change: 完善编码检测与手动指定编码

## Why
基线要求编码支持覆盖 UTF-8、GBK/GB18030、UTF-16(含 BOM)并允许用户手动指定编码,但当前实现:GB18030 未使用、UTF-16BE 被当作 LE 解码、前端"编码 ▾"仅为静态文本没有交互,也没有重解码命令。更关键的是,当前行索引直接在原始字节中搜索单字节 `\n`,对 UTF-16LE/BE 不能可靠建立行边界；只替换解码器不足以修复 UTF-16。后台索引开始后检测到的真实编码也尚未同步回前端。

## What Changes
- 后端编码检测支持 GB18030(GBK 的超集),正确区分 UTF-16LE 与 UTF-16BE(依 BOM)
- 初始建索引在发布任何行偏移前先完成编码采样,按生效编码建立 LF/CRLF 行边界
- 新增命令 `set_session_encoding(session_id, encoding)`:后台重建与目标编码匹配的行索引,完成后原子切换并刷新当前会话
- 索引进度/会话接口返回当前检测到和实际生效的编码,避免前端长期停留在初始占位值
- 前端"编码 ▾"改为可交互下拉:展示检测结果、允许手动切换编码并刷新视图

## Impact
- Affected specs: `log-viewing`(MODIFIED:文本编码检测与解码)
- Affected code:
  - `src-tauri/src/index.rs`(编码检测/解码、按会话重解码)
  - `src-tauri/src/lib.rs`(新增 `set_session_encoding` 命令)
  - `src/api/tauri.ts` / `src/api/mock.ts`(编码接口)
  - `src/components/LogContent.tsx`(编码下拉交互)

## Dependencies
- 在 `add-frontend-lint-tooling` 后执行,避免首次格式化与编码 UI 逻辑混杂
- 复用 `refactor-zip-streaming-read` 完成后的流式输入,编码采样不得重新引入整文件缓冲

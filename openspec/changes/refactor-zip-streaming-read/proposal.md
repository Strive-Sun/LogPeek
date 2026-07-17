# Change: zip 条目真流式读取,消除整条目入内存

## Why
基线要求 `open_entry` 返回可流式读取的解压流、Stored 条目可直接 seek。但当前实现仍在后台线程中先把整个条目读入 `Vec<u8>` 游标,之后才进入增量索引；几百 MB 的条目会造成等量内存峰值,且这段预读取期间没有 `index-progress`、首屏也不可读。需要改为真正的流式读取,让解压字节直接进入查看层的增量缓存/索引管线,并对 Stored 条目暴露受条目边界约束的字节 seek 能力。

## What Changes
- `open_entry` 改为返回**流式**解压读取器,不再将整条目缓冲进内存,首批字节可立即进入增量索引
- 区分 **Stored**(未压缩)与 **Deflate**(压缩)条目:Stored SHALL 暴露条目范围内的字节 seek;Deflate 为顺序流,随机访问所需的临时缓存由查看层按需引入
- 明确字节 seek 与按行跳转的边界:Stored 可直接定位字节偏移,但按行跳转仍依赖已建立的行索引,不承诺在索引前跳到未知行
- 保持既有安全边界(实际字节熔断、加密条目明确错误)不回退
- 先建立 Stored/Deflate、安全熔断与取消读取的针对性测试,再替换读取实现

## Impact
- Affected specs: `archive-reading`(MODIFIED:免解压读取)
- Affected code:
  - `src-tauri/src/archive/zip_reader.rs`(流式读取器、Stored seek)
  - `src-tauri/src/archive/mod.rs`(必要时扩展 trait 以暴露可 seek 能力)
  - `src-tauri/src/index.rs`(利用流式/可 seek 能力构建索引)
  - `src-tauri/src/lib.rs`(后台读取生命周期与进度事件保持连续)

## Sequencing
- 当前剩余 change 中优先执行,先修复大条目内存峰值与索引前无进度窗口
- 其针对性测试随本 change 一并交付,后续 `add-backend-unit-tests` 只做矩阵收口

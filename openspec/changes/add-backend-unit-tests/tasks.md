## 1. archive-reading 测试
- [ ] 1.1 汇总/复用 zip 流式 change 的夹具,验证 `list_archive_entries` 仅读中央目录、不产生解压产物
- [ ] 1.2 汇总/复用 `open_entry` 的 Stored、Deflate、有界 seek 与取消读取测试
- [ ] 1.3 裸文本 passthrough:单文件视为单条目
- [ ] 1.4 非文本条目被标记 `is_log=false`
- [ ] 1.5 声明大小不可信时仍按实际读取字节数执行超上限熔断

## 2. directory-monitoring 测试
- [ ] 2.1 `classify` 类型判定:zip magic、裸文本扩展名/采样
- [ ] 2.2 后缀筛选规则应用与边界
- [ ] 2.3 配置持久化写入与读取回环(含 suffixes/show_all)
- [ ] 2.4 失效目录跳过不 panic

## 3. log-viewing 测试
- [ ] 3.1 在现有 2 个增量索引测试基础上补齐行偏移矩阵(含空行、末行无换行)
- [ ] 3.2 边界行读取(start/count 越界、尾行)
- [ ] 3.3 LF 与 CRLF 切分,行尾 `\r`/`\n` 去除
- [ ] 3.4 汇总/复用编码 change 的 UTF-8、GB18030、UTF-16LE/BE 与换行测试
- [ ] 3.5 超大单行(> 64KB)截断并标记
- [ ] 3.6 `close_log_session` 释放缓存;LRU 回收后临时文件被删除

## 4. CI 集成
- [x] 4.1 确认现有 CI 已执行 `cargo test`(Windows + macOS 矩阵)
- [ ] 4.2 确认新增用例全部进入既有 CI,不存在平台条件导致的漏跑
- [ ] 4.3 修复本 change 范围内的测试性/夹具缺陷；功能缺陷回到对应 change 处理

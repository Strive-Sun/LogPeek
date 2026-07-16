## 1. 项目脚手架
- [ ] 1.1 初始化 Tauri 2.x 工程(`src-tauri` + 前端 Vite/React/TS)
- [ ] 1.2 配置 `cargo fmt` / `clippy` 与前端 Prettier/lint
- [ ] 1.3 确认 Windows + macOS 均能 `tauri dev` 启动空壳
- [ ] 1.4 搭建三栏布局骨架(可拖拽分栏)与 CSS 变量深浅色主题(跟随系统 + 手动切换)
- [ ] 1.5 引入 Radix 基元与 `@tanstack/react-virtual`

## 2. 归档读取(archive-reading)
- [ ] 2.1 定义 `ArchiveReader` trait(`entries()` / `open_entry() -> Read 流`)
- [ ] 2.2 实现 zip reader(基于 `zip` crate;区分 Stored 可 seek 与 Deflate 顺序流;`entries()` 仅读中央目录不解压)
- [ ] 2.3 实现裸文本 passthrough reader(单文件视为单条目归档,与包内条目共用查看路径)
- [ ] 2.4 实现 `is_log` 判定(扩展名 + 内容采样)
- [ ] 2.5 安全边界:以实际读取字节数熔断大小上限(防 zip bomb);加密/分卷归档返回明确错误
- [ ] 2.6 命令 `list_archive_entries(archive_path)`:免解压返回条目列表
- [ ] 2.7 单元测试:列条目仅读清单不解压、打开条目、passthrough、非文本条目标记、声明大小与实际不符时熔断

## 3. 目录监控(directory-monitoring)
- [ ] 3.1 基于 `notify` 实现多目录同时监听
- [ ] 3.2 实现文件到达大小稳定检测
- [ ] 3.3 到达后判定类型:zip 归档(扩展名/magic)或裸文本日志文件(扩展名/内容采样)
- [ ] 3.4 可配置后缀筛选:规则应用于目录树展示与新文件通知,与目录列表一同持久化
- [ ] 3.5 命令 `add_watch_dir` / `remove_watch_dir` / `list_watch_dirs`
- [ ] 3.6 监控目录列表 + 筛选规则持久化到本地配置(JSON);启动时读取并恢复监控
- [ ] 3.7 失效目录跳过不阻断启动
- [ ] 3.8 目录树惰性展开:展开 zip 节点时才 `list_archive_entries`;裸文本为叶子节点
- [ ] 3.9 单元测试:到达检测逻辑、类型判定、筛选、持久化读写、失效目录跳过

## 4. 新日志提示(log-notification)
- [ ] 4.1 后端在判定为日志包后 emit `new-archive-detected` 事件
- [ ] 4.2 前端顶栏组件:接收事件、显示计数与提示
- [ ] 4.3 点击提示 → 展开新日志包列表(计数不变)
- [ ] 4.4 查看某个新包 → 计数减一(已看集合去重,重复查看不递减)
- [ ] 4.5 "全部标记已读" → 计数清零
- [ ] 4.6 文件删除/同名覆盖时更新通知列表与计数(监听 remove/覆盖事件)

## 5. 日志查看:行索引 + 窗口化加载(log-viewing)
- [ ] 5.1 命令 `open_log_session`:后台流式扫描条目建行偏移索引,返回 `session_id`;Deflate 条目一趟解压到内部临时缓存(方案 A)后按缓存 seek;解压后 >2GB 拒绝并提示;写盘失败回退为仅顺序读
- [ ] 5.2 建索引进度通过 `index-progress` 事件反馈;支持边建边读(返回当前已索引行数上界)
- [ ] 5.3 命令 `read_lines(session_id, start, count)`:按偏移读取指定行范围;单行超阈值(如 64KB)截断并标记
- [ ] 5.4 文本编码检测与解码(UTF-8 / GBK/GB18030 / UTF-16 + BOM),支持手动指定编码
- [ ] 5.5 行分隔符兼容 LF/CRLF,返回行去除行尾 `\r`/`\n`
- [ ] 5.6 命令 `close_log_session`:释放行索引与内部临时缓存;会话数超上限时 LRU 回收;进程退出兜底清理所有残留缓存
- [ ] 5.7 前端虚拟滚动文本视图:按可视区调用 `read_lines`,支持随机跳转
- [ ] 5.8 建索引进度条 UI
- [ ] 5.9 超长行截断 / 横向虚拟滚动处理
- [ ] 5.10 单元测试:行偏移索引正确性、边界行读取、CRLF 切分、编码解码、超大单行截断、缓存清理
- [ ] 5.11 无感体验实测:几百 MB 压缩条目点开首屏 < ~200ms、顺序滚动无掉帧、已就绪范围随机跳转即时(见技术文档 4.4 指标)

## 6. 集成与验证
- [ ] 6.1 端到端手测:丢 zip 进监控目录 → 顶栏提示 → 点开 → 查看日志
- [ ] 6.2 Windows + macOS 双端各跑一遍闭环
- [ ] 6.3 `openspec validate add-zip-log-monitoring --strict` 通过

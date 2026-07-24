<p align="center">
  <a href="./README.md">English</a> · <strong>简体中文</strong>
</p>

<p align="center">
  <img src="resources/icons/app/128x128.png" width="88" height="88" alt="LogCrate logo">
</p>

<h1 align="center">LogCrate</h1>

<p align="center">
  <strong>监控目录，发现日志，不用解压，直接阅读。</strong>
</p>

<p align="center">
  面向 Windows 与 macOS 的轻量桌面日志阅读器。<br>
  把“下载压缩包 → 手动解压 → 翻找日志 → 打开查看”缩短为一次点击。
</p>

<p align="center">
  <a href="https://github.com/Strive-Sun/LogCrate/releases/latest"><img src="https://img.shields.io/github/v/release/Strive-Sun/LogCrate?style=flat-square&label=release" alt="Latest release"></a>
  <a href="https://github.com/Strive-Sun/LogCrate/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/Strive-Sun/LogCrate/ci.yml?branch=main&style=flat-square&label=CI" alt="CI status"></a>
  <img src="https://img.shields.io/badge/platform-Windows%20%7C%20macOS-4a9eff?style=flat-square" alt="Windows and macOS">
  <img src="https://img.shields.io/badge/built%20with-Tauri%202-24c8db?style=flat-square" alt="Built with Tauri 2">
</p>

<p align="center">
  <a href="https://github.com/Strive-Sun/LogCrate/releases/latest"><strong>下载最新版</strong></a>
  · <a href="CHANGELOG.md">更新日志</a>
  · <a href="docs/technical-design.md">技术设计</a>
</p>

<p align="center">
  <picture>
    <source
      media="(prefers-color-scheme: dark)"
      srcset="resources/screenshots/logcrate-hero-dark.png"
    >
    <source
      media="(prefers-color-scheme: light)"
      srcset="resources/screenshots/logcrate-hero-light.png"
    >
    <img
      src="resources/screenshots/logcrate-hero-light.png"
      alt="LogCrate 主界面预览，展示监控目录、展开的 ZIP、日志选项卡和日志正文"
      width="1200"
    >
  </picture>
</p>

---

## 为什么是 LogCrate

线上排障经常从一个 ZIP 开始：下载、解压、在多层目录中寻找 `.log` / `.txt`，再用编辑器逐个打开。文件越大、压缩包越多，这套重复操作越打断思路。

LogCrate 围绕这一条链路设计：

```mermaid
flowchart LR
    A[下载目录 / 日志目录] -->|实时监控| B[发现归档与文本日志]
    B --> C[通知与目录定位]
    C --> D[免手动解压读取]
    D --> E[行索引 + 虚拟滚动]
    E --> F[多选项卡阅读]
```

- **下载即发现**：递归监控目录，深层子目录出现新日志也能提醒。
- **归档直接阅读**：像展开文件夹一样浏览 ZIP、7z、RAR、TAR 与压缩流，不产生散乱的手工解压目录。
- **嵌套归档惰性展开**：自动识别归档中的归档，只在主动展开时读取下一层。
- **大文件也能打开**：后台建立行索引，正文只加载当前可见范围，不把 GB 级日志一次性塞进内存。
- **排查上下文不丢**：多文件选项卡分别保留滚动位置、编码和已加载内容。
- **本地优先**：日志处理在本机完成，不依赖云端上传服务。

## 功能亮点

| 能力           | 说明                                                                             |
| -------------- | -------------------------------------------------------------------------------- |
| 实时目录监控   | 同步外部创建、删除、重命名和修改；递归发现任意深度的新日志                       |
| 归档免手动解压 | 直接读取 ZIP、7z、RAR4/RAR5、TAR、tar.gz/bz2/xz/zst 和单文件压缩流             |
| 裸日志阅读     | 支持 `.log`、`.txt`、`.out`、`.err`、`.trace`、`.json`、`.csv` 等文本文件        |
| 拖入即用       | 拖入单个文件会监控其父目录；拖入文件夹会直接加入监控；文本日志同时自动打开并定位 |
| 多文件选项卡   | 重复打开自动去重；空间不足时收入“更多”菜单，并保留每个文件的阅读状态             |
| 大日志虚拟滚动 | 行偏移索引、窗口化读取、有限缓存，避免正文随文件大小线性占用内存                 |
| 编码支持       | 自动检测 UTF-8、GBK / GB18030、UTF-16LE / UTF-16BE，也可手动切换                 |
| 新日志通知     | 顶栏未读角标、逐条定位、一键全部已读                                             |
| 后缀筛选       | 自定义目录树与通知关注的文件后缀，也可临时显示全部文件                           |
| 自动更新       | 启动检查或手动检查；下载进度、签名验证、安装与重启形成完整闭环                   |
| 桌面体验       | 深浅主题、关闭到托盘、自动隐藏滚动条、可调目录栏宽度                             |
| 界面语言       | 默认跟随系统，可在设置中即时切换英文与简体中文                                   |

> LogCrate 当前是**只读查看器**。可以重命名或删除磁盘文件，但不能编辑日志正文，也不会创建、修改或重新打包归档。

## 5 分钟上手

### 1. 安装

进入 [GitHub Releases](https://github.com/Strive-Sun/LogCrate/releases/latest)，下载与你系统匹配的安装包：

- **Windows**：优先选择 `setup.exe`，也可以使用 `.msi`。
- **macOS**：下载 `.dmg` 安装包；Release 提供通用架构构建。

LogCrate 基于系统 WebView 构建。Windows 10 / 11 通常已经包含 WebView2；缺失时系统会提示安装。

### 2. 添加监控目录

首次启动后点击左侧底部的 **“+ 添加监控目录”**，选择经常接收日志的目录，例如：

- 浏览器下载目录；
- 聊天工具的文件接收目录；
- 测试设备导出目录；
- 本地服务日志目录。

LogCrate 会保存监控配置，下次启动自动恢复。监控根会按父子关系去重：监控父目录后，不会重复监控它的子目录。

### 3. 打开日志

你可以用三种方式开始阅读：

1. 在左侧目录树中点击裸日志文件；
2. 展开归档，按需逐层展开嵌套归档，再点击日志条目；
3. 从文件管理器拖入一个日志、归档或文件夹。

拖入日志文件时，LogCrate 会自动添加其父目录、展开目录树、定位文件并打开正文。拖入归档或其它文件时会加入其所在目录的监控；拖入文件夹则监控该文件夹本身。

> 当前一次只处理一个拖入路径；多文件批量拖入在路线图中。

### 4. 同时查看多个文件

继续点击其它日志即可创建选项卡：

- 点击已有文件只会激活原选项卡，不会重复打开；
- 窗口放不下时，多余选项卡进入 **“更多”** 菜单；
- 点击“更多”中的文件，会把它换入可见区；
- 鼠标悬停在选项卡名称上，可以查看完整绝对路径；
- 点击选项卡上的 `×` 释放对应查看会话。

后端最多保留有限数量的活跃会话。较久未使用的选项卡可能进入休眠，再次点击时会自动重建索引并恢复编码选择。

### 5. 处理乱码与筛选文件

- **乱码**：使用正文底部左侧的编码选择器切换 UTF-8、GBK、GB18030 或 UTF-16。
- **文件太多**：点击“监控目录”右侧的后缀筛选，只保留关心的扩展名。
- **临时找其它文件**：开启“显示全部”，当前已打开文件不会因筛选变化而消失。

### 6. 新日志到达

受监控目录出现新的受支持归档或匹配后缀的日志后，右上角铃铛会显示未读数量。点击具体通知会逐层展开目录、定位目标；点击“全部已读”只清除通知，不会删除文件。

## 常用操作速查

| 我想做什么         | 操作                    | 结果                         |
| ------------------ | ----------------------- | ---------------------------- |
| 监控新目录         | 点击“+ 添加监控目录”    | 保存并立即开始递归监控       |
| 快速查看本地日志   | 把单个日志拖入窗口      | 添加父目录、定位并打开日志   |
| 监控整个文件夹     | 把文件夹拖入窗口        | 将该文件夹加入监控根         |
| 查看归档内日志     | 按需逐层展开并点击日志条目 | 惰性打开嵌套归档，无需手工解压 |
| 区分同名文件       | 悬停选项卡              | 显示磁盘绝对路径及包内路径   |
| 切换文本编码       | 使用正文底部编码菜单    | 后台按新编码重建行索引       |
| 在资源管理器中查看 | 右键目录或文件          | 打开系统文件管理器定位路径   |
| 停止监控但保留文件 | 右键监控根 → 移除监控   | 仅取消监控，不修改磁盘内容   |
| 删除文件或目录     | 右键 → 删除并确认       | 移到系统回收站，而非永久删除 |
| 让应用后台运行     | 点击窗口右上角关闭      | 隐藏到托盘，监控继续运行     |
| 完全退出           | 托盘菜单 → 退出 LogCrate | 停止监控并结束进程           |
| 检查新版本         | 设置 → 检查更新         | 下载、验证并安装正式版本     |

## 支持范围

### 当前支持

- **系统**：64 位 Windows、Intel / Apple Silicon macOS。
- **归档**：ZIP、7z、单卷 RAR4/RAR5、TAR、tar.gz/tgz、tar.bz2/tbz/tbz2、tar.xz/txz、tar.zst/tzst。
- **单文件压缩流**：gzip、bzip2、xz、zstd 会合成一个可直接打开的日志条目。
- **嵌套归档**：任意受支持格式均可互相嵌套；只在主动展开时读取下一层，默认最多 5 层。
- **文本**：常见日志扩展名以及可被内容采样识别的文本文件。
- **编码**：UTF-8、GBK、GB18030、UTF-16LE、UTF-16BE。
- **界面语言**：英文和简体中文，支持跟随系统或持久化手动选择。

### 当前边界

- 只读预览，不修改源文件；源文件被删除后，可将已缓存的日志快照另存为本地文件。
- 一次只处理一个拖入路径。
- 不创建、修改、删除归档条目或重新打包归档。
- 能识别加密与分卷 7z/RAR，但暂不支持输入密码或读取多卷。
- 暂不支持 WIM 磁盘映像容器；WIM 需要单独评估原生依赖和跨平台打包方案。
- 自动更新需要能够访问 GitHub Release 下载地址。

## 后续开发计划

路线图表达方向，不承诺具体版本或交付日期。欢迎通过 Issue 讨论优先级。

### 近期：更快找到关键日志

- [ ] **日志等级筛选**：识别并筛选 `INFO`、`WARNING / WARN`、`ERROR`、`FATAL` 等等级。
- [ ] **日志等级标注**：为不同等级提供稳定的颜色、行标记和数量统计，快速定位错误上下文。
- [ ] **全文搜索与高亮**：支持关键字、大小写、正则表达式、结果计数和前后跳转。
- [ ] **快速时间定位**：按时间戳跳转，并支持只查看指定时间范围。
- [ ] **多文件批量拖入**：一次接收多个日志或目录，并明确展示处理结果。

### 中期：并列分析与对比

- [ ] **双日志并列显示**：左右并排打开两个日志文件，独立或同步滚动。
- [ ] **日志差异对比**：按行、时间或关键字段对齐两份日志，突出新增、缺失与变化内容。
- [x] **选项卡工作区恢复**：重启后恢复已打开文件、选项卡顺序和活动项；源文件已删除时明确提示且不再恢复。
- [ ] **书签与行标注**：收藏关键行、添加本地备注，并在文件变化后尽量恢复定位。
- [ ] **实时追踪追加内容**：为持续写入的裸日志提供类似 `tail -f` 的跟随模式。

### 长期：更多格式与更强工作流

- [ ] **结构化日志视图**：针对 JSON Lines 等格式提供字段展开、列选择和条件过滤。
- [ ] **超大日志搜索索引**：在不整文件载入内存的前提下加速重复查询。
- [ ] **导出排障片段**：按选中行或时间范围导出最小日志片段，便于提交 Issue 或分享给同事。
- [ ] **可配置规则**：保存常用等级、关键字、颜色与后缀组合，按项目快速切换。
- [ ] **AI 辅助日志分析**：针对用户明确选择的日志范围生成摘要、识别异常模式，并给出可能原因和排查步骤；支持配置本地或远程模型，任何内容发送前均明确范围并提供脱敏控制。

## 工作原理

LogCrate 使用 Tauri 2 构建，前端负责交互与虚拟列表，Rust 后端负责文件系统监听、归档读取、编码检测和行索引。

| 层                 | 职责                                               |
| ------------------ | -------------------------------------------------- |
| React + TypeScript | 目录树、通知、选项卡、设置和虚拟滚动正文           |
| Tauri IPC          | 在前端操作与本地 Rust 能力之间传递命令和进度事件   |
| Rust watcher       | 递归目录监控、事件合并、文件稳定检测和配置持久化   |
| ArchiveReader      | ZIP、7z、RAR、TAR、压缩流、嵌套归档与裸文本的有界流式读取 |
| SessionManager     | 编码检测、行偏移索引、有限会话 LRU 与临时资源清理  |

技术细节见 [技术设计文档](docs/technical-design.md)，版本变化见 [CHANGELOG.md](CHANGELOG.md)。

## 本地开发

### 环境要求

- [Node.js](https://nodejs.org/) 22 或当前 LTS；
- [Rust](https://rustup.rs/) 与 Cargo；
- Windows：Visual Studio 的“使用 C++ 的桌面开发”工作负载；
- Tauri 对应平台的系统依赖，详见 [Tauri Prerequisites](https://v2.tauri.app/start/prerequisites/)。

### 启动桌面应用

```bash
npm install
npm run tauri:dev
```

第一次会编译 Tauri 与 Rust 依赖，后续启动使用增量编译。

### 只调试前端

```bash
npm run dev
```

浏览器访问 `http://localhost:1420`。前端会自动使用内置 mock 数据，无需启动 Rust 后端。

### 质量检查

```bash
npm run format:check
npm test
npm run lint
npm run build
cargo test --manifest-path src-tauri/Cargo.toml
```

### 构建安装包

```bash
npm run tauri:build
```

产物位于 `src-tauri/target/release/bundle/`。正式版本由 GitHub Actions 在 Windows 与 macOS 上构建并签名。

## 项目结构

```text
logcrate/
├── src/                  # React + TypeScript 前端
│   ├── api/              # Tauri / mock API 适配层
│   ├── components/       # 目录树、选项卡、日志正文、设置等组件
│   └── util/             # 纯状态工具与前端单元测试
├── src-tauri/            # Rust + Tauri 后端
│   └── src/
│       ├── archive/      # 格式注册、归档读取器、嵌套流与裸文本抽象
│       ├── index.rs      # 行索引、编码、缓存与会话生命周期
│       ├── watcher.rs    # 目录监控、稳定检测与配置持久化
│       └── lib.rs        # Tauri 命令、事件与托盘生命周期
├── openspec/             # 功能规格、变更提案与归档
├── docs/                 # 技术设计和开发流程
└── .github/workflows/    # CI 与跨平台 Release
```

## 参与贡献

Issue、功能建议和 Pull Request 都很欢迎。提交问题时，建议附上：

- LogCrate 版本与操作系统；
- 日志是裸文件还是可能多层嵌套归档内的条目；
- 文件大小、编码以及可复现步骤；
- 涉及界面问题时附上截图，但请先脱敏日志内容和本地路径。

新增能力会先通过 OpenSpec 明确行为和边界，再进入实现。开发与发布约定见 [docs/dev-workflow.md](docs/dev-workflow.md)。

## 许可证

仓库目前尚未添加开源许可证。在许可证明确前，请不要假定代码可以被自由复制、修改或再分发。

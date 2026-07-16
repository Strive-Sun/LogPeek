# LogPeek

免解压、跨平台的桌面日志阅读器,专为「下载日志压缩包 → 免解压 → 直接查看」这一高频排查场景打造,基于 [Tauri](https://tauri.app/)(Rust 后端 + Web 前端)构建。

本应用支持 64 位 Windows 与 macOS 系统。

## 它解决什么问题

排查线上或客户端问题时,日志通常以压缩包形式流转(zip / tar.gz / 7z / rar)。传统流程是:下载压缩包 → 手动解压到某个目录 → 在解压结果里翻找 `.log` / `.txt` → 用文本编辑器打开。文件一多,手工解压与翻找的成本很高,且解压产物会污染磁盘。

LogPeek 把这一步压缩为「下载即看」:压缩包丢进监控目录,自动识别、顶栏提示、免解压直接查看包内日志。

## 功能特性

- **多目录监控** —— 盯住你的下载 / 备份目录,新日志包一到就发现;配置持久化,重启自动恢复。
- **免解压读取** —— 不把压缩包解压到磁盘,只读中央目录列出条目,点开某个日志才流式读取该条目。
- **目录树虚构展开** —— 压缩包在目录树里像文件夹一样展开为内部文件,展开只读清单、零落盘。
- **裸文本文件** —— 目录里的 `.log` / `.txt` 等裸文件与压缩包同等对待,直接查看。
- **扛得住大日志** —— GB 级日志通过行偏移索引 + 窗口化加载(虚拟滚动),不整包进内存。
- **顶栏新日志提示** —— 铃铛角标显示未读数,逐条递减 + 一键标记已读。
- **可配置后缀筛选** —— 自定义显示的文件后缀(如仅 `.log` / `.txt`),即时过滤。
- **编码自适应** —— 自动探测并解码 UTF-8 / GBK / UTF-16(含 BOM),中文日志不乱码。
- **深浅色主题** —— 默认跟随系统,可手动切换,CSS 变量驱动即时生效。

## 界面

三栏式布局:左栏监控目录树、中栏条目列表、右栏日志正文(虚拟滚动)。顶栏含主题切换、新日志铃铛、后缀筛选。详见 [技术设计文档 · UI 设计](docs/technical-design.md#6-ui-设计)。

## 安装使用(普通用户)

从 Release 下载对应安装包,双击安装即可:

- **Windows**:`LogPeek_x.y.z_x64-setup.exe`(NSIS,推荐)或 `LogPeek_x.y.z_x64_en-US.msi`
- **macOS**:`LogPeek_x.y.z_x64.dmg`

安装包约 2–3MB(复用系统 WebView,不打包 Chromium)。Windows 首次运行若缺少 WebView2,会自动联网安装;Win10 1803+ / Win11 一般已预装。

## 依赖项(开发)

### Node.js

用于安装前端依赖与构建。推荐最新 LTS 版本。

https://nodejs.org

### Rust

Tauri 后端使用 Rust 编写,需要 Rust 工具链(含 Cargo)。

https://rustup.rs

Windows 还需 MSVC C++ 生成工具(通常随 Visual Studio「使用 C++ 的桌面开发」工作负载安装)。

### Bash

部分脚本需要类 bash 环境。Windows 上推荐使用 Git Bash(随 Git for Windows 一起安装);macOS 默认 shell 即可。

## 开发

安装依赖:

```bash
npm install
```

启动开发模式(自动起前端 dev server + 编译 Rust + 打开桌面窗口,支持热更新):

```bash
npm run tauri:dev
```

> 首次运行会拉取并编译整个 Tauri 依赖树,耗时较长;后续为增量编译,秒级启动。

仅调试前端 UI(浏览器打开,后端走内置 mock 数据,无需 Rust):

```bash
npm run dev        # 打开 http://localhost:1420
```

前端通过 API 抽象层自动切换数据源:在 Tauri 中调用真实后端命令,在浏览器中回退到 mock,组件代码无需改动。

## 构建

产出安装包与免安装可执行文件:

```bash
npm run tauri:build
```

产物位于 `src-tauri/target/release/bundle/`(安装包)与 `src-tauri/target/release/`(可执行文件)。

## 项目结构

```
logpeek/
├── src/                  # 前端 (React 18 + TypeScript)
│   ├── api/              #   API 抽象层:tauri.ts(真实) / mock.ts(浏览器)
│   ├── components/       #   三栏、目录树、日志正文、顶栏等组件
│   └── util/
├── src-tauri/            # 后端 (Rust)
│   └── src/
│       ├── archive/      #   归档读取:ArchiveReader trait + zip + 裸文本 passthrough
│       ├── index.rs      #   行偏移索引 / 窗口化加载 / 编码解码 / 会话生命周期
│       ├── watcher.rs    #   目录监控 / 大小稳定检测 / 配置持久化
│       └── lib.rs        #   Tauri 命令与事件注册
├── docs/                 # 技术设计文档
└── openspec/             # OpenSpec 规格与变更提案
```

## 技术栈

- **框架**:Tauri 2.x — 复用系统 WebView,安装包体积与内存显著小于 Electron。
- **前端**:React 18 + TypeScript + Vite,虚拟滚动用 `@tanstack/react-virtual`,CSS 变量主题。
- **后端**:Rust。zip 读取用 `zip`,目录监控用 `notify`,编码用 `encoding_rs`,配置持久化用 `serde` / `serde_json`。

技术选型与架构详见 [docs/technical-design.md](docs/technical-design.md)。

## 路线图

- **M1(当前)** —— 最小闭环:多目录监控、zip 免解压 + 裸文本、行索引窗口化查看、编码自适应、主题、后缀筛选。
- **M2** —— Deflate 重启点索引(免磁盘放大的随机访问)、采样索引优化、「已看」状态持久化。
- **M3** —— 多格式:tar.gz、7z。
- **M4** —— 搜索 / 过滤 / 高亮、主题配色打磨、rar(视授权)。

## 许可

待定(计划采用开源许可)。


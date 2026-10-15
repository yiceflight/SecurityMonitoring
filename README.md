# 安全监控

仅面向 Windows 的电脑使用记录与审计客户端。采用 Tauri 2、React、TypeScript 和 Rust，不提供 macOS 客户端支持。

## 当前进度

已实现 Windows 原生截屏、Rust 后台定时调度、托盘菜单、当前用户登录后自动启动、截图与使用记录保存和 Windows 事件日志。主页面展示历史截图与底部时间轴，监控状态常驻右上角。无需选择日期或输入时间，直接拖动时间轴查看本机保留的历史截图。首次运行默认停止监控，在设置弹窗中点击“开始监控”才会开始定时采集。

默认每 60 秒截屏，保留最近 30 个自然日。监控会将所有显示器的虚拟桌面合成一张 JPEG，同时保存截图时间、尺寸、前台程序名、窗口标题和键鼠空闲秒数。状态包括正在监控、已停止、等待解锁和采集失败；失败后按设定间隔重试。锁屏、UAC 安全桌面或断开用户会话时跳过截图，恢复交互桌面后自动继续。截图空间设置不设固定上限；记录所在磁盘剩余空间降至 100 MB 或更少时，优先删除最早的截图后继续采集。

关闭窗口只隐藏到系统托盘，Rust 调度继续运行。单击或双击托盘图标可恢复面板（包括已最小化的窗口），右键菜单可打开面板、开始/停止监控、打开记录目录或退出。再次运行只唤起已有客户端。退出客户端会结束采集；下次启动恢复用户最后一次明确选择的启停状态。

本轮已通过前端构建、核心逻辑测试和完整 Windows 目标编译检查。真实 Windows 上的截图、托盘、登录启动、事件查看器和安装包验收仍须按 [Windows 验收说明](docs/windows-monitoring-acceptance.md) 执行。

## 使用方式

1. 在 Windows 中运行客户端，点击左上角 logo 10 次后输入固定密码，通过验证后打开“监控设置”弹窗；每次重新打开都需验证。所有监控设置仅在弹窗内显示，可调整截屏间隔（至少 1 秒的整数，无业务上限）、截图保留天数（1–365 天）和截图空间设置（至少 100 MB，无固定上限），点击“保存设置”。缩短保留时间后会立即清理过期记录；磁盘剩余空间不足时会自动删除最早截图。
2. 按需要勾选“登录 Windows 后自动启动”。这会注册当前用户的登录启动项，并在登录时隐藏到托盘；不要求日常运行时使用管理员权限。
3. 首次使用或从旧版本升级后，点击“注册日志源”，完成系统管理员授权。固定的内置 PowerShell 指令只注册 `SecurityMonitoring.System` 到 System（系统）日志并写入验证事件。取消授权或注册失败会显示错误。
4. 在设置弹窗中点击“开始监控”，首张截图立即采集，随后按间隔采集。点击“停止监控”后停止自动采集；界面确认停止后不会再有后台截图。
5. 右上角每 3 秒刷新实际监控状态；停止、等待解锁、异常和读取失败均如实显示。主页面默认展示保留记录中的最新截图，拖动底部时间轴查看历史画面；点击“刷新记录”重新读取历史并显示最新一张。需要维护时，可在设置弹窗中点击“打开记录目录”查看本地文件。

Windows 事件日志记录启动、退出、开始/停止、截图结果、设置变化和保留清理，写入 System（系统）日志，来源为 `SecurityMonitoring.System`、事件 ID 为 `1000`。每次截图成功保存后立即写入一条“截图：图片完整路径”，没有独立日志间隔或日志开关。过期图片删除成功后逐张写入“截图已过期，自动删除：图片完整路径”；截图空间设置或磁盘剩余空间清理也逐张写入删除原因、路径和空间信息。失败使用 Error 等级。所有运行日志仅写入 Windows 事件日志，不再创建或追加本地审计文件。Windows 日志源未注册或服务不可写时，设置弹窗提示实际错误，截图仍保存。[Windows 事件源说明](https://learn.microsoft.com/en-us/windows/win32/eventlog/event-sources)、[New-EventLog 官方说明](https://learn.microsoft.com/en-us/powershell/module/microsoft.powershell.management/new-eventlog?view=powershell-5.1)。

## 时间轴回放

首页只保留截图区域和底部时间轴，不提供日期、起止时间或定位输入框，也不显示电脑使用规范。时间轴覆盖本机当前保留的全部记录，跨日期按真实采集时间排列，初次打开显示最新截图。拖动时显示最近一张实际截图及其日期、时间、前台程序和窗口标题；拖到空白处会提示“该时刻无截图，显示最近记录”，不把间隔补成连续录像。

时间轴获得焦点后可用左右方向键逐张切换，Home / End 跳至首张 / 末张。“放大查看”支持适应窗口或原始尺寸查看。图片逐张读取，快速拖动合并读取请求；读取失败、截图被清理、无记录分别提示，切换时不会把上一张截图当成当前画面。新增记录通过“刷新记录”载入，监控停止后仍可回看保留记录。

## 本地存储

数据根目录为 Tauri 的当前用户应用数据目录（Windows 通常为 `%APPDATA%\\com.securitymonitoring.app`），设置弹窗显示实际路径。配置使用原子替换，记录使用按日期分组的 JSONL 并同步落盘；启动时处理因崩溃中断的末尾记录，不将未写完的数据算作成功。数据仅在本机保存。

```text
settings.json                   监控与启动设置
records/YYYY-MM-DD/
  YYYY-MM-DD-<时间编号>.jpg       桌面截图
  captures.jsonl                截图与前台程序元数据
```

旧配置中的 `eventLog` 和 `logIntervalSeconds` 会被忽略，现有截图及启停设置保持有效；下次保存设置时移除这两个旧字段。已有本地审计文件不再读取或追加，随对应过期日期目录正常清理。`captures.jsonl` 仅作为截图元数据索引保留。

保留清理只处理本应用 `records` 下过期的日期目录，不清理 Windows 事件查看器中的记录。Windows 事件日志的保留由系统管理。查询日期和图片编号在后端验证；图片预览只可读取受限的截图路径。

## 开发环境

- Node.js 24 LTS、pnpm 11.9.0。
- Windows 开发机：Rust stable（MSVC 工具链）、MSVC C++ Build Tools、Windows SDK 和 WebView2。

安装依赖：

```sh
pnpm install
```

在 Windows 上启动原生桌面客户端（同时启动前端服务）：

```sh
npm run dev:client
# 也可使用 pnpm desktop:dev 或 pnpm tauri dev
```

仅启动浏览器开发预览（可在其他系统上查看界面，不代表支持该系统的原生客户端）：

```sh
pnpm dev
```

两种启动方式共用同一开发端口，择一运行。若前端已单独启动，可使用 `pnpm tauri dev --config '{"build":{"beforeDevCommand":""}}'` 连接现有服务，避免再次启动前端。浏览器预览不支持 Tauri 原生调用。

## 独立端口

| 用途 | 地址 | 配置位置 |
| --- | --- | --- |
| 前端开发 / Tauri devUrl | http://127.0.0.1:19427 | vite.config.ts、src-tauri/tauri.conf.json |
| HMR WebSocket | ws://127.0.0.1:19427 | 与开发服务共用端口 |
| 构建产物预览 | http://127.0.0.1:19428 | vite.config.ts |

开发和预览均启用 `strictPort: true`，端口被占用时直接报错，不自动换端口，也不终止其他项目进程。修改开发端口时须同步修改 Vite 的 server/HMR、Tauri 的 devUrl 和 devCsp。正式安装的客户端加载本地打包资源，不依赖这些开发端口。

## 检查与构建

```sh
pnpm typecheck
pnpm test
pnpm build
cargo test --manifest-path src-tauri/monitor-core/Cargo.toml
# 以下原生检查与打包命令在 Windows 上运行
cargo check --manifest-path src-tauri/Cargo.toml
pnpm desktop:build
# 预览已构建的前端
pnpm preview
```

`pnpm build` 只构建前端；`pnpm desktop:build` 在 Windows 开发机或 Windows CI 上构建原生应用及 NSIS（.exe）和 MSI（.msi）安装包。安装、自启动、截屏、事件日志、锁屏恢复和多显示器行为必须在 Windows 上验收。

## GitHub 自动构建与下载

推送到 `main` 分支后，[Windows Release 工作流](.github/workflows/windows-release.yml) 会在 Windows runner 上运行前端测试、Rust 核心测试，并构建 Windows x64 的 NSIS（`.exe`）和 MSI（`.msi`）安装包。前端类型检查随 Tauri 的构建命令执行。

两个安装包上传并确认完整后，工作流会自动发布 GitHub Release，标记为 Latest。前往 [最新版本下载](../../releases/latest)，在 Assets 中选择 `-setup.exe` 或 `.msi`；[历史版本](../../releases) 会保留。构建产物也会保存到对应 Actions 运行的 Artifacts。

Release 标签格式为 `v<应用版本>-build.<运行编号>.<重试编号>`（例如 `v0.1.0-build.12.1`），无需每次推送都修改应用版本号；重新运行会创建新的 Release。安装包内部版本仍使用 `src-tauri/tauri.conf.json` 中的版本，正式升级版本时应同步修改 `package.json`、`src-tauri/Cargo.toml` 及相关锁文件。构建或上传失败时不会公开发布不完整的 Release，可能留下草稿供排查。

也可以在 Actions → Windows Release → Run workflow 中选择 `main` 手动运行。工作流使用 GitHub 自动提供的 `GITHUB_TOKEN` 和 `contents: write` 权限，无需配置个人访问令牌；仓库或组织策略须允许 Actions 创建 Release。当前未配置 Windows 代码签名证书，下载的安装包为未签名版本。

工作流使用 [Tauri 官方 Action](https://github.com/tauri-apps/tauri-action)。首次推送时须一并提交应用源码、`pnpm-lock.yaml`、两个 Rust `Cargo.lock` 和打包图标，不能只提交工作流文件。

## 目录

```text
src/                       React 界面
public/                    静态资源
src-tauri/src/             Tauri 命令、窗口与托盘
src-tauri/monitor-core/    Windows API、调度、配置、存储与核心测试
src-tauri/capabilities/     Tauri 权限
src-tauri/tauri.conf.json   应用、窗口与打包配置
vite.config.ts             前端开发与预览端口
```

截图与托盘在已登录用户的进程中运行。本实现没有安装 Windows 系统服务，未登录、休眠、关机时无法截图。其他系统可运行前端与核心测试，原生监控返回“不支持”；不维护 macOS 采集实现。开发客户端也保留关闭收起与托盘恢复行为，macOS 上可点击 Dock 图标恢复隐藏窗口；这不代表支持 macOS 监控采集。

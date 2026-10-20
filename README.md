<p align="center"><img src="assets/branding/rustgit-icon.png" width="128" alt="RustGit"></p>

# RustGit

[English](README.en.md) · **简体中文**

**给项目存版本，让多台电脑共享同一份工作。**

RustGit 提供 macOS 和 Windows 桌面应用。你可以只在本机保存历史，也可以让一台电脑或服务器托管项目，其他设备连接后一起工作。

## 为什么用 RustGit

- **自己掌握项目数据。** 托管位置由你决定：自己的电脑、局域网主机，或可连接的服务器。
- **一次完成提交和共享。** 选择文件、填写说明、点击「提交并共享」，本地保存后自动发送。发送失败可重试，已保存的版本不会丢失。
- **减少重复保存和传输。** 引擎对文件分块、去重和压缩，共享时复用已存在的内容、传输缺少的数据。实际收益取决于文件类型和修改方式。
- **把不该同步的文件留在本机。** 自动读取 `.gitignore` 和嵌套规则，也支持 `.rgitignore`，方便排除依赖、缓存和构建目录。
- **跨平台协作。** macOS、Windows 使用同一套引擎和仓库格式，界面支持中文、英文。
- **按设备控制访问。** 加密连接、邀请加入、读写权限，以及撤销设备授权。

## 下载与安装

| 系统 | 安装包 |
| --- | --- |
| macOS · Apple Silicon（M 系列，macOS 14+） | [下载 DMG](https://github.com/xiezhengyi1/Rustgit/releases/download/v0.1.0/RustGit-0.1.0-macos-arm64.dmg) · [ZIP](https://github.com/xiezhengyi1/Rustgit/releases/download/v0.1.0/RustGit-0.1.0-macos-arm64.zip) |
| Windows · x64（Windows 10 1809+ / Windows 11） | [下载 EXE 安装程序](https://github.com/xiezhengyi1/Rustgit/releases/download/v0.1.0/RustGit-0.1.0-windows-x64-setup.exe) |

[全部版本与校验文件](https://github.com/xiezhengyi1/Rustgit/releases/tag/v0.1.0)

macOS 打开 DMG，将 RustGit 拖进「应用程序」；Windows 运行 EXE 安装。当前 macOS 包未经 Apple 公证，Windows 包未签名，系统可能要求确认来源。Windows 缺少 WebView2 时，安装器会联网下载。

## 怎么用

### 1. 给本机项目保存第一个版本

1. 打开 RustGit，在「项目」中新建项目，或打开已有文件夹并初始化。
2. **先检查 `.gitignore`**，排除 `node_modules/`、构建目录等不需要共享的内容。
3. 进入「更改」，勾选要保存的文件，填写版本说明。
4. 点击 **「提交版本」**，完成本地保存。

以后修改文件后，重复选择和提交即可。在「历史与分支」中查看差异、创建分支或恢复文件。最近打开的项目会保留在左侧，点击即可切换。

### 2. 让另一台电脑加入

**先在托管电脑操作：**

1. 打开已提交过版本的项目，进入「共享」，选择 **托管此项目**。
2. 开启局域网访问，让其他电脑能够连接。
3. 创建邀请：只需要下载就给读取权限，需要协作修改就给读写权限。
4. 将邀请链接私下发给另一台电脑。

**再在另一台电脑操作：**

1. 打开「项目」，使用邀请链接加入项目。
2. 选择保存文件夹，等待下载完成。
3. 打开文件夹，确认项目文件已下载。

邀请通常只能使用一次；每台新设备需要各自的邀请。托管电脑需要保持唤醒，并让 RustGit 持续运行。

### 3. 日常修改、发送和接收

| 你要做什么 | 在哪里操作 |
| --- | --- |
| 把自己的修改给其他设备 | 「更改」→ 选择文件 → 填写说明 → **提交并共享** |
| 接收别人的新版本 | 「共享」→ **拉取并合并** |
| 发送失败后再试 | **重试共享**，不用重复提交 |
| 两边都改了同一处 | 先拉取，处理冲突并提交，然后共享 |
| 找回旧文件 | 「历史与分支」→ 选中版本 → 恢复相应文件 |

「共享成功」表示托管端已收到版本；其他电脑仍需拉取，文件才会更新。

<!-- operation-benchmark:start -->
## 与 Git 的操作速度对比

以下是 **2026-10-20、Apple M3 / 8 GB / macOS 27.0.1** 上的 CLI 实测。使用相同的合成源码数据（起始 301 个文件、2.14 MiB），每个工具从全新仓库重复 5 轮，表中为耗时中位数；**越低越快**。Git 提交包含 `add + commit`。

| 操作 | Git（ms） | RustGit（ms） | Git / RustGit |
| --- | ---: | ---: | ---: |
| 初始化 | 12.9 | 13.4 | 0.96× |
| 首次提交 | 167.6 | 24.0 | 6.98× |
| 检查状态：无修改 | 8.85 | 7.46 | 1.19× |
| 检查状态：有修改 | 9.50 | 7.39 | 1.28× |
| 查看工作区差异 | 8.85 | 8.17 | 1.08× |
| 历史（11 条） | 8.02 | 5.25 | 1.53× |
| 查看单个提交 | 8.74 | 6.30 | 1.39× |
| 增量提交 | 22.1 | 12.3 | 1.79× |
| 创建分支 | 7.99 | 5.46 | 1.46× |
| 切到初始版本 | 11.1 | 8.08 | 1.37× |
| 切回最新分支 | 11.7 | 9.26 | 1.27× |
| 恢复单个文件 | 8.16 | 7.07 | 1.15× |
| 暂存未提交工作 | 24.0 | 16.0 | 1.50× |
| 恢复暂存工作 | 11.0 | 9.64 | 1.14× |
| 撤销提交 | 13.2 | 14.8 | 0.89× |
| 合并分支（无冲突） | 16.9 | 14.0 | 1.21× |
| 首次发送 | 119.7 | 1648.5 | 0.07× |
| 克隆并落盘文件 | 69.7 | 343.9 | 0.20× |
| 增量发送 | 44.9 | 55.5 | 0.81× |
| 获取新提交 | 40.4 | 24.0 | 1.69× |
| 拉取并合并新提交 | 67.0 | 25.2 | 2.66× |
| 发送：已经最新 | 15.8 | 10.7 | 1.47× |
| 获取：已经最新 | 24.1 | 7.78 | 3.10× |
| 拉取：已经最新 | 32.8 | 7.41 | 4.42× |
| 完整性检查 | 49.1 | 14.9 | 3.29× |
| 常规整理（GC） | 186.4 | 20.2 | 9.23× |

`Git / RustGit` 大于 1 表示 RustGit 在此项较快，小于 1 表示 Git 较快。差异很小的项目可能处于运行波动范围内。RustGit 并非每项都更快。

**数据类型会改变结果：** 64 MiB 大文件组的完整克隆为 Git 6.44 秒 / RustGit 0.69 秒；5,001 个小文件组的首次发送为 Git 0.55 秒 / RustGit 20.32 秒。前者包含局部修改的大文件历史，不能推广到所有大文件；后者暴露了 RustGit 当前的小文件首次传输开销。

同步都走本机 TCP：Git 使用未加密的 `git://`，RustGit 使用身份验证和加密连接；不包含首次邀请与设备登记。完整性检查和 GC 的实现与保障范围不同，这里只比较命令耗时。结果包含进程启动开销，**不是 Windows、真实局域网或公网速度**。另测了 5,001 个小文件和 64 MiB 大文件，完整结果、每轮原始数据及复现命令见 [速度基准](docs/performance.md)。
<!-- operation-benchmark:end -->

## 可以用服务器托管吗？

**可以。服务器运行 `rgit serve`，桌面端通过邀请连接。** 服务器不需要桌面界面，也不需要一直开着你的个人电脑。适合在常开主机、Linux 服务器或具备运行条件的 NAS 上集中保存共享历史。

服务器保存的是 RustGit 仓库，提供版本同步；目前不提供类似 GitHub 的网页代码浏览和账号管理后台。一个 `serve` 进程托管一个项目，多个项目使用不同目录和端口。

### 在 Linux 服务器上启动

安装 Rust 1.89+ 和 C 编译工具后，在服务器上执行：

```bash
git clone https://github.com/xiezhengyi1/Rustgit.git
cd Rustgit
cargo build --release -p rustgit-cli
mkdir -p "$HOME/.local/bin" "$HOME/rustgit-repos"
install -m 755 target/release/rgit "$HOME/.local/bin/rgit"

# 保持相同的配置目录，服务与管理命令都使用这个目录
export RGIT_HOME="$HOME/.local/share/rustgit-server"
"$HOME/.local/bin/rgit" --insecure-file-keys serve \
  --init "$HOME/rustgit-repos/myproject.rgit" --listen 0.0.0.0:7878
```

Linux 当前需要显式使用 `--insecure-file-keys`：私钥以仅所属用户可读的文件保存，未加密；**网络连接仍然经过身份验证和加密**。请使用专用系统用户，并保护配置目录及备份。macOS 和 Windows 默认使用系统钥匙串或凭据管理器。Linux 部署步骤基于 CLI 实现，当前发布未提供 Linux 预编译包，也未完成本次 Linux 原生部署验证。

### 创建邀请，把本机项目上传

保持上面的服务运行，在服务器的另一个终端执行，将 `YOUR_SERVER_IP` 换成客户端可以访问的 IP 或域名：

```bash
export RGIT_HOME="$HOME/.local/share/rustgit-server"
"$HOME/.local/bin/rgit" --insecure-file-keys \
  -C "$HOME/rustgit-repos/myproject.rgit" invite create \
  --role write --expires 24h --hint tcp://YOUR_SERVER_IP:7878
```

需要允许客户端访问服务器的 TCP 7878 端口；使用云服务器时，同时检查云安全组和系统防火墙。不要把 `0.0.0.0` 或 `127.0.0.1` 当成其他设备的连接地址。

**上传已有 RustGit 项目：** 在本机项目目录中，用邀请绑定服务器，再发送已有提交：

```bash
rgit join 'projectapp://join/这里替换成完整邀请内容' --add-remote
rgit push
```

之后用桌面应用打开这个项目，就可以日常点击「提交并共享」。若项目已有其他 `origin`，请先确认连接目标；也可以用 `--remote server` 添加不同名称的连接。

**让其他设备下载：** 在服务器为每台设备另建邀请，然后在桌面应用中加入，或运行：

```bash
rgit join 'projectapp://join/这里替换成另一份完整邀请内容' myproject
```

新建的服务器仓库是空的，需要先上传第一个版本，再让其他设备下载；否则工作文件夹中可能只有 `.rgit`。

常驻运行、自动重启、备份、SSH 连接和公网中继见 **[服务器托管指南](docs/server.md)**。

## 常见问题

**能直接连接 GitHub 或已有 Git 仓库吗？** 目前不能。RustGit 使用独立的 `.rgit` 格式，不兼容 Git 仓库协议。这个 GitHub 仓库用于发布软件源码和安装包。

**为什么加了 `.gitignore` 仍会同步？** 忽略规则作用于未跟踪文件；已经提交过的文件仍受版本管理。应在第一次提交前设置规则。`.git/`、`.rgit/` 始终排除，全局 Git 忽略规则不读取。

**Windows 和 Mac 共享时有什么要注意？** Windows 当前不支持恢复符号链接。应排除依赖缓存、构建目录和 macOS 应用包，避免把平台产物作为项目源码共享。

**现在能用到什么程度？** 当前是 `v0.1.0` 早期版本。macOS 应用已运行验证；Windows 安装替换、UI「提交并共享」和第二副本拉取文件并校验哈希已通过实测。重要项目请保留独立备份。完整验证范围见 [实现状态](docs/status.md)。

## 开发文档

[CLI 使用](docs/cli.md) · [服务器托管](docs/server.md) · [macOS 构建](docs/macos-app.md) · [Windows 构建](docs/windows.md) · [存储与恢复](docs/format.md) · [存储基准](docs/benchmarks.md) · [设计决策](docs/decisions.md)

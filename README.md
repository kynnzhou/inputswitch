# InputSwitch

[![CI](https://github.com/kynnzhou/inputswitch/actions/workflows/ci.yml/badge.svg)](https://github.com/kynnzhou/inputswitch/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/kynnzhou/inputswitch)](https://github.com/kynnzhou/inputswitch/releases/latest)
[![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Windows-blue)]()
[![License: MIT](https://img.shields.io/badge/License-MIT-green.svg)](LICENSE)

一台显示器接两台主机，**点一下或按一个热键**就把屏幕切过去，不用伸手去按显示器上的按钮。两台都装上，还能**共用一套键盘鼠标**、互传文件、同步剪贴板。

原生二进制（macOS / Windows），目标机器不需要装 Python、Node 或任何运行时。

```
键盘鼠标 ──┐
           ├── 主机 A（键鼠插在这台）
显示器 ────┤
           └── 主机 B（不接键鼠）
```

## 功能

| | |
|---|---|
| 🖥 **一键切屏** | 热键 / 托盘菜单 / 控制面板任一处点击，通过 DDC/CI 切换显示器输入源；每个口的取值**本机实测验证**，不依赖厂商标准值 |
| ⌨️ **共用键鼠** | 软件级 KVM：「跟随画面」切屏即交键鼠；「多屏共享」光标推到屏幕边缘就过去；最多四台对端十字拓扑，会话中可无断换台 |
| 🔐 **配对加密** | 两台填同一个配对码：局域网消息全部 HMAC-SHA256 签名 + 时间戳防重放，键鼠事件流 ChaCha20-Poly1305 加密 |
| 📁 **文件传输** | 支持多文件与整个目录，SHA-256 逐文件校验、重名不覆盖；复制文件可在对面按 Ctrl+V **按需传输**（复制时一个字节不发） |
| 📋 **剪贴板同步** | 文本自动同步，且只发给"现在握着键鼠"的那台，多机互连时不互相踩 |
| 🪟 **控制面板** | 用系统自带 WebView 的原生窗口（macOS WKWebView / Windows WebView2），3×3 拖拽摆位图配置键鼠拓扑，开不出来自动退回浏览器 |
| 🛡 **自启与守护** | 开机自启、崩溃自动重启（macOS LaunchAgent / Windows 计划任务）；「副机模式」一键给没接键鼠的那台配齐 |
| 🔌 **双机中继** | 本机 DDC 不通时请对端代切——只要任意一台能用，双向都能切 |

## 下载安装

到 [Releases](https://github.com/kynnzhou/inputswitch/releases/latest) 下载最新版。

**macOS（Apple Silicon）**：解压后把 `InputSwitch.app` 拖进「应用程序」，双击运行，菜单栏出现 **is** 图标。首次使用需在「设置 → 隐私与安全性」中勾选 InputSwitch 的**辅助功能**与**输入监控**——先勾选、再启动。

**Windows（x64）**：解压后双击 `InputSwitch.exe`（`WebView2Loader.dll` 必须和 exe 同目录；Win10 1803+ / Win11 自带 WebView2 运行时）。无需任何系统授权。

## 快速上手（三步）

1. **打开程序**——两台机器都装上并运行
2. **设热键目标**——点托盘图标 → `打开控制面板…` → 在「这台显示器」的输入口列表里找到**另一台电脑**接着的那个口 → `设为热键`
3. **按热键**——默认 `Ctrl+Alt+S`（Mac 上是 `Control+Option+S`），屏幕立刻切到对面

不设热键也能用：托盘菜单、面板主按钮、每个口旁边的「切换」都可以点。切屏的同时键鼠会跟着画面走（配了共用键鼠时），切回来时自动交还。

## 它是怎么工作的

显示器一旦切到另一个输入口，本机的 DDC 通道就断了——**只有当前正在显示的那台机器能发指令**，所以"把屏幕拿回来"必须由对面那台主动推。InputSwitch 用局域网把两台机器连起来：切屏走本机 DDC，画面在对面时由握着画面的那台代切。

输入源取值由每台显示器的固件决定、普遍不遵守 MCCS 标准（尤其 USB-C）。程序以「显示器从拓扑中消失」为可靠信号实测验证每个口，按显示器型号（EDID）存档，一台试出的结果另一台直接复用。

> 深入了解：[完整手册与设计文档](docs/manual.md)——两种键鼠模式的物理前提、权限、扫描与探测的取舍、三平台 DDC 差异、为什么这么设计。

## 常见问题

**点了没反应 / 切不动？** 按命中概率排查：① 显示器 OSD 里把 DDC/CI 打开（很多机器出厂是关的）；② 别经过扩展坞 / 转接头，用线直连；③ 有的显示器只在 DP/HDMI 口转发 DDC，USB-C 不转发；④ 虚拟机、远程桌面里拿不到 DDC。诊断：`inputswitch-cli selftest`。

**Windows 切走后桌面图标重排了？** 切换输入源等同于"拔掉显示器"，Windows 的显示拓扑跟着物理连接走，窗口会挤到主屏。这是纯软件方案的天花板，根治只能上带 EDID 模拟的硬件 KVM。

**macOS 重装/升级后键鼠抓不住了？** 本程序是 ad-hoc 签名，每次重新编译授权都会失效：去设置里把 InputSwitch 取消勾选再重新勾上，然后**重启程序**（先勾再启动）。

## 命令行

包里附带 `inputswitch-cli`（macOS 在 `.app/Contents/MacOS/`，Windows 在 exe 旁边），零依赖可单文件分发：

| 命令 | 作用 | 会改屏幕吗 |
|---|---|---|
| `probe` / `selftest` | 列出显示器与 DDC 能力 / 诊断 + 排查建议 | 否 |
| `status` | 当前输入源 + 局域网对端 | 否 |
| `switch <口>` | 切换输入源（`--relay` 本机失败请对端代切） | **是** |
| `wizard --own X --other Y` | 逐个试值，找出真实取值 | **是** |
| `scan` | 扫描每个口上有没有主机 | **是** |
| `send <路径…>` | 直接给对端发文件 | 否 |
| `pair [码]` / `clip [文本]` | 配对码 / 剪贴板读写 | 否 |
| `kvm --peer <IP>` | 不经托盘直接跑共用键鼠 | 按热键时 |

## 从源码构建

```bash
cargo build --release --bins   # macOS / Linux
./build.sh                     # macOS：打包 .app、签名、安装到 /Applications
./cross-build-windows.sh       # 在 macOS 上交叉编译 Windows 版（Zig 工具链）
```

参与开发请看 [CONTRIBUTING.md](CONTRIBUTING.md)（构建/测试命令、界面铁律、发版流程）。

## 许可

[MIT](LICENSE)。界面与图标中的字体轮廓来自 [Nunito](design/brand/licenses/OFL-Nunito.txt)（SIL OFL 1.1）。共用键鼠的实现参照了 [lan-mouse](https://github.com/feschber/lan-mouse) 与 [Deskflow](https://github.com/deskflow/deskflow) 两个 GPL 项目的公开设计，代码全部原创。

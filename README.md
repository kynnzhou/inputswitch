# InputSwitch

一台显示器接两台主机，**点一下或按一个热键**就把屏幕切过去，不用伸手去按显示器上的按钮。

Windows / macOS 双端，原生二进制，目标机器不需要装 Python、Node 或任何运行时。

---

## 怎么用（三步，不用开终端）

**1. 打开程序**

- macOS：把 `InputSwitch.app` 拖进「应用程序」，双击
- Windows：双击 `InputSwitch.exe`

菜单栏（macOS）/ 系统托盘（Windows）出现一个 ⇄ 图标。

**2. 设一下热键目标**

点图标 → `打开控制面板…` → 「输入源」里找到**另一台电脑**接着的那个口 → `设为目标`。

> 例：显示器 DP 口接 Windows、USB-C 口接 Mac。
> 在 Mac 上就选 `DP-1`；在 Windows 上就选 `USB-C`。
> 含义是"按热键时，把屏幕推给对面那台"。

**3. 按热键**

默认 `Ctrl+Alt+S`（Mac 上是 `Control+Option+S`）。屏幕立刻切到对面。

菜单里也有 `切换输入源`，点哪个口就切哪个口，不用热键也能用。

**开机自启**：面板「设置」里点一下，macOS 写 LaunchAgent，Windows 写注册表 Run 键。

## 为什么是"推给对面"而不是"拉回自己"

这是 DDC/CI 的物理约束，不是设计选择：

> 显示器一旦切到另一个输入口，本机的 DDC 通道就断了。

所以**只有当前正在显示的那台机器能发指令**。你在 Windows 上想把屏幕拿回来是做不到的 ——
必须由 Mac 主动"推"过去。这也是为什么每台机器只需要配一个目标口。

补充一个实测到的细节：屏幕被切走后，那台机器的系统层面会认为**显示器被拔了**，
`EnumDisplayMonitors` 直接返回 0 台，连物理显示器句柄都拿不到。
所以面板的「输入源」在这种机器上是空的，并明确写"本机现在没有接通的显示器"——
这不是读取失败，是真的没有显示器可读。

这种情况下主按钮会变成 `请 <对端> 把画面切回本机（0x…）`：本机自己发不出 DDC，
但可以让握着画面的那台替你切（走局域网命令通道）。前提是本机成功读到过一次
自己接在哪个口 —— 接上屏幕启动一次就会记住并落盘。

## 第一次用如果切不动

按顺序排查，命中率从高到低：

1. **显示器 OSD 里把 DDC/CI 打开** —— 很多机器出厂是关的。INNOCN 在「其他设置」里。
2. **别经过扩展坞 / 转接头**，用线直连。DDC 在多数 hub 上会被吃掉。
3. 有的显示器只在 DP/HDMI 口转发 DDC，USB-C 口不转发。
4. 虚拟机、远程桌面里拿不到 DDC。

诊断命令（macOS 在 `.app/Contents/MacOS/inputswitch-cli`，Windows 是旁边的 `inputswitch-cli.exe`）：

```bash
inputswitch-cli probe       # 列出显示器、DDC 能力、当前输入源
inputswitch-cli selftest    # 同上，外加排查建议和退出码
inputswitch-cli status      # 当前状态 + 局域网里发现的对端
```

`✓ 可读可写` 最好；`△ 仅可写` 也能用（切换靠"显示器从拓扑里消失"来确认）；
`✗ 不可用` 就是那个口根本不转发 DDC。

## 如果标准值不对

MCCS 标准里 `0x60` 的取值是 15=DP、17=HDMI、27=USB-C，但**相当多显示器不遵守**，
尤其 USB-C 口基本是厂商私有码。

菜单里的口名后面标着实际会写下去的值（如 `DP-1 (0x0F)`）。如果点了没反应：

```bash
inputswitch-cli wizard --own usbc --other dp1
```

它会逐个遍历候选值，哪个让显示器从本机拓扑里消失，哪个就是那个口的真实值，并写进配置。

> ⚠️ 命中瞬间本机屏幕就黑了，恢复要靠对面那台按热键，或显示器 OSD 手动切回。

试出来后配置会按**显示器型号**存下来。因为取值由显示器固件决定，跟主机无关 ——
一台机器上试出来的结果，另一台拷过去直接能用。

## 配置文件

- macOS：`~/Library/Application Support/inputswitch/config.json`
- Windows：`%APPDATA%\inputswitch\config.json`

```json
{
  "hotkey": "ctrl+alt+s",
  "hotkey_target": "dp1",
  "labels": { "usbc": "Mac", "dp1": "Windows" },
  "monitors": {
    "P275MV PLUS": {
      "inputs": [
        { "name": "usbc", "value": 27, "verified": true },
        { "name": "dp1",  "value": 15, "verified": true }
      ]
    }
  }
}
```

面板「设置」底部的 `打开配置文件夹` 可以直接跳过去。日志在同目录的 `app.log`，出问题把它发我。

## 双机中继（可选）

两台机器都开着本程序时，会通过 UDP 广播（端口 47831）互相发现。

用途：如果 A 机的 DDC 不通但 B 机通，A 可以在本机切换失败时请 B 代劳 ——
因为 B 此刻正持有 DDC 通道。只要**任意一台**能用，双向都能切。

```bash
inputswitch-cli switch dp1 --relay
```

## 配对：别让别人的电脑动你的屏幕

点 `打开控制面板…` → 「配对」卡片里填。

两台填同一个码（6 位够用，越长越难猜）。保存后：

- 每条局域网消息都带 `HMAC-SHA256` 签名 + 时间戳 + nonce
- 验不过的直接丢，还会在面板上显示"被拒次数"
- 重放和篡改都过不去

没配对时不会拦人 —— 同一网段的任何设备都能给你的显示器发指令。
所以第一次用建议就把码填上。

```bash
inputswitch-cli pair --generate      # 生成一个随机码
inputswitch-cli pair 123456          # 直接设
inputswitch-cli pair                 # 看当前状态和密钥指纹
```

## 控制面板

菜单 → `打开控制面板…`，会弹出程序自己的窗口（不是浏览器标签页）。

一屏能看到所有状态：配对、每个输入口有没有主机、对端在哪台机器上、
热键、接收目录、传输进度。切换、改目标、扫描、传文件都在这里点。

窗口用的是系统自带的 WebView，不额外打包浏览器内核：

| 平台 | 内核 | 说明 |
|---|---|---|
| macOS | WKWebView | 系统自带，无额外依赖 |
| Windows | WebView2 | Win10 1803+ / Win11 自带；缺运行时会自动退回浏览器 |

页面本身跑在 `http://127.0.0.1:47833/`（只监听本机），窗口就是把这个地址装进去。
所以窗口实在开不出来时（比如 Windows 缺 WebView2 运行时），程序会**自动**改用
默认浏览器打开同一个地址 —— 功能一模一样，只是没了个窗口。命令行和脚本也能直接调这套接口。

### 托盘菜单只留四件事

托盘不再是第二个设置面板。能改配置的东西全在控制面板里，菜单只保留"不开窗口就能做完的事"：

| 菜单项 | 为什么留在托盘 |
|---|---|
| 状态两行 | 一眼看到当前口 + 这台显示器能不能被软件切 |
| `切换输入源 ▸` | 程序的核心动作，点两下就切完 |
| `共用键鼠` + 模式切换 | 边用边切的开关，开窗口太慢 |
| `打开控制面板…` | 其余所有事情的入口 |
| `退出` | 总得有个地方退 |

配对、传文件、热键、热键目标、开机自启、剪贴板、检测支持性、扫描口、打开配置文件夹、
以及原来那个"在浏览器中打开面板"—— **全部只在面板里出现一次**，不再两边各放一份。

> **Windows 分发注意**：`x86_64-pc-windows-gnu` 构建出来的 exe，WebView2 的载入器
> 是**动态链接**的（`webview2-com-sys` 只在 `target_env = "msvc"` 时才用静态库），
> 所以 `WebView2Loader.dll` 必须和 `InputSwitch.exe` 同目录，否则双击直接弹
> 「由于找不到 WebView2Loader.dll，无法继续执行代码」——这跟代码无关，是进程
> 启动阶段加载器就失败了。`cross-build-windows.sh` 已自动把它拷进 `dist/windows`。
> 用 MSVC 工具链构建时这个 DLL 会静态链进 exe，不需要陪它。
> 命令行版 `inputswitch-cli` 现在按 `--no-default-features` 单独构建，不带 wry，
> 产物零外部依赖，可以单文件分发。

> 为什么界面是 HTML 而不是原生控件：tao 只有窗口和事件循环，没有控件系统；
> 要做出一屏看全的界面得再拉一套 GUI 框架，而它们和 tao 的事件循环冲突
> （egui 绑的是 winit）。系统 WebView 是唯一既能跨平台、又不引入冲突事件循环的做法。

关闭窗口只是关掉，下次点菜单会重新开（约半秒）。服务只绑定 `127.0.0.1`，外网连不进来。

## 共用键鼠

两套**物理前提完全不同**的场景，装的时候要先选一种：

### 模式 A：跟随画面（软件 KVM，默认）

```
键盘鼠标 ──┐
           ├── 主机 A（键鼠插在这台）
显示器 ────┤
           └── 主机 B（不接键鼠）
```

一套键鼠插在 A 上。显示器在两台之间切，**画面切到哪台，键鼠就跟到哪台**。

- 切屏成功的那一刻，A 自动把键鼠抓过来转发给 B，B 注入回本机
- 切回 A（或按 `Ctrl+Alt+Esc`）立刻收回
- 面板里有两个按钮可以先手动试："现在就把键鼠交给对端" / "收回键鼠"

**这种场景下没有"鼠标推到屏幕边缘"这回事**：画面已经切走了，A 的屏幕是黑的，
用户看不见光标在哪，靠撞边缘触发等于让人盲操作。所以触发点是切屏动作本身。

> 早期版本只实现了模式 B，却当成通用方案 —— 在那台没显示器的机器上永远等不到
> "鼠标撞边缘"，表现出来就是"共用键鼠完全没工作"。这是个设计错误，已修。

### 模式 B：多屏共享

```
[ 屏幕 A ][ 屏幕 B ]
  键鼠 A     键鼠 B
```

两台机器各有屏幕、各有键鼠，并排摆。光标推到 A 的右边缘就越界到 B，
在 B 推回左侧边缘又回到 A。`Ctrl+Alt+Esc` 也能交还。

这是 Synergy / lan-mouse 的经典形态。两台都要装、都开着、配对一致。

### 模式 C：多台机器，十字拓扑（v0.6.47 起）

一台机器最多可以同时挂四条链路，上下左右各一台：

```
                    (0,-1) 上
        (-1,0) 左 · (0,0) 本机 · (1,0) 右
                    (0,1)  下
```

配置在 `config.json` 的 `kvm` 段：

```json
"kvm": {
  "enabled": true,
  "mode": "edge",
  "role": "console",
  "grid_x": 0,
  "grid_y": 0,
  "links": [
    { "node": "CHINAMI-0DFF1JU", "side": "right", "mode": "edge",   "input": "" },
    { "node": "MC5K9SJ",         "side": "bottom","mode": "edge",   "input": "" }
  ]
}
```

- `side` = **本机**从哪一侧出去到它。四向天然双向：A 在 B 右边 ⇔ B 在 A 左边，
  所以**不用两边各配一次**（Deskflow 的有向边要写两遍，写漏一边就是"能出去回不来"）
- `mode` 留空 = 用外层 `mode`；`edge` 占一条边，`follow` 不占边（靠切输入源触发）
- `input` 留空 = 用对端广播的 `own_value`
- `addr` 是上次连通时记下的 IP，**别手改**（对端换 IP 后会自动回填）
- 离线分两种：**留下过 `addr` 的仍占住那条边** —— 拔网线后从它那侧推，会得到
  一句"X 没有回应，不越界"，不会先把键鼠吞掉再等 2.5 秒；`addr` 空的不占边，
  那侧推过去毫无反应（面板里连通一次就会记下）
- 同一台机器不要配两条链路；两条 `edge` 链路也不要填同一个 `side`
  （后者会让其中一条永远走不到，日志的启动行会把每条链路的方向逐条列出来对照）
- 5 台必须**全部 ≥ v0.6.47**：老版本只认一个对端 IP，会把其它三台的包静默丢掉，
  表现是"半通"

### 切换方式

托盘 → `共用键鼠 · 开启（…）`，下面还有一项直接在两种模式之间切换。
面板「共用键鼠」卡片里可以选模式、选角色（键鼠插在哪台）、开关、手动交还/收回。

> 切换模式会重启键盘钩子（两种模式对"没抓取时该不该盯边缘"的处理不同），
> 所以切完需要重新打开一次共用键鼠。

### 怎么实现的

参照两个项目的公开设计重写，代码都是自己写的（两者都是 GPL，不能照搬）：

- **[lan-mouse](https://github.com/feschber/lan-mouse)**（Rust，GPL-3.0）：
  相对位移事件、按方向注册捕捉、设备二态防回环、交还时补 key-up。
- **[Deskflow](https://github.com/deskflow/deskflow)**（C++，GPL-2.0，Synergy 血统）：
  **心跳 + 空闲超时**、**事件流加密**、**锁定到某一台**。这三条是只看 lan-mouse
  时会漏掉、但真机上一定会踩的。

为什么两个都要看：lan-mouse 离我们的语言和体量最近（好读），Deskflow 是这套
东西跑了二十多年的成熟实现（可靠）。前者给结构，后者给教训。

| 环节 | macOS | Windows |
|---|---|---|
| 抓取 | `CGEventTap`（回调返回 NULL 即吞掉） | `WH_MOUSE_LL` / `WH_KEYBOARD_LL`（返回 1 即拦下） |
| 抓住光标 | `CGAssociateMouseAndMouseCursorPosition(false)` | 钩子拦下移动事件，光标自然不动 |
| 注入 | `CGEventCreateMouseEvent` + `CGEventPost` | `SendInput` |

几个不写就出错的地方：

- **必须能吞事件**。`rdev` 这类库只能旁观，拦不住本地光标，做不了 KVM。
- **注入要打标记**。注入的事件仍会经过自己的钩子，不打标记就是死循环。
  macOS 用 `CGEventSourceUserData`（字段 42），Windows 用 `dwExtraInfo`。
- **传相对位移，不传绝对坐标**。两台机器分辨率和缩放都不同，相对位移天然跨分辨率。
- **二态互斥**。一台机器要么在控制别人、要么在被控制，不能同时，否则 A→B→A 回环。
  多链路下这条扩展成**抢占矩阵**：正给 A 转发时收到 B 的开工请求一律拒绝
  （否则 A 还在收键鼠、B 同时开始注入，两台一起动）；已在对端控制下时收到第三方的
  开工请求，先把原来那台拆干净再换。
- **交还时补 key-up**。抓取期间按下的键，对端只收到 down，不补 up 会一直"卡住 Cmd"。
  多链路下还要注意：补发与 `Leave` 必须**显式寻址到正被释放那台**，先快照再发 ——
  清空会话目标之后补的 up 全丢。
- **事件走独立 UDP 端口**（`network.port + 3`），一个事件一个包、不确认。
  丢一个鼠标移动比用 TCP 把鼠标粘住强。
- **必须有心跳**。UDP 是无连接的，对端进程崩了、Wi-Fi 断了，本机收不到任何东西，
  但仍停在"正在控制对端"，于是把自己的键鼠全吞了 —— 用户看到的是鼠标没了、键盘也死了。
  Deskflow 用空闲超时解决，这里照做：每 800ms 发一次 `Ping`，2.5 秒收不到任何包
  就强制交还并写日志。
- **事件流要加密**。Deskflow 默认 TLS。我们配对后用配对码派生的密钥做
  ChaCha20-Poly1305（每个包 25 字节开销，不用握手、不用证书）。没配对时是明文，
  日志和面板都会警告。
- **锁定到本机**。拖窗口、拖分隔条的时候鼠标很容易撞到边缘，一撞就切走很烦。
  菜单 → `锁定在本机` 打开后，撞边缘也不再切出。

### 权限（macOS 必须做，只做一次）

系统设置 → 隐私与安全性：

- **辅助功能**：勾上 InputSwitch —— 抓取和注入都要它
- **输入监控**：勾上 InputSwitch —— 抓键盘要它

⚠️ 我们是 ad-hoc 签名，**每次重新编译 cdhash 都会变，授权会失效**，
表现为"看着在跑但鼠标抓不住"。需要去列表里删掉旧条目重新添加。
根治得用 Developer ID 证书做稳定签名（99 美元/年）。

Windows 不需要授权。但安全软件可能拦 `SetWindowsHookEx`，被拦时菜单会报错而不是静默失败。

### 自检

```bash
inputswitch-cli kvm-probe                       # 看权限和屏幕尺寸
inputswitch-cli kvm --peer 192.168.0.40         # 直接跑，不经过托盘
inputswitch-cli kvm --peer 192.168.0.40 --side left --seconds 30
```

## 传文件

面板「文件」卡片里点 `发送文件…`，或者点对端卡片上的 `发文件`。

- 走 TCP 47832，复用发现层已经知道的对端 IP，不用再填地址
- 支持多文件和整个目录（保留相对路径）
- 每个文件 SHA-256 校验，对不上直接丢，不会留一个看起来正常的坏文件
- 重名不覆盖，自动加 `(1)`
- 接收目录默认 `~/Downloads/InputSwitch`，可以在面板里改
- 对端塞 `../../` 这种路径会被拒绝

```bash
inputswitch-cli send ~/Desktop/报告.pdf     # 命令行直接发（不用开托盘）
```

## 剪贴板

面板「设置」里点 `同步剪贴板文本` 那一行的开关。

500ms 轮询一次变更计数，内容哈希去重防回环。只同步**文本** ——
图片和文件列表要各自处理格式转换，暂时没做。

```bash
inputswitch-cli clip             # 读当前剪贴板
inputswitch-cli clip "测试文本"   # 写进去（自检用）
```

## 一个必须知道的副作用

**Windows 会认为显示器被拔掉了。** 切换走后桌面图标可能重排、窗口会挤到主屏。
这是纯软件方案的天花板 —— Windows 的显示拓扑跟着物理连接走，DDC 切换等同于拔线。

要彻底解决只有一条路：带 EDID 模拟的硬件 KVM，它能让非活跃端口保持"假在线"。
但那就不是软件能做的事了。

## 命令一览

| 命令 | 作用 | 会改屏幕吗 |
|---|---|---|
| `inputswitch-cli probe` | 列出显示器与 DDC 能力 | 否 |
| `inputswitch-cli selftest` | 诊断 + 排查建议（有问题退出码 1） | 否 |
| `inputswitch-cli status` | 当前输入源 + 局域网对端 | 否 |
| `inputswitch-cli switch <口>` | 按名字切 | **是** |
| `inputswitch-cli switch <口> --relay` | 本机失败时请对端代切 | **是** |
| `inputswitch-cli wizard --own X --other Y` | 遍历找出真实取值 | **是** |
| `inputswitch-cli autostart [--on\|--off]` | 开机自启开关 | 否 |
| `inputswitch-cli pair [码] [--generate]` | 看/设配对码 | 否 |
| `inputswitch-cli send <路径…>` | 直接发文件给对端 | 否 |
| `inputswitch-cli clip [文本]` | 读/写剪贴板 | 否 |
| `inputswitch-cli daemon` | 无托盘常驻（headless） | 按热键时 |

## 从源码构建

```bash
cargo build --release --bins
```

产物两个二进制，必须分开编，原因在 Windows 上（名字不能只差大小写 ——
macOS 文件系统不区分大小写，`InputSwitch` 和 `inputswitch` 会互相覆盖）：

| 二进制 | 子系统 | 用途 |
|---|---|---|
| `InputSwitch` | windows（GUI） | 托盘本体，双击不弹黑框 |
| `inputswitch-cli` | console | 命令行诊断，终端里能看到输出 |

打包脚本：

```bash
./build.sh          # macOS → dist/InputSwitch.app（含图标、ad-hoc 签名、LSUIElement）
.\build.ps1         # Windows → dist\windows\InputSwitch.exe
```

在 macOS 上交叉编 Windows 版需要 `rustup target add x86_64-pc-windows-gnu` 加 mingw-w64 工具链；
最省事的是直接用 Zig 的 `zig cc` 当链接器。

## 三个平台的 DDC 实现

| | Windows | macOS | Linux |
|---|---|---|---|
| 通道 | `dxva2.dll` | IOKit `IOAVService*I2C` | `ddcutil` 子进程 |
| 读 VCP | 稳定 | **多数机器读不到** | 稳定 |
| 写 VCP | 稳定 | 看连接方式 | 稳定 |
| 句柄 | `GetPhysicalMonitorsFromHMONITOR` 拿的物理句柄 | `IOAVServiceCreateWithService` | `/dev/i2c-*` |

Windows 上一个高频坑：`SetVCPFeature` 的第一个参数必须是
`GetPhysicalMonitorsFromHMONITOR` 返回的**物理监视器句柄**，直接传 `HMONITOR` 一定失败。

macOS 上 DDC 走 Display Coprocessor，读操作经常返回未初始化内存。代码对每一帧做
校验和 + 长度位校验，不通过一律返回 `None`，**绝不把垃圾当数据**。这是有实测依据的：
某台 P275MV PLUS 在 USB-C 下连续 24 种请求帧组合，返回的都是同一串 `6e 80 be ...` 垃圾。

## 代码结构

```
src/
├── models.rs     数据结构、VCP 常量、MCCS 标准值表
├── config.rs     配置持久化（profile 按显示器型号索引）
├── switcher.rs   切换编排、拓扑验证、输入源发现
├── peer.rs       UDP 广播发现 + 中继指令
├── autostart.rs  开机自启（LaunchAgent / 注册表 Run）
├── tray.rs       托盘 + 全局热键（tao 事件循环 + 工作线程跑 DDC）
├── gui.rs        托盘入口（windows 子系统）
├── main.rs       CLI 入口（console 子系统）
└── ddc/
    ├── mod.rs    后端 trait
    ├── mac.rs    IOKit IOAVService（纯 FFI，无第三方绑定）
    ├── win.rs    dxva2（纯 FFI）
    └── linux.rs  ddcutil
```

加新平台只需实现 `ddc::Backend` 的 `list_displays` / `get_vcp` / `set_vcp` 三个方法。

# inputswitch 项目开发规则

> 一台显示器接两台主机：热键 / 托盘一键切 DDC/CI 输入源，附软件 KVM、配对加密、文件传输、剪贴板同步。
> 纯 Rust 双二进制（`InputSwitch` 托盘 + `inputswitch-cli` 诊断），面板前端是手写 JS/CSS，由 `panel.rs` `include_str!` 内嵌进 exe。
> 规则对标 `cc-bridge/CLAUDE.md`（其自身对标 PastePanda），按 inputswitch 的真实技术栈与目录结构适配。

## 0. 规则优先级

1. **`CONTRIBUTING.md`** —— 随仓库发布、协作者可见的公开约定（构建/测试/协作/界面铁律/发版）。
2. **本文件** —— AI 与本人的操作细则，只补执行细节。

冲突时以 `CONTRIBUTING.md` 为准，尤其：**commit 风格、发版流程、测试必须单线程**这三处。本文件不得与之相悖。

---

## 1. 先出方案再动手

修改代码前先出方案让用户确认；涉及取舍时给 2-3 个方案对比（含优缺点），让用户选。复杂任务先进 Plan Mode 规划再执行。

例外（直接做）：单文件小修、改注释/文案、跑门禁命令、只读排查。

## 2. 版本号只有一个真源：`Cargo.toml` 的 `version`

- **真源**：`Cargo.toml` 的 `version = "X.Y.Z"`。代码里一律读 `crate::VERSION`（即 `env!("CARGO_PKG_VERSION")`），**不许再手写 `vX.Y.Z`**。启动日志里那句 `构建=v{}` 已经从手写字面量改成引用 `crate::VERSION`。
- **为什么在 2026-10-27 换成 `Cargo.toml`**：自动更新要拿「当前版本」去比远端 `updater.json` 的 `version`，这个值必须机器可读且唯一。以前真源是 `tray/mod.rs` 里手写的字符串，改版本要在两处同步，漏一处就会出现「面板说 0.6.90、日志说 0.6.89」这种没法排查的错位。
- **展示副本要跟着改**：`assets/badge-release.svg` 里的 `vX.Y.Z` 是静态文本（README 的 release 徽章），发版时与真源一起改。漏改 = 徽章版本和真实版本不符。
- **不发版就不动版本号**。日常改动只体现在 commit message 里，发版时再归到 Release notes。
- 只有用户明确说 **"发版" / "tag"** 才递增，并走「发版流程」。

## 3. 打包 exe 前必须问用户

`build.ps1`（Windows）/ `build.sh`（macOS）跑 release + LTO + `codegen-units=1`，要几分钟，用户确认后再跑。Windows 分发安装器还要 `node tools/pack-windows.js`（stage 产物 → makensis → `dist/windows/InputSwitch_<版本>_x64-setup.exe`，本机需装 NSIS；CI 里由 release.yml 的 choco 步骤提供）。

编译前先关掉正在运行的 `InputSwitch.exe`：Windows 会锁住 exe 导致链接失败，且托盘程序常驻，很容易忘。

## 4. 改面板 UI 前先读真实源件，改完必跑 panelcheck

- 面板是 `assets/panel/` 七件，`index.html` + `style.css` + 五段 JS，按路由分别 `include_str!`。
- 门禁：`node tools/panelcheck.js assets/panel`，**110 条断言**（P2 壳体重构后从 80 条扩到 110：四页切换、侧栏宽档、页脚三件各一组）。它管的不只是渲染，还有多年踩坑沉淀的**界面铁律**——改之前先跑一遍拿基线，别等改完再看红。
- 界面铁律以 `CONTRIBUTING.md` 为准：主切换按钮永远显示（不能切时换文案 + 灰蓝，绝不 disabled/隐藏）、全页只有一个实心蓝按钮（P3 起更新弹窗里的主操作是唯一例外：遮罩盖上时顶栏那个根本看不见，「任何一刻屏上只有一个蓝块」没破）、顶栏三个维护动作平铺、托盘菜单是系统控件不可定制。
- **新增 JS 文件要同步四处**：`index.html` 的 `<script src>`、`tools/panelcheck.js` 的 `JS_ORDER`、`src/panel.rs` 的 `const PANEL_X = include_str!(...)`、以及 `panel.rs` 里对应的路由分支。加载顺序是 `core.js → render.js → kvm.js → update.js → actions.js`（见 `index.html` 尾部），core 的全局变量要让后面的看得见，顺序错了 panelcheck 的 DOM 桩会 undefined。

## 5. 等用户说「提交」再 commit

不要自动 `git commit`、不要自动 `git push`。用户说"提交 / commit"后，先走第 15 条（审 diff），再审完提交，最后单独确认是否 push。

## 6. 本机验证路径（没有 HMR，Rust 改动必须重编）

- **改面板 JS/CSS 也要重编**：`include_str!` 是编译期内嵌，改完不重编 exe 看到的还是旧面板，**不存在热更新**。
- 分层验证，避免每次都开窗口：
  1. `node tools/panelcheck.js assets/panel` —— 离线断言，秒级
  2. `cargo build --bins` —— 真编进 exe
  3. `.\target\debug\InputSwitch.exe` → 托盘「打开控制面板…」—— 看真实 WebView2 渲染
- **CLI 是主要验证手段**：`inputswitch-cli probe / selftest / status`，零依赖、能直接看到 DDC 能力与对端状态。
- ⚠️ 本机（Lenovo 21B1，只接内屏走 eDP）DDC 不可用，`probe` 必报「DDC 完全没有应答」。**这是正常现象**（内屏基本不转发 DDC/CI），不是环境坏。切屏类改动只能在"外接 DP/HDMI + OSD 开 DDC/CI"的机器上验，本机只能验编译与逻辑。

## 7. 架构、模块化与体量

**模块职责见 `src/lib.rs`。新增功能默认新建模块** `src/xxx.rs` + 在 `lib.rs` 声明，不要往巨型文件里追加。

历史上七个巨型文件已经拆掉，拆完都由 `tools/check-size.js` 的基线棘轮盯着：

| 模块 | 拆前 | 现状 |
|---|---|---|
| `src/tray/` | `tray.rs` 4202 行 | 22 个文件，最大 `menu.rs` 575 行；`engine/` 六文件承接原 `engine.rs`（最大 `probe.rs` 208 行）；`actions/` 五文件承接原 `actions.rs`（最大 `kvm.rs` 286 行） |
| `src/kvm/` | `kvm/mod.rs` 3570 行 | 29 个文件，最大 `win.rs` 1723 行（平台钩子，天然大）；`capture/` 六文件承接原 `capture.rs`（最大 `mod.rs` 291 行） |
| `src/cli/` | `main.rs` 1379 行 | 7 个文件，最大 `act.rs` 346 行；`main.rs` 只剩 clap 定义 + 分发表；`update.rs` 176 行（唯一要联网的命令，单独放好排查） |
| `src/config/` | `config.rs` 1117 行 | 5 个文件，最大 `store.rs` 517 行；`mod.rs` 只管切分轴与 `pub use` 收口 |
| `src/switcher/` | `switcher.rs` 979 行 | 7 个文件，最大 `scan.rs` 379 行；`mod.rs` 只管切分轴与 `pub use` 收口 |
| `src/kvm/event/` | `kvm/event.rs` 832 行 | 4 个文件，最大 `codec.rs` 336 行；`mod.rs` 只管切分轴与 `pub use` 收口 |
| `src/kvm/recv/` | `kvm/recv.rs` 793 行 | 5 个文件，最大 `input.rs` 285 行；`mod.rs` 的 `Ctx` 收拢 spawn 的全部状态（`inject` 除外 —— macOS 的 `Inject` 含 `CGEventSourceRef` 不是 `Send`，收进来 `thread::spawn` 直接编译不过，它在 `run()` 里现造、按参数传给要用它的方法），`run()` 只留收包 → 验址 → 解密 → 分发骨架 |
| `src/update/` | （新增模块） | 7 个文件，最大 `mod.rs` 580 行；**零新增依赖**（网络走系统 curl、摘要用已有 sha2、版本比较自己写） |

拆分思路：平台差异 → `kvm/{win,mac}.rs`；纯计算/协议 → 独立模块；主线程的全部可变状态 → `tray/ui.rs` 的 `Ui` 结构体（每个 tao `Event` 一个 `on_*` 方法）；命令行 → `cli/` 按「跑下去会不会真的改动屏幕」切（只读诊断与会黑屏的操作分开，排查时滚动不易看漏）；配置 → `config/` 按 schema / paths / kvm / store / state 切（纯数据、本机路径、链路表行为、读写盘、运行状态各占一个文件）；切换编排 → `switcher/` 按 types / probe / topology / switch / scan / discover 切（报告名词、身份与能力、屏幕在不在、切换判据、扫描、向导各占一个文件）；事件协议 → `kvm/event/` 按 codec / edge / seal 切（事件枚举与编解码、边界几何、加密封装各占一个文件，测试跟着被测代码走）；接收线程 → `kvm/recv/` 按 match 分支切（enter 接管、input 注入、handoff 交还、upkeep 维护各占一个文件，730 行 `move` 闭包的捕获变量收进 `Ctx` 结构体，分支中间的 `continue` 提进方法后变 `return`）；工作线程 → `tray/engine/` 按命令性质切（switch 切屏执行、probe 探测、clip 剪贴板传输、beat 心跳协作各占一个文件，`engine_loop` 的 4 个参数加 4 个跨分支局部状态收进 `Ctx`，11 个 `Command` 分支提成 `on_*` 方法，嵌套函数 `push` 改成 `Ctx` 方法只留 `(target, text)`；4 个查询助手留在 `status.rs` 做自由函数 —— `tray::switch` 也要用 `sync_own`，方法化反而要把 `Ctx` 暴露过去）；面板动作 → `tray/actions/` 按「改一件事要动几个文件」切（配对码 pair / 共用键鼠总控 kvm / 链路摆位 link / 系统开关与杂项 system 各占一个文件，21 个 `Action` 分支提成 `on_*` 方法，`on_panel` 只留分发骨架，`_ => {}` 兜底 Switch / Rescan 那几个「不用回主线程」的变体）；抓取线程 → `kvm/capture/` 按 match 分支切（begin 撞边进门、input 转发、claim 抢占认领、end 松手收尾、hop 改指向各占一个文件，600 行 `move` 闭包的 18 个参数加 17 个跨分支循环状态收进 `Ctx` 结构体，5 个 `Captured` 分支提成 `on_*` 方法，Note / PasteIntent 留分发骨架内联）。

**自动更新（`src/update/`，2026-10-27 落地 P0）** —— 编排结构对齐 cc-bridge 的 `commands/update.rs`，底层全部换成**零新增依赖**的等价实现。三个决策已拍板：

| 决策点 | 选择 | 代价 / 说明 |
|---|---|---|
| Windows 安装形态 | **NSIS 安装器**（下载 setup.exe，静默 `/S` 安装） | 分发形态从「解压即用」变成安装器；首个带 updater 的版本**必须手动装一次**，自动更新只从它往后生效 |
| 验签 | **SHA-256 + HTTPS**（不引 minisign） | 挡不住「发布源本身被攻陷」，但能挡住下载截断、串包、镜像篡改 |
| 更新源 | **ghproxy → GitHub 两级** | 不做 Gitee 镜像；ghproxy 挂了还有直连兜底 |

零新增依赖是硬约束：网络走系统 `curl` 子进程（Win10 1803+ / macOS 自带），摘要用已在依赖里的 `sha2`，版本比较自己写（`manifest::is_newer`）。**不要再引 `ureq`/`reqwest`** —— 当前 `rustls` 默认加密后端是 `aws-lc-rs`，需要 C 工具链，「交叉编译到 Windows 不需要 C 工具链」的前提会当场破功。

分期：P0 检查（已完成，`inputswitch-cli update` 可无界面验证整条链路）→ P1 下载/进度/摘要校验（已完成，`update --download` 落到临时目录并校验）→ P2 安装/重启（已完成，`update --install` 等本进程退出的静默安装 + 8 秒重启倒计时可取消）→ P3 面板 UI（已完成，徽章/更新说明弹窗/重启过渡层 + `/api/state` 的 `update` 字段 + `/api/update` 单入口五动作；`UpdateState` 直接嵌进面板 `Snapshot`，没再抄一份 view）→ P4 发布流水线（已完成，`packaging/windows/InputSwitch.nsi` + `tools/pack-windows.js` 出 NSIS 安装器，`tools/gen-updater-json.js` 扫产物现算 SHA-256 生成清单，`.github/workflows/release.yml` 推 tag 自动构建双端并建 Release，`tools/release.js` 本地编排与预检，`tools/fake-update-server.js` 本地 HTTPS 假源联调）。

**体量棘轮**（存量文件只准变小不准变大；新增文件上限 Rust 800 行 / 面板 400 行）：

- `node tools/check-size.js` —— 查；`--update` 把现状写回基线（只降不升）
- `--rename <旧>:<新>` —— `git mv` 后基线数值原样继承，**必须先于检查应用**
- `--allow-grow <文件>` —— 显式放行某个文件变大（会记进脚本，可审计）

**面板体量棘轮**（同样只准变小；新增 UI 区块**另起文件**）：

| 文件 | 基线行数 |
|---|---|
| `assets/panel/style.css` | 578 |
| `assets/panel/kvm.js` | 395 |
| `assets/panel/render.js` | 280 |
| `assets/panel/index.html` | 229 |
| `assets/panel/update.js` | 184 |
| `assets/panel/actions.js` | 97 |
| `assets/panel/core.js` | 72 |

与 cc-bridge 的 300 行硬线不同：本项目面板是手写 JS、无框架无构建，`style.css` 529 行是设计令牌 + 组件样式的自然体量，强行切 300 会产生大量 `@import` 或重复。**用基线棘轮代替硬线**。

## 8. 性能与常驻循环六问

本项目是"转发键鼠"的程序，热循环直接决定**手感**和 CPU 占用，不是可选项。

新增或修改任何 `loop` / 周期任务 / 事件消费循环（`tray/` 的 400ms tick 与事件循环、`kvm/*` 的 16ms 钉扎线程、心跳、DDC 探测轮询）时，**逐条回答**：

1. **让出点在哪里？** 必须能指出 `sleep` / `interval().tick()` / 阻塞系统调用 / 有界 IO 之一。"循环里有 `.await`"不算——`await` 一个立即完成的 future 等于没让出。
2. **生产端异常时这个循环会变成什么？** 把"通道关闭 / 返回错误 / 立即返回"当成**必然发生**去推，而不是当异常。
3. **"流已结束"的信号有没有被处理？** 禁止 `let _ = rx.recv().await`。特别注意 `timeout(d, fut).await.is_ok()` 对**已关闭通道**是 `Ok(None)` → `is_ok()` 为真，这不是"还有事件"。
4. **每轮单次成本是多少？** 系统调用、锁、子进程、枚举各几次？频率 × 单次成本要能口头算出来。
5. **有没有兜底上限？** 吸收/重试/合并类循环必须有 `MAX_*` 或退避，让"生产端发疯"时最坏退化成固定频率。
6. **平台分支那一半怎么验？** 本机只有 Windows，写了 `cfg(not(windows))` 就必须回答它在 mac 上靠什么保证（见第 10 条）。

**本项目特有的一条**：DDC 探测是"可能把显示器切走"级别的重动作，**禁止塞进紧循环或每次轮询**。`wizard` / `scan` 由用户手动触发，失败要有退避，不能自动重试刷屏。

## 9. 失败要说清楚，不猜、不谎报

这是 `CONTRIBUTING.md` 的家规 3，执行细则：

- **异步回执只能承诺已发出的事**：「已请对端交还」不能写成「已收回」。
- **多处判据必须同源**：后端算好的字段，前端不要自己再推一遍。
- **"读不到"要区分原因**：不支持 DDC / 没权限 / 这一路没在显示 / 中间串了扩展坞——`probe` 的通路诊断文案就是范例，别一律写成"失败"。
- 查不出根因就说查不出，不要给一个看起来合理的猜测。

## 10. 平台分支与验证边界

本机只有 Windows。写 `#[cfg(target_os = "macos")]`（`src/ddc/mac.rs`、`src/kvm/mac.rs`、`build.sh`、`cross-build-windows.sh`）必须回答：**mac 上的行为靠什么保证**——CI 的 macOS job（`cargo check --bins` + `cargo test --lib`）/ 代码契约注释 / 真机。**不能靠"看着对"**。

Linux 同理：只有 `src/ddc/linux.rs` + xclip 兜底，CI 只做 `--no-default-features` 的纯库编译，运行时行为本机验不了。

## 11. 安全模块不得放松

- 配对：HMAC-SHA256 签名 + 时间戳防重放（`src/pair.rs`）
- 键鼠事件流：ChaCha20-Poly1305（`src/kvm/`）
- 文件传输：路径穿越校验（`src/transfer.rs`）
- 对端指令准入：必须过 `peer::passes_pair_guard`

任何改动都不得削弱以上约束。以下测试是红线，**别删、别 skip、别改成恒真**：

`pair::tests::nonce_replay_is_rejected`、`transfer::tests::rejects_traversal`、`peer::tests::refuses_command_when_peer_wants_pair`、`peer::tests::refuses_command_to_unknown_peer`、`peer::tests::refuses_command_to_dead_peer`

## 12. 文件存放与命名

**沿用现状，不追溯改名**（README 与源码注释已有引用链）：

| 类型 | 目录 | 现有示例 |
|---|---|---|
| 文档 | `docs/` | `manual.md` |
| 设计稿 / 取证图 | `design/` | `panel-mock.html`、`icon-refine/` |
| 校验与生成脚本 | `tools/` | `panelcheck.js`、`make-icons.py` |
| 面板前端 | `assets/panel/` | 七件 |

与 cc-bridge 的"文件名一律中文"**故意不同**：本项目是公开仓库，`README` 与源码注释里已有英文引用，改中文名的收益低于断链成本。若要中文化，必须连带改所有引用，单独提一次。

## 13. 行尾：不要加 `.gitattributes`

现状 `core.autocrlf=true`，`git ls-files --eol` 显示 `i/lf  w/crlf` —— **仓库内已是 LF 存储、工作区 CRLF 检出**，这个组合是自洽的，normalize 不会产生噪声。

**不要**照搬 cc-bridge 加 `* text=auto eol=lf`：本项目有 24 个工作区文件会被触发 renormalize，产生大量假改动，还和协作者的环境打架。写文件行尾跟周围一致即可，git 会归一化存储。

## 14. Git push 排查

- remote 是 HTTPS `https://github.com/kynnzhou/inputswitch.git`；gh 已认证、对本仓库有 push 权限（协作者）。**账号名不写进文档**——换人或换 token 时写死的就成了假信息，每次以 `gh auth status` 为准。
- 本机若走了 HTTP 代理（`http.proxy` / `HTTP_PROXY`），HTTPS 可能报 `curl 16 HTTP2 framing layer`，且**失败输出末尾仍会跟 `Everything up-to-date`**（极具欺骗性）⇒ 用 `PUSH_EXIT=$?` 定性，重试加 `-c http.version=HTTP/1.1 -c http.postBuffer=524288000`。代理地址同样因人而异，以 `git config --get http.proxy` 为准，不写死。
- **判定是否真推上去，不看 push 输出**：`git ls-remote https://github.com/kynnzhou/inputswitch.git refs/heads/main` 取远端真值，与本地 HEAD 比对。别信本地陈旧的 `origin/main`。
- push 前确认没有 `InputSwitch.exe` / `cargo.exe` 在跑（抢 `target/` 锁会让门禁失败）。
- `403 Bad credentials` / `Repository not found`：`gh auth status` 看账号与 scope（需 `repo`）。

## 15. 提交前必须审一遍本次改动有无 bug

用户说「提交 / commit」后，**先不要 `git add`**：

1. **逐文件审 diff**（`git diff` + 新增文件通读），重点查：逻辑错误、边界条件（空值/越界/极端输入）、安全（凭据泄漏、路径穿越、注入）、**遗漏的接线点**（改了 A 没改调用方——本项目尤其容易漏：改 CLI 忘改托盘、改面板忘改 `include_str!` 路由）、与既有约定的冲突。
2. **有问题当场修**，修完重跑受影响的门禁（第 17 条），再进提交流程。
3. **确认无问题才提交**；审查发现但决定不修的点，须在 commit message 或回复里说明。
4. 工作区混有并行改动时按批次归属，**绝不 `git add -A`**。

## 16. Commit 信息：一行中文，说清"做了什么 + 为什么"

**与 cc-bridge 的 `feat:/fix:` 前缀故意不同。** 本仓库 `CONTRIBUTING.md` 与现有 `git log` 都是中文一行流：

```
v0.6.71 幽灵输入口：对端自报不再当本机证据
v0.6.89 面板源码拆分为 assets/panel/ 六件 + v0.6.77~v0.6.88 积压改动；补 MIT LICENSE
```

不要引入 Conventional Commits 前缀——会与协作者的提交历史不一致。

---

## 发版流程（以 `CONTRIBUTING.md` 为准，此处只补执行顺序）

1. 改 `Cargo.toml` 的 `version` + `assets/badge-release.svg` 里的 `vX.Y.Z`（**两处都要改**，徽章是静态 SVG，不会自动同步）
2. 提交，PR 合进 `main`（protect-main），本地切到最新 `main`
3. `node tools/release.js check` 预检：工作区干净 / tag 未占用 / gh 已登录 / 徽章版本与 Cargo.toml 一致
4. `node tools/release.js tag --yes` 打 annotated tag 并推送（链路探测：直连与代理 7897 轮流挂，谁通走谁）
5. `.github/workflows/release.yml` 接手：Windows job（choco 装 NSIS → 编译 → `pack-windows.js`）/ macOS job（`build.sh` 出 zip）/ publish job（**校验 tag 与 Cargo.toml 一致** → 汇总产物 → git log 攒 notes → `gen-updater-json.js` 现算 SHA-256 → `gh release create`，重跑时退成 `upload --clobber` 保证幂等）

本地手工路径（补包用）：`node tools/gen-updater-json.js <目录> --notes-file notes.md` + `node tools/release.js publish <目录> --yes`。`release.js` 所有子命令默认 dry-run。

**tag 名必须等于 `v<Cargo.toml version>`** —— 这是唯一能被机器校验的约定，publish job 第一步就查，不一致直接红。

**每一步前先问用户**，第 2 步前先跑门禁。

---

## 17. 门禁现状：不要假装它是绿的

`CONTRIBUTING.md` 只列了三条检查，实际跑下来有四条红绿不一。**如实记录，别在规范里写"保持零警告"却没人执行**——那正是规则烂掉的方式。

| 检查 | 命令 | 现状 |
|---|---|---|
| 编译 | `cargo check --bins` | ✅ 通过 |
| 构建 | `cargo build --bins` | ✅ 通过（两 exe 均链接成功） |
| 瘦构建（bin） | `cargo build --bin inputswitch-cli --no-default-features` | ✅ 0 警告（2026-10-27 清掉 `cli/service.rs:53` 的 `let mut sw` 存量；这个形态 release.yml 真构建，此前门禁的 no-default-features 只查 `--lib`，bin 的警告一直没人管——`cmd_daemon_headless` 整个函数是 `cfg(not(feature = "tray"))` 专属，默认构建里不存在，所以只在瘦构建现形） |
| 单元测试 | `cargo test --lib -- --test-threads=1` | 109/109 通过（2026-10-27 实测，`update` 模块 52 条 + 面板 round-trip 1 条 + 锚点取帧 1 条；`clipboard` 那 2 条在拿不到剪贴板的受限 shell 里会失败，属执行环境问题，先在自己终端确认） |
| 面板断言 | `node tools/panelcheck.js assets/panel` | ✅ 110 条全过（2026-10-28 P2 壳体重构：新增四页切换 / 侧栏宽档 / 页脚三件 30 条；含 P3 自动更新五幕：徽章/弹窗/进度/过渡层/忽略） |
| clippy | `cargo clippy --all-targets -- -D warnings` | ✅ **0 条**（2026-10-25 从 65 条清到 0，已具备进钩子/CI 的条件） |
| fmt | `cargo fmt --check` | 🔴 46 个文件有 diff（2026-10-26 实测；`kvm/recv/`、`tray/engine/`、`tray/actions/`、`kvm/capture/` 拆分零新增，recv/ 下仅剩 mod.rs 1 处 —— while 体多缩进 4 格，从原 `recv.rs` 迁移来的存量；engine.rs 删掉后 49 → 48，actions.rs 删掉后 48 → 47，capture.rs 删掉后 47 → 46 —— 旧 capture.rs 自带 1 处存量，删除即消账） |
| 体量 | `node tools/check-size.js` | ✅ 通过（2026-10-28 P2 壳体重构：面板五件按 `--allow-grow` 审计放行 —— core.js +57 / style.css +90 / index.html +39 / render.js +31 / actions.js +8，全是壳层逻辑与新 DOM，基线已写回） |
| 发版预检 | `node tools/release.js check` | ✅ P4 实测：正确拦截未提交改动与 tag 占用、核对徽章版本、dry-run 不碰任何东西（gh 登录状态本机已具备） |
| 发布产物自检 | `node tools/gen-updater-json.js <目录> --check` | ✅ P4 实测：重算 SHA-256 与清单逐平台比对，防手改 |

**纪律**：
- **`--test-threads=1` 不是可选项**，并行跑会 SIGABRT，这不是 bug。
- **不要全仓 `cargo fmt`**：`tray/` 和 `kvm/` 会被整体重排成巨型 diff，把协作者在飞的改动全变成冲突。只对**你新写的文件**保持 rustfmt 风格，改动老文件时跟周围代码走。
- **clippy 已清零，可以进钩子/CI 了**；fmt 还是红的，别在 46 个文件没处理前把 `cargo fmt --check` 写进门禁——那会让门禁从第一天就是红的、然后被所有人无视。
- 现在 CI 只有四张面（linux 纯库编译 / panelcheck / macOS 编译+测试 / Windows 编译），**没有 clippy 和 fmt**。想加要单独提 PR 跟协作者对齐，别顺手塞进功能 PR。
- 本地**没有** pre-push 钩子（`.git/hooks` 为空）。cc-bridge 那套"七项门禁钩子"没搬过来：先有绿的门禁，再谈自动执行。

---

## 附：与 cc-bridge 规则的关键差异（照抄会错的地方）

| cc-bridge 规则 | inputswitch 的处理 | 原因 |
|---|---|---|
| 版本真源 = `Cargo.toml` | **一致**：真源也是 `Cargo.toml` 的 `version`，代码一律读 `crate::VERSION` | 2026-10-27 统一 —— 自动更新要拿版本号做大小比较，手写字符串做不到机器可读 |
| 前端 ≤300 行 + hooks 计数 | 面板用**基线棘轮**，不设硬线 | 手写 JS 无框架，`style.css` 529 行是自然体量 |
| `cargo fmt --check` 零警告 | **不要全仓 fmt**，只保新文件 | 现状 46 个文件不干净，全格式化会和协作者冲突 |
| commit 用 `feat:/fix:` 前缀 | **一行中文**，无前缀 | 与 `CONTRIBUTING.md` 和现有 log 一致 |
| 加 `.gitattributes` 统一 LF | **不加** | 索引已是 LF，加了会触发 24 文件 renormalize 假改动 |
| 文件名一律中文 | 沿用短英文名 | 公开仓库，README/注释已有引用 |
| 七项 pre-push 钩子 | 暂不装 | 门禁还没全绿，先人跑 |
| Tauri dev / Vite HMR | 无热更新，改面板也要重编 | `include_str!` 编译期内嵌 |

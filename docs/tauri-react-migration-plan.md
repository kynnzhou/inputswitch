# Tauri 2 + React 迁移方案（对齐 cc-bridge 架构）

> 2026-10-29 出方案，等用户确认后分期执行。本文只规划「改什么、怎么分期、每期验收」，
> 不含时间估计。

## 1. 目标与边界

**目标**：前端从手写 vanilla JS 迁到 React 18 + TypeScript + Vite + vitest + eslint 工程体系；
桌面壳从裸 tao+wry 迁到 Tauri 2；工程化体系（pre-push 七项门禁 / CI frontend job / CHANGELOG /
版本同步脚本）对齐 cc-bridge。

**边界（明确不动）**：
- `src/` 核心库 97 个文件全部不动：`kvm/`、`ddc/`、`switcher/`、`transfer/`、`pair.rs`、`peer.rs`、
  `update/`、`clipboard.rs`、`config/`。它们是平台无关纯逻辑，与前端技术栈无关。
- `panel.rs` 的 HTTP API 契约不动：约 40 个 `/api/*` 端点、`Snapshot` 序列化形状、
  `Action` 分发模型全部保留。CLI / 脚本 / 浏览器退路继续复用同一套接口。
- 安全红线不动：HMAC-SHA256 + 时间戳防重放、ChaCha20-Poly1305、路径穿越校验、
  `peer::passes_pair_guard`。
- 自动更新继续用自研 `update/` 模块（curl 子进程 + sha2 + NSIS），**不引 tauri-plugin-updater**。

## 2. 前置探针结论（2026-10-29 实测，本方案的 go/no-go 依据）

| 探针 | 命令 | 结论 |
|---|---|---|
| 基础 Tauri 是否引入 C 工具链加密库 | 新建空 crate，`tauri = { version = "2", default-features = false, features = ["wry"] }`，`cargo tree`  grep `aws-lc-rs\|aws-lc-sys\|ring\|reqwest\|rustls` | **0 命中**。基础 tauri 树干净 |
| 常用小插件是否引入 | 追加 `tauri-plugin-dialog / clipboard-manager / opener / process / global-shortcut` | **0 命中**（97 包） |
| updater 插件 | cc-bridge `Cargo.lock` 实证：`tauri-plugin-updater 2.10 → reqwest 0.13 + rustls 0.23 → aws-lc-rs → aws-lc-sys` | **命中，破功**。aws-lc-sys 要 C 工具链，「交叉编译到 Windows 不需要 C 工具链」的前提当场破 |

**结论**：全量迁 Tauri 可行，**唯一禁止项是 tauri-plugin-updater**。这恰好与 CLAUDE.md
「零新增依赖是硬约束，不要再引 ureq/reqwest」一致——更新模块原样保留，不换插件。

## 3. 目标架构

```
inputswitch/
├─ Cargo.toml                  # 根 crate 保持为纯逻辑 lib（无 GUI 依赖，比现在更干净）
├─ src/                        # 核心库不动（kvm / ddc / switcher / transfer / pair / update…）
│  └─ panel.rs                 # 保留：HTTP API（40 端点）+ 托管 Vite 构建产物（新增静态路由）
├─ src/main.rs                 # inputswitch-cli 入口不动
├─ desktop/                    # 新增：前端 + Tauri 壳（布局对齐 cc-bridge）
│  ├─ package.json             # React 18 / TS / Vite 6 / Tailwind v4（可选）/ vitest / eslint
│  ├─ vite.config.ts           # dev server + /api 代理到 127.0.0.1:47833
│  ├─ index.html
│  ├─ src/
│  │  ├─ main.tsx / App.tsx
│  │  ├─ components/           # workspace / devices / transfers / settings / update / ui
│  │  ├─ hooks/                # useSnapshot(400ms 轮询) / useTheme / useTransferTasks…
│  │  ├─ lib/                  # api.ts(端点封装) / format.ts / esc.ts / theme.ts
│  │  └─ styles/               # 现 style.css 的设计令牌 + 组件样式原样迁入
│  ├─ scripts/
│  │  ├─ sync-version.mjs      # 版本同步：根 Cargo.toml → src-tauri/Cargo.toml + package.json + tauri.conf.json
│  │  ├─ gen-changelog.mjs     # CHANGELOG.md → changelog.generated.ts（更新弹窗数据源）
│  │  └─ check-component-size.mjs   # ≤300 行 + useState/useEffect ≤8，棘轮（对齐 cc-bridge）
│  └─ src-tauri/               # 新增：Tauri 壳 crate（path 依赖根 lib）
│     ├─ Cargo.toml            # tauri(wry) + 5 个小插件，无 updater
│     ├─ tauri.conf.json
│     ├─ build.rs
│     ├─ src/main.rs           # Tauri Builder：托盘菜单 / 全局热键 / 面板窗口 / 事件分发
│     └─ capabilities/         # 权限最小化
├─ assets/panel/               # 旧手写面板：P4 完成后删除（先归档一版到 design/ 作对照基线）
├─ assets/panel-dist/          # 新增：Vite 产物拷贝目标（gitignore），panel.rs include 它
├─ CHANGELOG.md                # 新增：结构化（新增/修复/变更/优化/安全 + 更新摘要）
├─ tools/
│  ├─ panelcheck.js            # P4 退休（铁律迁 vitest）
│  ├─ check-size.js            # 保留：扫描范围改为 src/ Rust 棘轮；前端交给 check-component-size
│  └─ copy-panel-dist.mjs      # 新增：desktop/dist → assets/panel-dist
├─ .githooks/pre-push          # 新增：七项门禁（对齐 cc-bridge）
├─ build.ps1 / build.sh / cross-build-windows.sh   # 前面加 npm ci && npm run build && copy-dist
└─ .github/workflows/ci.yml    # 加 frontend job；release.yml 的 tag 构建加前端链
```

**数据流**：React 前端由 `panel.rs` 的 HTTP 服务同源托管（`/` → dist/index.html，
`/assets/*` → 哈希产物），`fetch('/api/...')` 与现在完全一致。Tauri 窗口加载
`http://127.0.0.1:{port}/`（就是现在 `panelwin.rs` 干的事），WebView2/WKWebView 加载本机
http 页面已在现网工作，零 CORS、零 IPC 重写。浏览器退路（面板开不出来时）原样保留。

### 关键设计决策

| # | 决策点 | 选择 | 理由 / 备选 |
|---|---|---|---|
| D1 | 后端 API 形态 | **保留 loopback HTTP，前端 fetch 同源** | panel.rs 文档注释写明的设计核心：CLI/脚本/手机复用 + 退路。备选「改 tauri commands」要重写 40 端点且丢复用，仅形态对齐，不做 |
| D2 | SPA 托管 | panel.rs include `desktop/dist` 拷贝件 | 同源零 CORS；构建顺序 npm build 先于 cargo build，写进 build 脚本与 CI |
| D3 | Tauri 插件 | dialog / clipboard-manager / opener / process / global-shortcut 可用；**updater 禁用** | 探针实证（§2）。更新继续自研 curl+NSIS |
| D4 | 样式方案 | **CSS 原样迁**（design tokens + 组件类），Tailwind 作后续可选 | 1008 行 style.css 重写成 utility 是视觉回归高风险、低收益；cc-bridge 对齐重点是工程体系不是 Tailwind |
| D5 | Linux | 不变：CI 纯库编译 + 运行时浏览器访问托管的 SPA | Tauri Linux 要 WebKitGTK，src-tauri crate 只编 win/mac |
| D6 | 托盘事件模型 | tao EventLoop + `Ui` 的 `on_*` 方法 → Tauri run loop + menu/tray 事件回调 | 引擎线程 / snapshot / Action 分发不动；`panelwin.rs` 的三处平台 hack（macOS activateIgnoringOtherApps、fit_screen、fresh 滚动重置）要平移到 Tauri 窗口封装 |
| D7 | 全局热键 | 先试 tauri-plugin-global-shortcut；不行则保留 global-hotkey crate 在独立线程 | kvm/win.rs 的钩子线程自带消息循环，与壳无关，不受影响 |
| D8 | 版本真源 | 根 `Cargo.toml` 一处真源，`sync-version.mjs` 同步另三处（对齐 cc-bridge 机制） | badge-release.svg 也纳入同步 |
| D9 | CHANGELOG | 新增结构化 CHANGELOG.md；更新弹窗 notes 从 git log 攒改为从 CHANGELOG 抽 | 对齐 cc-bridge 的 gen-changelog + updateNotes 模式 |
| D10 | panelcheck 256 条 | 铁律清单迁 vitest（约 30-40 条核心），脚本退休 | 手写 JS 时代的结构断言；React 组件化后大部分由 tsc + 组件测试覆盖 |

## 4. 分期计划

每期结束都可回滚（git 分支 `feat/tauri-react-migration`），每期有明确验收。

### P0 地基（无行为变化）
- `desktop/` 骨架：package.json / vite / tsconfig / eslint / vitest 配置 + 一个 Hello 页面
- `tools/copy-panel-dist.mjs`、`panel.rs` 静态路由（托管 dist，先托管旧面板编译件占位）
- `CHANGELOG.md` 建骨架；`scripts/sync-version.mjs`
- `.githooks/pre-push` 七项（前端项此时是空跑通过）；CI 加 frontend job 空跑
- **验收**：`npm ci && npm run build` 产物被 panel.rs 托管，托盘窗口打开看到新页面（内容还是旧的，用 iframe/跳转先顶替）；pre-push 七项全绿；cargo build/test 不回归

### P1 前端 React 化（后端零改动）
- 按四页拆：`App`（侧栏/顶栏/页脚壳）+ `Workspace` + `Devices` + `Transfers` + `Settings` + `UpdateDialog` + `ui/` 基础件
- `lib/api.ts` 对照 panel.rs 路由清单逐一封装（switch/target/pair/autostart/clipboard/rescan/scan/check/hotkey/ask-peer/send/send-folder/send-test/transfer/inbox/kvm*/submachine/label/link/update…）
- `hooks/useSnapshot`：400ms 轮询 + 窗口不可见停轮询（对齐 cc-bridge 的 useAppHidden 思路）+ 自调度进度条
- `hooks/useTheme`：localStorage 三态 + reduce-motion，防闪脚本迁进 index.html
- 样式：style.css 按页拆进 `styles/`，class 名原样保留，视觉零回归
- panelcheck 铁律逐条迁 vitest（顶栏字面 / mainSwitch 永远显示 / 唯一 primary 按钮 / 三个维护入口 / aria-pressed 同步…）
- **验收**：四页功能与旧面板逐屏对照（`tools/shot-serve.js` 夹具复用）；vitest 覆盖铁律 + 纯函数（human_size/is_newer/相对时间）；tsc/eslint 零告警；旧面板可随时切回（assets/panel 还在）

### P2 Tauri 壳替换 tao
- `desktop/src-tauri`：main.rs 用 `tauri::Builder`；托盘菜单（tray-icon）；WebviewWindow 替代 panelwin.rs；capabilities 最小化
- `tray/ui.rs` 事件模型改写；`tray/menu.rs` 菜单构造改 Tauri menu；`Panel::open/show/hide/fit/fresh` 逻辑平移（含 macOS activate 与 fit_screen）
- 全局热键按 D7 定稿；失败退回浏览器逻辑保留
- 根的 `tray` feature 与 `[[bin]] InputSwitch` 移除，改由 src-tauri crate 产出；`src/panelwin.rs` 删除
- **验收**：真机托盘全功能（开面板/热键/切屏/副机模式）；WebView2 与 WKWebView 双端截图；`cargo build --bins` + Zig 交叉编译仍通过（探针结论复核）；Linux 纯库编译不变

### P3 构建链与门禁切换
- build.ps1 / build.sh / cross-build-windows.sh 加前端三步（npm ci → build → copy-dist）
- ci.yml：frontend job 实跑（体量/eslint/tsc/vitest/vite build）；补 clippy job（fmt 债按§5 处理）
- release.yml：tag 构建加前端链；NSIS 阶段评估从自研 pack-windows.js 切 tauri bundle（可降级为不动）
- check-size.js 扫描范围调整；check-component-size.mjs 接管前端棘轮
- **验收**：三条构建路径本机全过；CI 五面全绿；release.js check 通过

### P4 收尾
- 删 `assets/panel/`（归档到 `design/panel-vanilla-archive/`）；panelcheck.js 退休
- README / CONTRIBUTING.md / CLAUDE.md / docs/manual.md 更新（开发者需 node ≥ 20、构建顺序、门禁清单）
- **验收**：全门禁绿 + 6 张截图 + 用户视觉验收

## 5. 已知债务与冲突处理

| 债务 | 处理 |
|---|---|
| `cargo fmt --check` 46 文件红（存量） | 不在本迁移中全仓 fmt（CLAUDE.md 明令禁止，会砸协作者在飞改动）。CI 只加 clippy；fmt 用「只查本次改动文件」脚本过渡（`git diff --name-only \| xargs rustfmt --check`），债务单独排期 |
| eslint 对面板 JS | 新代码是 TS，直接上 eslint + react-hooks；无 suppressions 存量 |
| 单 binary 体积 | 预计涨 3-6MB（Tauri 运行时 + React 产物）；cc-bridge NSIS 6MB 是先例，release 仍 opt-level=z + LTO + strip，可接受 |
| 协作者环境 | CONTRIBUTING 写明 node 要求与两步构建；Zig 交叉编译链不变 |

## 6. 风险登记

| 风险 | 等级 | 对策 |
|---|---|---|
| Tauri run loop 与托盘 app 行为差异（macOS LSUIElement 抢焦点、隐藏而非关闭、fresh 滚动） | 高 | P2 单分期；三处 hack 逐一平移并真机验证；保留「窗口建不出来退回浏览器」 |
| global-hotkey 消息循环归属 | 中 | D7 双方案，P2 第一天验证，不行即退独立线程方案 |
| React 重写引入视觉/交互回归 | 中 | CSS 与 class 原样迁移；每页新旧截图对照；铁律迁 vitest |
| panelcheck 覆盖损失 | 中 | 铁律清单先逐条建 vitest 再删脚本（P1 内完成，不跳期） |
| 构建顺序耦合（npm 先于 cargo） | 低 | copy-dist 脚本 + 构建脚本三步写死；CI 同序 |
| 开发者/协作者工具链变重 | 低 | 文档明示；目标用户零感知（仍单 exe + 无运行时） |

## 7. 待用户确认的决策点

1. D4 样式方案：CSS 原样迁（推荐）还是直接上 Tailwind v4 重写？
2. D1 确认保留 HTTP 后端（推荐）——若坚持改 tauri commands，P1/P2 工作量约翻倍且丢 CLI 复用，需单独论证。
3. P3 的 NSIS：切 tauri bundle 还是保留自研 pack-windows.js？（建议保留，降低发版管线风险）
4. fmt 存量债：本迁移只过渡不清理，确认？

# Tauri 2 + React/TypeScript 迁移提案（公开讨论版）

> 状态：**待讨论，未决定**。本文把优点、缺点、代价和备选方案摆清楚，供协作者共同决策。
> 执行细案（分期、验收、风险登记）见同目录 [`tauri-react-migration-plan.md`](./tauri-react-migration-plan.md)，
> 本文只回答「要不要做、为什么、代价是什么」。

## 1. 背景

InputSwitch 的控制面板（`assets/panel/`）是手写的原生 HTML + CSS + JS，共 7 个文件约 2900 行，
由 `src/panel.rs` 用 `include_str!` 编译期内嵌，跑在系统 WebView（Windows WebView2 /
macOS WKWebView）里，数据通道是一个只绑 127.0.0.1 的 HTTP 服务。

这套选型是有意为之且当日在职的：目标机器零运行时、单 binary、无 node 工具链、
CLI/脚本可复用同一套 `/api`。它支撑项目走到了 v0.6.90。

提出迁移的动因不是「它坏了」，而是三件累积的事实：

1. **门禁体系追不上手写 JS**。面板目前只有 `panelcheck.js` 的结构断言（256 条），
   没有类型检查、没有逻辑单测、没有 lint；界面铁律靠断言脚本一条条钉，
   新增交互的回归成本随页面增长线性上升。
2. **同技术栈分叉**。维护者的另一个项目 cc-bridge 已使用 Tauri 2 + React/TS，
   两个项目的工具、脚本、经验不能互通。
3. **面板开不出来时的退路是浏览器**，这一点迁移后反而更容易保留（见 §3）。

## 2. 提案内容

- **前端**：`desktop/` 新目录，React 18 + TypeScript + Vite 6 + vitest + eslint，
  按四页（工作台 / 设备与布局 / 文件传输 / 设置）拆组件，公共纯函数进 `lib/`，
  轮询状态进 `hooks/`。样式先原样迁移（design tokens + 组件类），Tailwind 作为后续可选项。
- **桌面壳**：`desktop/src-tauri/` 独立 crate（path 依赖根 lib），Tauri 2 提供托盘菜单、
  窗口、系统对话框、剪贴板等能力，替换现在的裸 tao + wry。
- **后端不动**：`panel.rs` 的约 40 个 `/api/*` 端点、`Snapshot` 序列化形状、`Action`
  分发模型全部保留；React 产物由 `panel.rs` 同源托管，前端 `fetch('/api/...')` 与现在
  完全一致。CLI、脚本、「面板开不出来退回浏览器」的退路原样保留。
- **工程化对齐 cc-bridge**：pre-push 七项门禁（体量 / fmt / eslint / tsc / vitest /
  clippy / test）、CI frontend job、CHANGELOG.md 结构化、版本号脚本同步。
- **自动更新不换**：继续用自研 `update/` 模块（curl + SHA-256 + NSIS）。
  **明确不引 tauri-plugin-updater**，原因见 §4 探针。

## 3. 目标数据流

```
React 前端（desktop/，Vite 构建产物）
   │  fetch('/api/...')，同源
   ▼
panel.rs：127.0.0.1 HTTP 服务（40 端点原样保留 + 托管 SPA 产物）
   │  Action 分发（原样不动）
   ▼
src/ 核心库 97 个文件：kvm / ddc / switcher / transfer / pair / update…（零改动）
```

Tauri 窗口加载 `http://127.0.0.1:{port}/`——这正是现在 `panelwin.rs` 已在做的事，
WebView2/WKWebView 加载本机 http 页面是现网工作路径，零 CORS、零 IPC 重写。

## 4. 可行性探针（2026-10-29 实测）

迁移最大的未知是依赖树。已用空 crate 跑 `cargo tree` 实证：

| 探针 | 结论 |
|---|---|
| `tauri 2`（wry 特性）+ dialog / clipboard-manager / opener / process / global-shortcut 五插件 | **0** 个 aws-lc-rs / ring / reqwest / rustls（共 97 包） |
| `tauri-plugin-updater` | 拉入 reqwest 0.13 + rustls → aws-lc-rs → aws-lc-sys（需 C 工具链），**会打破「macOS 上 Zig 交叉编译 Windows 不需要 C 工具链」的前提**，故明确不用 |

即：**迁 Tauri 可行，唯一禁止项是 updater 插件**——这与项目既有的「零新增依赖」约束一致。

## 5. 优点

| # | 优点 | 说明 |
|---|---|---|
| A1 | 类型安全覆盖前端 | tsc 覆盖 40 个端点的请求/响应形状，`Snapshot` 字段改名类回归编译期即失败 |
| A2 | 回归测试可写 | vitest 可对纯函数（体积格式化、版本比较、相对时间）和组件交互（铁律：主按钮永在、唯一蓝按钮、aria 同步）建真实单测，比 DOM 桩断言更强也更易维护 |
| A3 | 开发体验 | Vite HMR 改 UI 秒见效果；现在改面板 JS 也要 `cargo build` 全量重编 |
| A4 | 协作者入门成本降 | React/TS 是主流栈，手写 2900 行全局函数式 JS 需要通读七件套才能上手 |
| A5 | 与 cc-bridge 工具互通 | 体量门禁、eslint 配置、钩子、CI 模式可互相借鉴，维护经验不分裂 |
| A6 | 后端契约反而更稳 | HTTP 原样保留意味着迁移是纯前端替换，`panelcheck` 盖的后端行为零风险 |
| A7 | 生态能力 | Tauri 插件提供对话框/剪贴板/进程管理等系统能力，替代自研平台分支代码 |

## 6. 缺点与代价（诚实清单）

| # | 缺点 | 影响 |
|---|---|---|
| D1 | 协作者必须装 node ≥ 20 | 构建从「一条 cargo」变成「npm ci → build → cargo」两步；目标用户零感知，但贡献门槛抬高 |
| D2 | 单 binary 体积预计涨 3–6MB | Tauri 运行时 + React 产物；cc-bridge 的 NSIS 安装包 6MB 是先例，仍是「无运行时」单 exe，但对体积敏感的用户会注意到 |
| D3 | 双轨维护期 | P1 期间新旧面板并存（旧的可随时切回），但意味着两个事实源；迁移必须连续做完，不宜半途停放 |
| D4 | Tauri run loop 与托盘 app 存在行为差异 | macOS LSUIElement 抢焦点、窗口隐藏而非关闭、首次显示滚动重置——`panelwin.rs` 有三处对应 hack 需平移并真机验证，这是 P2 的最大技术风险 |
| D5 | panelcheck 256 条断言要迁移 | 铁律清单需先逐条迁 vitest 再删脚本；漏迁 = 回归防护开洞 |
| D6 | 全局热键消息循环需重新验证 | 现用 global-hotkey crate 挂在 tao 事件循环上，迁 Tauri 后要确认归属（有 tauri-plugin-global-shortcut 备选） |
| D7 | Linux 仍无原生窗口 | 迁移不改变这一点：Linux 继续浏览器访问托管的 SPA（Tauri Linux 依赖 WebKitGTK，不引入） |
| D8 | 存量 fmt 债务仍在 | 46 个文件 `cargo fmt --check` 红的问题本迁移只过渡不清偿（全仓 fmt 会砸协作者在飞改动） |

## 7. 备选方案（未选，但保留）

- **B. 不迁技术栈，只对齐工程化体系**：保留手写 JS + tao/wry，补 pre-push 钩子、CI clippy、
  CHANGELOG、版本同步、eslint(JS)。工作量最小，但 A1–A5 一项都拿不到，且手写 JS 的门禁
  天花板仍在。
- **C. 只补最痛三项**：pre-push + CI clippy + CHANGELOG，其余排队。是 B 的子集。

选 A 的理由：缺点集中在「迁移期的一次性成本」（D3–D6），优点是「长期的每日成本」
（A1–A5）。项目还在活跃开发期，越晚迁双轨期越长。

## 8. 征求意见

请协作者重点回复以下问题（同意/反对+理由，或提出没列到的点）：

1. **总体方向**：接受 React/TS + Tauri 2 迁移（A），还是选 B/C？
2. **协作者环境**：node ≥ 20 工具链对各位是否可接受？构建两步化有无障碍？
3. **样式路线**：现有 1008 行 CSS 原样迁移（视觉零回归），还是趁迁移上 Tailwind？
4. **后端形态**：确认保留 loopback HTTP（前端同源 fetch），还是趁迁移改 tauri commands？
   （改 commands 约 40 个端点重写，且失去 CLI/脚本/退路复用，本文不建议）
5. **双轨期容忍度**：P1 新旧并存阶段多长可接受？
6. **风险优先级**：D4（托盘行为差异）、D5（铁律迁移）之外还有哪些担忧？

## 9. 决策方式

本讨论开放收集意见 → 维护者汇总 → 若方向为 A，按
[执行细案](tauri-react-migration-plan.md) 的 P0–P4 分期开工，每期独立可回滚、
有明确验收；若方向为 B/C，另行出案。**讨论未形成结论前不动代码。**

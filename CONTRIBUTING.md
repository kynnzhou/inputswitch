# 参与开发

## 环境

- Rust stable（`rustup default stable`）
- macOS 上可以开发全部功能；Windows 可本机构建（`build.ps1`），也可在 macOS 上交叉编译（`./cross-build-windows.sh`，Zig 工具链由 `tools/setup-zig.sh` 提供）
- 面板前端是纯手写 JS/CSS，无构建步骤、无 npm 依赖；唯一的检查工具是 Node 脚本

## 构建 / 测试 / 校验

```bash
cargo check --bins                      # 两个二进制都过一遍编译
cargo build --bin inputswitch-cli --no-default-features   # 瘦构建：发版流水线构建的就是这个形态，保持 0 警告
cargo test --lib -- --test-threads=1    # ⚠️ 必须单线程：套件里有 DDC/回环测试，并行会崩
node tools/panelcheck.js assets/panel   # 面板 UI 的离线断言（80 条）
```

**`--test-threads=1` 不是可选项**，并行跑会 SIGABRT，这不是 bug。

CI（`.github/workflows/ci.yml`）在每次 push / PR 上跑同样的检查（Linux 纯库编译、面板断言、macOS 编译+测试、Windows 编译）。PR 绿了再请人 review。

## 协作流程

- 开分支干活，`main` 保持随时可发布；合并走 PR
- commit 信息一行中文说清“做了什么+为什么”，参考 `git log`（如 `v0.6.71 幽灵输入口：对端自报不再当本机证据`）
- 改 UI 前先跑一遍 panelcheck——它断言的不只是渲染，还有多年踩坑沉淀的界面规则

## 这个项目的几条家规

**1. 注释是文档，改代码前先读注释。** 大量常量与顺序背后有实测依据，注释里写着“别随手改”的（如 `MARGIN=1.5`、`REARM_DIST`、心跳间隔的比例关系），动之前先弄明白当初为什么这么定。

**2. 界面铁律**（panelcheck 里有对应断言）：

- 主切换按钮**永远显示**：不能切时换文案 + 灰蓝样式，绝不 disabled / 隐藏
- 全页只有一个实心蓝按钮（顶栏主按钮）
- 顶栏三个维护动作（重新检测 / 扫描 / 检测）平铺，不收进下拉
- 原生托盘菜单是系统控件，无法自定义样式，别提议给它加背景/圆角

**3. 失败要说清楚，不猜、不谎报。** 异步动作回执只能承诺已发出的事（“已请对端交还”不能写成“已收回”）；多处判据必须同源（后端算好的字段，前端不要自己再推一遍）。

**4. 版本号只有一个来源**：`Cargo.toml` 的 `version`。代码读 `crate::VERSION`，`build.sh` 和发版流水线都从这里提取。展示副本 `assets/badge-release.svg` 里的 `vX.Y.Z` 是静态文本，不会自动同步，发版时一起改。发版流程见下。

## 发版

正常发版走 CI：**推 tag，其余自动**。

```bash
# 1. 改 Cargo.toml 的 version + assets/badge-release.svg 里的 vX.Y.Z（两处一起改）
# 2. 提交，走 PR 合进 main，本地切到最新的 main
# 3. 预检（不碰任何东西）：工作区干净 / tag 未占用 / gh 已登录 / 徽章版本一致
node tools/release.js check
# 4. 打 tag 并推送 —— release.yml 自动构建双端、生成 updater.json、建 Release
node tools/release.js tag --yes
```

tag 推上去后 `.github/workflows/release.yml` 自动完成：Windows 打 NSIS 安装器
（`InputSwitch_<版本>_x64-setup.exe`）/ macOS 打 `.app` zip → 用当次产物**现算 SHA-256**
生成 `updater.json` → 建 GitHub Release 并上传。**tag 名必须和 Cargo.toml 的 version 一致**，
不一致流水线直接失败（版本号是机器可读的真源，见第 4 条）。

本地手工发版（workflow 挂了要补包，或就是想本机发）：

```bash
./build.sh                  # macOS：dist/InputSwitch-macOS-arm64.zip
# Windows：先 .\build.ps1，再（需要本机装 NSIS）：
node tools/pack-windows.js  # dist/windows/InputSwitch_<版本>_x64-setup.exe
node tools/gen-updater-json.js dist --notes-file notes.md   # 生成 updater.json
node tools/release.js publish dist --notes-file notes.md --yes
```

`release.js` 所有子命令默认 dry-run，只打印将要执行的命令，`--yes` 才真干。

**自动更新的第一个版本必须让用户手动装一次**：安装形态从「解压即用」变成了安装器，
只有装过安装版的机器上才有自动更新链路。

## 代码地图

[docs/manual.md](docs/manual.md)（完整手册与设计文档）讲了为什么这样设计；模块职责见 `src/lib.rs`。几个体积最大的模块：`tray/`（托盘/热键/事件循环/切屏编排，已拆成 13 个文件）、`kvm/`（软件 KVM：协议、状态机、平台钩子，已拆成 17 个文件）、`panel.rs`（面板 HTTP 服务）。控制面板前端在 `assets/panel/`（骨架/样式/四段 JS，由 `panel.rs` 内嵌并按路由分发）。两处的体量红线见 `tools/check-size.js` 的基线棘轮。

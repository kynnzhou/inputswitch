# 更新日志

格式约定：每版一节，条目分 **新增 / 修复 / 变更 / 优化 / 安全** 五类；`更新摘要` 一段是
面板更新弹窗给用户看的人话（gen 脚本从这里抽，见 desktop/scripts/，P1 接入）。
版本真源是根 `Cargo.toml` 的 `version`（sync-version.mjs 同步到 desktop/package.json
与 assets/badge-release.svg）。

## 0.7.0（2026-10-29）

### 更新摘要

- 控制面板全面翻新为 React 新前端：左侧四页导航（工作台 / 设备与布局 / 文件传输 / 设置），摆位图支持点选放置与拖拽，弹窗体系（帮助 / 排查 / 扫描确认 / 管理输入源 / 更新）全部无障碍化
- 面板引擎真接线：切屏、扫描、配对、共用键鼠生命周期、文件传输等全部操作即点即生效
- 项目源码迁入 Tauri 2 壳架构（desktop/src-tauri），旧版面板保留在 /legacy 可随时回退

### 新增

- desktop/ 前端工程（Vite 6 + React 18 + TypeScript + vitest + eslint），40 个 API 端点全封装、全结构 TS 镜像
- Tauri 2 壳：托盘全量菜单（2.5s 签名刷新）、全局热键（注册/事件/网页改键录制）、面板窗口、文件接收服务
- 改键录制页（/hotkey-recorder）：网页收组合键，两端通用
- CI frontend job、pre-push 七项门禁、CHANGELOG、版本同步脚本

### 变更

- 默认面板 / 切换为 React 新前端；旧手写面板挪到 /legacy（构建产物缺失时自动回退）
- build.sh / cross-build-windows.sh / release.yml 加入前端三步（npm ci → build → copy-dist）

## 0.6.90（2026-10-27）

### 更新摘要

- 新增**自动更新**：程序自己检查新版本、下载、倒计时重启安装（Windows 用 NSIS 安装器，macOS 替换 .app）。**从这个版本起，以后的更新全自动，本版需要手动安装一次**
- 代码结构大整理：巨型源文件拆成目录模块，对外行为零变化
- 发布流水线自动化：推版本标签，CI 自动构建双平台安装包并发 Release

### 新增

- 自动更新 P0–P4：`update` 模块（检查/下载/SHA-256 校验/安装/8 秒倒计时重启）、
  面板顶栏更新按钮与进度弹窗、CLI `update --install`、NSIS 安装器（免管理员、
  卸载保留配置）、`release.yml` 发布流水线、`updater.json` 清单
- `main.rs` / `config` / `switcher` / `cli/` 等巨型文件目录模块化 + `tools/check-size.js` 体量棘轮

### 修复

- 推边进门后光标漂移到出门方向那一侧（锚点初值取了发送端屏幕比例，v0.6.67 引入）

## 0.6.89（2026-10-25）

### 更新摘要

- 面板界面全面翻新：顶部四个事实 + 主按钮、按设计稿对齐的视觉、四页结构雏形
- 图标换为 "is" 品牌字标（Nunito 真实轮廓，小尺寸逐像素校准）
- 项目开源（MIT），macOS / Windows 双平台安装包挂 GitHub Release

### 新增

- MIT LICENSE、CI（四检查面）、CONTRIBUTING、GitHub Discussions
- Windows 托盘图标与 exe 图标（手写 COFF 资源对象，交叉编译可用）

### 变更

- 面板源码从单文件 HTML 拆为 `assets/panel/` 六件，`panel.rs` 按路由内嵌
- 界面文案全面重写为用户视角；「去网页感」七处修正

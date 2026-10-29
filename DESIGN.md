---
name: "InputSwitch 交互原型"
description: "一体化控制台：在紧凑窗口内看清画面、键鼠归属和下一步操作。"
colors:
  accent: "#3561df"
  accent-hover: "#294fc0"
  accent-soft: "#edf2ff"
  accent-ink: "#3157bd"
  white: "#fff"
  surface: "#ffffff"
  subtle: "#f6f7f9"
  canvas: "#f9fafc"
  sidebar: "#f4f5f8"
  ink: "#232b3b"
  muted: "#667085"
  line: "#e5e8ee"
  line-strong: "#cbd2de"
  success: "#1c7656"
  success-soft: "#eaf6ef"
  warning: "#92601e"
  warning-soft: "#fff5e5"
  danger: "#bc4050"
  danger-soft: "#fff0f1"
  dark-accent: "#4268d4"
  dark-accent-hover: "#496fdc"
  dark-accent-soft: "#283754"
  dark-accent-ink: "#abc0ff"
  dark-surface: "#20252f"
  dark-subtle: "#292f3a"
  dark-canvas: "#1c212b"
  dark-sidebar: "#1b2029"
  dark-ink: "#e9edf5"
  dark-muted: "#a2adbf"
  dark-line: "#353d4b"
  dark-line-strong: "#515e73"
  dark-success: "#7fd0ab"
  dark-success-soft: "#233a32"
  dark-warning: "#ebbf7b"
  dark-warning-soft: "#3c3327"
  dark-danger: "#ff9caa"
  dark-danger-soft: "#422c35"
typography:
  headline:
    fontFamily: '"Segoe UI", "Microsoft YaHei UI", "PingFang SC", system-ui, sans-serif'
    fontSize: "24px"
    fontWeight: 650
    lineHeight: 1.35
    letterSpacing: "-.03em"
  title:
    fontFamily: '"Segoe UI", "Microsoft YaHei UI", "PingFang SC", system-ui, sans-serif'
    fontSize: "14px"
    fontWeight: 600
    lineHeight: 1.5
  body:
    fontFamily: '"Segoe UI", "Microsoft YaHei UI", "PingFang SC", system-ui, sans-serif'
    fontSize: "13px"
    fontWeight: 400
    lineHeight: 1.5
  control:
    fontFamily: '"Segoe UI", "Microsoft YaHei UI", "PingFang SC", system-ui, sans-serif'
    fontSize: "12px"
    fontWeight: 400
    lineHeight: 1.5
  label:
    fontFamily: '"Segoe UI", "Microsoft YaHei UI", "PingFang SC", system-ui, sans-serif'
    fontSize: "11px"
    fontWeight: 400
    lineHeight: 1.5
  tag:
    fontFamily: '"Segoe UI", "Microsoft YaHei UI", "PingFang SC", system-ui, sans-serif'
    fontSize: "11px"
    fontWeight: 500
    lineHeight: 1.6
  keyboard:
    fontFamily: '"Segoe UI", "Microsoft YaHei UI", sans-serif'
    fontSize: "11px"
    fontWeight: 400
    lineHeight: 1.5
rounded:
  keyboard: "4px"
  tag: "5px"
  field: "6px"
  control: "7px"
  group: "8px"
  toast: "9px"
  panel: "12px"
  dialog: "14px"
  toggle: "20px"
  circle: "50%"
spacing:
  space-4: "4px"
  space-6: "6px"
  space-7: "7px"
  space-8: "8px"
  space-10: "10px"
  space-12: "12px"
  space-16: "16px"
  space-24: "24px"
components:
  button-primary:
    backgroundColor: "{colors.accent}"
    textColor: "{colors.white}"
    rounded: "{rounded.control}"
    padding: "7px 16px"
  button-primary-hover:
    backgroundColor: "{colors.accent-hover}"
  button-primary-unavailable:
    backgroundColor: "{colors.accent-soft}"
    textColor: "{colors.accent-ink}"
  button-secondary:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.ink}"
    typography: "{typography.body}"
    rounded: "{rounded.control}"
    padding: "5px 12px"
  button-strong:
    backgroundColor: "{colors.ink}"
    textColor: "{colors.surface}"
    rounded: "{rounded.control}"
    padding: "5px 12px"
  button-maintenance:
    backgroundColor: "transparent"
    textColor: "{colors.muted}"
    typography: "{typography.label}"
    rounded: "{rounded.control}"
    padding: "3px 8px"
  button-text:
    backgroundColor: "transparent"
    textColor: "{colors.muted}"
    rounded: "{rounded.control}"
    padding: "5px 4px"
  input:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.ink}"
    typography: "{typography.body}"
    rounded: "{rounded.field}"
    padding: "6px 10px"
  navigation:
    backgroundColor: "transparent"
    textColor: "{colors.muted}"
    rounded: "{rounded.control}"
    padding: "9px 12px"
  navigation-selected:
    backgroundColor: "{colors.accent-soft}"
    textColor: "{colors.accent-ink}"
  tag:
    backgroundColor: "{colors.subtle}"
    textColor: "{colors.muted}"
    typography: "{typography.tag}"
    rounded: "{rounded.tag}"
    padding: "2px 7px"
  tag-good:
    backgroundColor: "{colors.success-soft}"
    textColor: "{colors.success}"
  tag-blue:
    backgroundColor: "{colors.accent-soft}"
    textColor: "{colors.accent-ink}"
  tag-warn:
    backgroundColor: "{colors.warning-soft}"
    textColor: "{colors.warning}"
  scene-panel:
    backgroundColor: "{colors.canvas}"
    textColor: "{colors.ink}"
  segmented:
    backgroundColor: "{colors.subtle}"
    rounded: "{rounded.group}"
    padding: "3px"
  segmented-option:
    backgroundColor: "transparent"
    textColor: "{colors.muted}"
    typography: "{typography.control}"
    rounded: "{rounded.tag}"
    padding: "4px 12px"
  segmented-selected:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.ink}"
  toggle:
    backgroundColor: "{colors.line-strong}"
    rounded: "{rounded.toggle}"
    padding: "3px"
    width: "34px"
    height: "20px"
  toggle-on:
    backgroundColor: "{colors.accent}"
  toast:
    backgroundColor: "{colors.ink}"
    textColor: "{colors.surface}"
    rounded: "{rounded.toast}"
    padding: "12px 15px"
  dialog:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.ink}"
    rounded: "{rounded.dialog}"
    padding: "0"
---

# Design System: InputSwitch 交互原型

## Overview

**Creative North Star: "一体化控制台"**

InputSwitch 的交互原型采用简洁、友好的桌面工具外观。标题栏、灰色导航和状态栏组成连续壳体，白色工作区直接衔接其中；画面归属、键鼠归属与下一步操作保持稳定位置，场景内容在壳体内切换。

本规范只适用于 `design/interactive/` 离线 HTML 原型，未表示生产面板已经迁移。设计以已实现的 HTML、CSS、JavaScript 为依据；状态、设备、网络地址和传输均为演示。保留 InputSwitch 名称及 `design/brand/icon.svg`，不引入图像 comp 或新的品牌素材。

**Key Characteristics:**

- 连续壳体、清楚分区、主要功能无需滚动。
- 单屏切换与多屏共享同等可达，共用一套状态与操作区。
- 一处实心蓝主操作，维护与配置使用更轻的按钮。
- 状态通过文字、图标和颜色共同表达，确认回执决定完成反馈。
- 紧凑中文排版、现有 SVG 图标、短促且可减少的动态效果。

## Colors

冷白工作区与安静灰色壳体承载信息；蓝色表示主要操作和当前选择，语义色只承担状态。前言是颜色取值的规范来源，命名对应 `app.css` 的同名自定义属性；`white` 对应代码中复用的固定白色。

### Primary

- `accent` 与 `accent-hover`：主操作、共享开关、焦点轮廓和传输进度。
- `accent-soft` 与 `accent-ink`：选中导航、当前输入源、已选设备和等待标签；浅色底与深色文字成对使用。
- `white`：蓝色按钮文字及开关圆点；深色外观仍使用固定白色。

### Neutral

- `sidebar`：连续标题栏、导航、状态栏和预览底面。
- `surface`：主工作区、输入框、普通按钮及浮层；`canvas`：设备关系区域和传输放置区；`subtle`：次级底面。
- `ink`、`muted`：主要信息与辅助信息；`line`、`line-strong`：分区接缝和可交互边界。

### State and themes

`success` / `success-soft` 用于连接正常与完成；`warning` / `warning-soft` 用于待连接、无可用输入等需要处理的状态；`danger` / `danger-soft` 用于失败与输入错误。状态名和下一步提示应与颜色同时存在。

`dark-*` 是同名 CSS 变量在深色主题中的实际覆盖值，名称只用于文档索引。运行时继续通过 `data-theme` 切换同一组变量，不新增另一套组件。默认浅色，也支持深色与跟随系统。Sidecar 的 tonalRamp 仅是由现有色值派生的色板预览，不是新增 CSS tokens，也不用于替代上述语义配色。

**The One Primary Action Rule.** 未被浮层遮挡的面板仅保留一个实心蓝操作按钮；不可用时主操作改用浅蓝解释态，仍可点击了解原因。共享开关、进度和选中状态可继续使用蓝色。

## Typography

界面使用前言记录的系统字体栈，优先适应 Windows 中文桌面环境；没有额外的装饰性展示字体或外部字体请求。`headline` 用于页面标题，`title` 用于分区，`body` 用于基础正文和普通按钮，`control` 用于紧凑操作，`label` 与 `tag` 用于辅助说明和状态，`keyboard` 用于快捷键。

字阶取自实际界面，不假设数学比例。标题在窄窗口与矮窗口分别缩为（21px）和（20px）；标签不继续缩小，空间不足时优先隐藏辅助句、缩减间距或切换布局。

**The Useful Hierarchy Rule.** 先让设备名、当前归属和操作文本清楚，再用弱色安排说明；不要把说明压缩成难以辨认的小字来容纳功能。

## Layout

默认应用铺满视口（宽 100%、高 100dvh），最小桌面高度为（520px）。壳体是两列三行网格：导航（178px），工作区占剩余宽度；标题栏（49px）与状态栏（32px）横贯全宽。白色工作区只有上、左接缝和左上圆角，壳体本身不作为悬浮卡片。

工作区依次放置常驻两行命令区、当前页面和底部状态。命令区上行是画面/键鼠归属与主操作，下行平铺“重新检测”“扫描输入口”“检测能否切换”。常态命令区内边距为（17px 24px 10px），页面容器为（23px 26px 18px）。前言间距只收录复用值，不强行把现有节奏改成等差网格。

| 条件 | 已实现行为 |
| --- | --- |
| 应用容器宽度 ≤ 950px | 导航收为 65px 图标栏；保留导航标题提示与当前页状态。 |
| 应用容器宽度 ≤ 720px | 缩减横向间距、主按钮与设备图；隐藏重复连接事实和顶栏热键提示。 |
| 应用容器高度 ≤ 620px | 降低设备图、传输区和设置行的密度，保留主要操作。 |
| 应用容器高度 ≤ 550px | 隐藏页面副标题与部分辅助说明，进一步压缩图示及行距。 |
| 应用容器宽度 ≤ 520px | 命令区与场景选择可换行；主按钮占满该行；设备列表改为横排。 |
| 浏览器宽度 ≤ 599px | 窄预览最小高度调整为 540px，隐藏设计稿角标。 |

主体容器保留溢出滚动作为保护；“无需滚动”是已验证桌面尺寸内的验收结果，不代表任意尺寸、缩放或新增内容下都不会滚动。传输列表通过分页适应高度。右上角“设计预览”通过 popover 展示演示状态、窗口尺寸、主题和重置，默认不占据工作区。

验收在本地完成（`node design/interactive/verify.cjs`，报告写入本地 `.impeccable/review/`，属本地验收资产、不随仓库发布）：19 项交互与 34 组布局通过，无记录的越界；包括 1080 × 720、820 × 680、600 × 520 及窄预览、深色场景。生产默认尺寸仍以现有产品实现为准。

**The Integrated Shell Rule.** 标题栏、导航和状态栏保持连续底色，工作区直接衔接；不要增加外围画布、悬浮窗口边框或常驻的第二条设计工具栏。

## Elevation & Depth

大区域依靠底色差、细线和位置关系分层。设备关系面板仅保留上下边界，列表与设置使用平铺行；不为每个信息块添加卡片阴影。

实际阴影收录于 sidecar：`overlay` 用于对话框、设计预览和提示条；`segmented-selection` 用于场景选项的轻微选中层；`toggle-thumb` 用于开关圆点。CSS 中残留但未被当前壳体引用的 `window-shadow` 不构成设计规范。

**The Structural Depth Rule.** 常驻区域以接缝和底色区分，悬浮阴影服务于临时覆盖内容；选项与开关保留已实现的微小层次。

## Shapes

圆角按作用区分：前言 `control`、`field` 与 `tag` 分别约束按钮、输入框和标签；`group` 用于分段选择及显示器示意；`panel` 用于放置区、预览浮层及工作区左上衔接；`dialog` 和 `toast` 用于覆盖反馈。开关用胶囊轨道，状态点和拇指使用圆形。

设备、输入源和布局位置由轮廓 SVG、实线或虚线边界表达。通用图标为（18px）线性 SVG，描边（1.7），尺寸随组件调整；品牌图标直接复用仓库矢量资产。虚线表示可放置区域，实体设备使用实线，选中态加蓝色边界和浅蓝底。

## Components

### Buttons

主操作始终容易找到。基础按钮的圆角、颜色和内边距取自前言；主操作常态最小高度（37px）、最小宽度（170px），窄/矮容器按现有规则收紧。悬停改变背景与边界，按下位移（1px）；忙碌用旋转图标和 `aria-busy`，真正禁用的次级操作降低透明度。不可用主操作保留解释入口。

普通描边按钮承担配置与辅助动作；石墨色强按钮用于“保存并连接”和对话框确认；维护按钮以细边框和弱文字平铺；文字按钮与图标按钮用于管理、帮助、关闭等轻量动作。尺寸变体只缩减现有控件，不创造新的操作层级。

### Inputs / Fields

输入框与选择框使用 `line-strong` 边界、`surface` 底色和 `field` 圆角，基础最小高度为（34px）。配对错误说明位于输入框下方并使用 `danger`；等待对端确认时输入只读，保存按钮显示等待状态。不要把占位文本作为唯一标签。

### Navigation and scene selector

导航常态为弱文字，悬停加灰底，当前页使用浅蓝底、蓝色文字和字重，并标注 `aria-current`。紧凑导航保留 SVG、标题提示和操作能力。

“单屏切换”和“多屏共享”使用相邻、同等权重的分段选项，白色选中面与轻阴影表达当前视图；按钮保留 `aria-pressed`。两者仅切换工作台视图，每条连接的摆位决定其共享方式，共用一块屏与四向相邻设备可同时存在。

### Status tags and feedback

标签使用前言四种样式表达普通、连接正常、当前/等待和警告。提示条位于状态栏上方，提供关闭，移动操作可提供撤销；普通反馈通过 `aria-live` 宣告，错误可使用 `role="alert"`。等待反馈使用忙碌图标，完成反馈才使用完成图标。

**The Confirmed State Rule.** 发送请求、等待确认和确认完成必须分开呈现；模拟确认到达后才更新画面归属，并在共享条件成立时交接键鼠。失败保持此前归属，不提前报成功。

### Device diagram, sharing and transfers

单屏图显示两台设备、连接线和输入源；当前设备同时具备边界、文字与标签提示。多屏图与设备页复用四向拓扑：点击选择与放置、拖动、方向键移动和撤销均可用。无输入信号的具体输入源禁用并解释原因。

共享开关使用 `role="switch"`、`aria-checked`、明确名称和移动圆点；工作台与设置共用同一偏好。传输放置区使用虚线边界，拖入时变为强调色；传输行呈现文件、进度/结果和对应操作，更新进度保留操作焦点。

### Overlays, motion and keyboard

对话框使用原生 dialog，关闭后返回触发控件；“设计预览”使用原生 popover，不是生产设置。常规状态变化为（160–220ms），连线信号只在等待切换时播放，旋转图标只用于忙碌；具体阴影、时长、缓动与断点位于 sidecar。系统减少动态效果或原型内相应设置均会缩短动画和过渡，不删除文字反馈。

交互控件使用强调色焦点轮廓（2px，外偏移 3px），场景及设置重绘保留触发焦点。紧急断开保留 `Ctrl + Alt + Esc`，在对话框、输入编辑和快捷键录制中优先响应；迟到的显示器确认不能重新交走已释放的键鼠。底栏相关按钮只展示帮助，不模拟可点击的鼠标夺回操作。

## Do's and Don'ts

### Do:

- **Do** 将本规范用于 `design/interactive/`，以实际 HTML、CSS 和交互状态作为依据。
- **Do** 保留连续壳体、常驻归属信息、一个主操作和三项平铺维护入口。
- **Do** 让单屏切换与多屏共享同等可达，按每条连接的摆位解释共享方式。
- **Do** 在确认后才呈现完成；等待、失败和不可用均提供明确文字反馈。
- **Do** 保留紧急快捷键、可见焦点、减少动态效果和现有 SVG 品牌图标。
- **Do** 在主要桌面尺寸检查完整操作区，特别是 600 × 520 的宽度与高度组合。

### Don't:

- **Don't** 声称生产面板已经采用这套原型规范，或把演示数据当作真实设备状态。
- **Don't** 把场景选择改成全局共享模式，覆盖已有的逐连接布局。
- **Don't** 用隐藏或禁用主操作代替不可用解释，也不要折叠三项维护入口。
- **Don't** 提前更新归属、用成功图标表示等待，或让迟到确认覆盖紧急断开。
- **Don't** 添加外围展示画布、悬浮壳体或重复的常驻预览工具栏。
- **Don't** 把未应用的旧窗口阴影、派生色板或示意设备名称当作新增产品规范。

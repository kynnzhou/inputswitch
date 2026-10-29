# InputSwitch UI 设计稿交接

更新日期：2026-10-28（Asia/Shanghai）  
项目目录：D:\AItool\winapp\inputswitch  
当前分支：codex/ui-layout-prototype

## 1. 接手说明

请接着现有成果完成交付，不要从零重做。用户最初要求全面优化软件 UI，并交付可交互设计稿；随后明确要求“壳体一体化”，已授权实施。最后要求生成本文件，交给另一个 AI 工具继续完成。

**当前状态：交互原型主体已经实现，一体化调整和已发现的问题已经通过复核；设计规范文档一致性核验已由接手方于 2026-10-28 补做完成（295 项逐条核对、0 项不一致，方法与结论见第 7.2 节），verify.cjs 与 panelcheck 均已复跑通过。用户尚未给出最终视觉验收结论。**

本次交付范围是独立的交互设计原型。生产面板、Rust 后端、系统配置、可执行程序和版本号均未改动。后续如无新的生产接入要求，继续完善并交付原型即可。

## 2. 用户已确认的要求

- 简洁、布局清晰、交互友好，有明确反馈和适量动画。
- 正常桌面窗口内，主要功能尽量无需滚动即可看清。
- 单屏切换、多屏共享同等重要，首页可以切换场景。
- 壳体一体化：标题栏、导航、状态栏连续统一，内容直接衔接窗口。
- 已多次明确“继续”“开工”，不用再次询问上述方向或重复规划。

## 3. 预览入口

- 正在使用的预览：http://127.0.0.1:8765/interactive/
- 本次交接前已检查该地址返回 HTTP 200；接手时应再确认服务是否仍在运行。
- 可以直接打开本地文件：D:\AItool\winapp\inputswitch\design\interactive\index.html
- 页面默认铺满预览窗口。右上角“设计预览”可展开演示状态、窗口尺寸、主题和重置。
- 当前浏览器标签页可能仍显示旧画面，需要刷新。
- 可选参数：?scene=multi、?page=devices、?page=transfers、?page=settings、?state=offline、?theme=dark。
- ?embed=1 隐藏“设计预览”入口，以浏览器视口直接模拟应用窗口。

这是离线原型。切屏、配对、文件传输和系统设置均为模拟；选择文件只读取文件名、大小等元数据，不读取文件内容，不发送文件。

## 4. 关键文件

以下路径均相对于项目根目录。

| 文件 | 用途 |
| --- | --- |
| design/interactive/index.html | 原型入口、应用壳体、导航、原生对话框、设计预览入口、SVG 图标 |
| design/interactive/app.css | 颜色、字体、布局、紧凑窗口适配、浅深色和动画 |
| design/interactive/app.js | 演示状态、各页面渲染、交互、异步反馈、快捷键和原型偏好 |
| design/interactive/verify.cjs | 浏览器交互与布局验收，可生成截图 |
| design/interactive/README.md | 原型体验方式、范围与运行说明 |
| design/README.md | 新原型入口；其后仍保留早期设计稿的历史说明 |
| PRODUCT.md | 产品能力、用户需求与约束 |
| DESIGN.md | 已生成的原型设计规范，含 YAML tokens |
| .impeccable/（本地验收资产，不随仓库发布：由 verify.cjs 本地生成的截图与报告） | 方向约定、验收结果等过程证据留存于本地工作区 |
| design/brand/icon.svg | 原型复用的现有品牌图标 |

生产界面与行为的参考入口是 CONTRIBUTING.md、CLAUDE.md、assets/panel/、src/panelwin.rs。开始编辑前先读项目约定；不要把早期设计稿或生产旧样式误当作本次新原型的视觉规范。

## 5. 当前已经实现的内容

### 一体化壳体

- 去掉外围展示画布、悬浮窗口和独立的常驻原型工具栏。
- 应用铺满视口，标题栏、侧栏、底部状态栏共享连续底色。
- 内容区域直接接入壳体，使用少量接缝与底色区分层次。
- 主切换操作与三项维护操作常驻。
- 小窗口导航收为图标栏；设计工具收进标题栏的“设计预览”。

### 工作台

- 单屏切换和多屏共享等权呈现。
- 切换工作台场景只切换视图，不改变每台设备的布局。
- 切换过程区分请求、等待确认、确认完成；失败保留原归属。
- 画面归属、键鼠归属、键鼠和剪贴板开关、活动反馈可见。
- 可体验模拟鼠标跨屏，以及离线、未配对、失败、无输入源等状态。

### 设备、传输与设置

- 配对输入校验、配对码、等待验证、配对结果。
- 设备点击选择、点击摆位、拖动、方向键移动、Delete 移出布局和撤销。
- 共用一块屏与四向邻接连接可并存。
- 文件/文件夹选择、拖放、演示传输、进度、暂停、继续、取消、重试、筛选与分页。
- 快捷键录制、共享偏好、光标速度、开机自启、恢复、守护、副机模式。
- 浅色、深色、跟随系统及减少动态效果。
- 帮助、输入源重命名、接收目录示例、日志和模拟更新反馈。
- 偏好保存于原型自己的浏览器存储。

### 键盘与反馈

- Ctrl + Alt + Esc 在弹窗、文本输入和快捷键录制期间仍优先响应。
- 紧急断开后，迟到的切换确认不能重新交走键鼠。
- 场景、设置切换与传输进度刷新保留对应操作焦点。
- 等待状态使用忙碌图标，完成后才使用成功图标。
- 普通过渡约 160–220ms；进度使用 transform，支持减少动态效果。

## 6. 已完成的验证与复核

验收由本地 `node design/interactive/verify.cjs` 完成，记录在本地 `.impeccable/review/acceptance.json`（不随仓库发布）：

- 19 项浏览器交互检查通过。
- 34 组布局组合通过。
- 覆盖 1080 × 720、820 × 680、600 × 520、390 × 844，以及单屏、多屏、离线、配对、设置分区和深色。
- 记录中无内容溢出、主要操作越界、脚本错误或生产 API 请求。
- 生产面板既有检查此前运行，80 条断言通过；本次没有修改生产面板。
- 最近一次 git diff --check 通过。

独立复核的最终结论为 **ship**。其结论范围是以下五项修复和新增的一体化壳体调整，不代表生产集成或真实硬件行为已获验证：

| 原发现 | 当前结果 |
| --- | --- |
| 600 × 520 多屏说明与底部动作重叠 | 已解决，矮窗口隐藏重复说明，保留底部跨屏动作 |
| 弹窗/输入状态挡住紧急快捷键 | 已解决，紧急处理优先，并保留草稿及可见反馈 |
| 场景、设置和传输更新丢失焦点 | 已解决，恢复触发控件或传输操作焦点 |
| 维护请求等待时误用成功图标 | 已解决，等待使用旋转图标 |
| 浅色成功标签对比度不足 | 已解决，success 为 #1c7656，对 #eaf6ef 约 5.01:1 |

本轮截图均在上述修复和壳体调整之后生成，并已打开检查：

- .impeccable/review/desktop.png
- .impeccable/review/1080-workspace.png
- .impeccable/review/1080-multi.png
- .impeccable/review/1080-devices.png
- .impeccable/review/1080-transfers.png
- .impeccable/review/1080-settings.png
- .impeccable/review/600-workspace.png
- .impeccable/review/600-multi.png
- .impeccable/review/600-devices.png
- .impeccable/review/600-transfers.png
- .impeccable/review/600-settings.png
- .impeccable/review/390-workspace.png
- .impeccable/review/390-multi.png
- .impeccable/review/dark-workspace.png
- .impeccable/review/preview-tools.png

不要把历史 detector 中已经修复的字号、进度动画问题再次当作未解决问题。已经完成过两轮界面检查和修复复核；若没有新反馈或新修改，不需要重启全量设计探索或反复补测。

2026-10-28 接手方复跑 verify.cjs（无 `--capture`，未重生成截图、未覆盖 acceptance.json）：19 项交互与 34 组布局全部通过，与上述记录一致；另跑 `node tools/panelcheck.js assets/panel`，80 条断言通过（本次未修改生产面板）。

## 7. 接手后实际需要做的事

1. **查看现有成果。** 打开预览，重点看一体化壳体、600 × 520 单屏/多屏、设备摆位与文件传输。以当前源件和新截图为准。
2. **完成设计规范核验（2026-10-28 已补做，结论：0 项不一致，文档无需修正）。** DESIGN.md 和 .impeccable/design.json 已写入；本次已读取 Markdown、成功解析 JSON，并确认 10 个组件都有 HTML/CSS。文档代理在最终核验时遇到调用额度限制，因此完整 YAML、token 引用与源 CSS 一致性核验没有最终完成报告。
   接手方补做方式：一次性核验脚本（未入库）逐条对照源码，共 295 项全部通过——前言 37 个颜色 token、30 条字阶、10 个圆角、8 个间距、73 条组件属性（`{colors.*}` / `{rounded.*}` 引用先解析再与 `app.css` 真实声明比对）；design.json 96 项（schema、10 个组件的 HTML/CSS 完整性、`refersTo` 与前言的对应、colorMeta / typographyMeta 与前言的对应、阴影 / 动效 / 断点值，以及 10 个组件的全部 CSS 声明逐条比对）；DESIGN.md 正文 37 条断言（壳体 178 / 49 / 32px 网格、520px 最小高度、命令区与页面容器内边距、37px / 170px 主操作、断点 950 / 720 / 620 / 550 / 520 / 599、焦点环、图标规格、紧急快捷键优先、迟到确认不交走键鼠、HDMI 禁用解释、传输分页等）；acceptance.json 记录核对（19 项交互、34 组布局、无脚本错误、无生产 API 请求）。
   核验中仅 5 条生成器归一声明以白名单放行（语义等价，均有人工理由）：`font` 简写（源码为 font-size / line-height / font-family 分开写）、`margin:6px 0 0`（源码 margin-top:6px 加全局 p 重置）、`width:178px`（源码由 grid 列提供）、`align-self:flex-end`（组件示例静态展示补全）。后续如再核验，按同一判据复跑即可。
3. **按具体反馈完善。** 用户尚未确认最终视觉验收。如后续提出新的视觉或交互反馈，就在现有原型上修改；没有额外反馈时，完成文档核验和交付即可，不自行扩展功能。
4. **如修改 UI，验证受影响的行为和窗口尺寸。** 沿用现有验收脚本；有视觉变更再更新对应截图，检查主要内容是否完整显示。
5. **交付预览与说明。** 告知用户刷新现有页面，明确交付仍为交互原型，并说明实际验证范围。

当前没有已知且尚未修复的阻断性 UI 问题。需要接手完成的是核验、可能的新反馈调整和最终交付，不是重新实现上文已经可用的功能。

## 8. 运行与验证命令

在 PowerShell 中进入项目目录：

~~~powershell
Set-Location -LiteralPath 'D:\AItool\winapp\inputswitch'
~~~

如果 8765 端口上的预览已经可用，直接复用。仅当服务未运行时启动：

~~~powershell
python -m http.server 8765 --bind 127.0.0.1 --directory 'D:\AItool\winapp\inputswitch\design'
~~~

本机可用的 Node.js：D:\AItool\nodejs\node.exe。已有 Playwright 包位置如下，不必重新安装：

~~~powershell
$env:INPUTSWITCH_PLAYWRIGHT = 'C:\Users\19145\.cache\codex-runtimes\codex-primary-runtime\dependencies\node\node_modules\playwright'
& 'D:\AItool\nodejs\node.exe' 'design/interactive/verify.cjs' --capture
~~~

该脚本默认直接打开本地 HTML，不依赖 HTTP 服务；Windows 使用已安装的 Edge 无头运行。去掉 --capture 只验证，不生成截图。需要通过 HTTP 验证时，可以设置：

~~~powershell
$env:INPUTSWITCH_PREVIEW_URL = 'http://127.0.0.1:8765/interactive/'
~~~

生产面板既有检查：

~~~powershell
& 'D:\AItool\nodejs\node.exe' 'tools/panelcheck.js' 'assets/panel'
~~~

本次环境中，操控用户现有浏览器的工具返回过“Local browser use is disabled in Work settings”。没有操作该标签页；界面验证使用独立无头浏览器。接手工具应依据自己的可用能力操作，不要沿用一个已被禁止的浏览器控制入口。

## 9. 必须保留的项目约束

- 主切换按钮永远可见且可点击；不可用时解释原因，不能隐藏或禁用。
- 未被浮层遮挡的面板只保留一个实心蓝主操作。
- “重新检测”“扫描输入口”“检测能否切换”三项维护动作平铺可见。
- 共享方式属于每台设备/每条连接，不得改为全局模式。
- 发送请求不等于完成，归属变化必须等待回执。
- 真正的键鼠紧急退出依靠快捷键/本地输入；不能添加误导性的可点击“夺回鼠标”操作。
- 只改本次任务所需内容，保留用户现有改动。
- 未得到用户提交指令，不要 commit 或 push；不要自行打包发布或修改版本号。
- 如果以后确需运行 Rust 测试，必须加 --test-threads=1；本原型收尾无需 Rust 构建。
- DESIGN.md 的规范范围目前仅是 design/interactive/，不代表生产软件已经采用。

## 10. 当前工作区与协作记录

当前成果仍在工作区，尚未提交：

- 修改：design/README.md。
- 新增：design/interactive/、PRODUCT.md、DESIGN.md、.impeccable/ 和本交接文件。
- 分支：codex/ui-layout-prototype。

本次使用过 Impeccable 设计流程：已完成产品背景、方向约定、实现、机械检查、独立复核和文档生成。上下文载入和初始方向探索已经执行，不要因为换工具就重复启动整套流程。

相关技能位置（另一工具无法访问时，以本文件、源代码和项目规范为准）：

C:\Users\19145\.agents\skills\impeccable\SKILL.md  
C:\Users\19145\.agents\skills\impeccable\reference\document.md

## 可直接给接手 AI 的任务

请阅读本交接文件、PRODUCT.md、DESIGN.md、CONTRIBUTING.md 和原型 README，接着当前一体化交互设计稿完成收尾。先核对设计规范与代码的一致性，再根据用户新的具体反馈做必要调整；没有额外反馈时直接完成验证和交付。保留单屏/多屏同等入口、主要功能无需滚动、一体化壳体和完整交互。不要重做已有功能，不要自动修改生产软件、提交或发布。


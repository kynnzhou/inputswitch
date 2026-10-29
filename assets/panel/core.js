// 基础层：共享状态（snap / busy / codeDirty / editingPorts / flashUntil）、
// 取数通道（api / post / pull）、两个小工具（esc / icon）和 P2 起的
// 壳层逻辑（showPage 切页 / updateNavCompact 侧栏宽档 / showSettingsTab
// 设置页选项卡），以及 P2.5 的主题三态与减少动态效果（applyTheme 等，
// 偏好存前端 localStorage）。其余四个脚本都依赖这里的全局变量与函数。
let snap = null;
let busy = false;
/// 用户动过输入框就别再被后台轮询覆盖，否则清空的码会自己弹回来
let codeDirty = false;
/// 正在给某个输入口改名。轮询每 1.2 秒把 #ports 整个重建一次，
/// 重建会换掉 input 节点 —— 用户打两个字输入就没了（实测：聚焦后
/// 打字，等一轮轮询，节点被替换、文字回到旧值）。改名期间跳过那一轮
/// 重建，和 codeDirty 保护配对码输入框是同一个模式。
/// P4 起改名搬进「管理输入源」弹窗（savePorts），这个标志照用。
let editingPorts = false;
/// flash() 的提示要在页面上活满 3.2 秒，而轮询每 1.2 秒会重写那一行。
/// 存截止时间，pull() 到这个点之前不动它。
let flashUntil = 0;

async function api(path, body) {
  const opt = { method: 'POST', headers: { 'Content-Type': 'application/json' } };
  if (body) opt.body = JSON.stringify(body);
  const res = await fetch(path, opt);
  return res.json().catch(() => ({ ok: true }));
}

function post(path) {
  if (busy) return;
  api(path).then(pull);
}

async function pull() {
  try {
    const res = await fetch('/api/state', { cache: 'no-store' });
    snap = await res.json();
    render();
  } catch (e) {
    // 写进场景警示条（P4 起它取代了配对横幅，常驻工作台顶部）
    document.getElementById('sceneAlert').textContent = '控制台与主程序失联了';
    // P2：失联也写进页脚 —— 用户可能停在设备/文件/设置页，
    // 工作台那条横幅他根本看不见，而页脚哪一页都在。
    const fc = document.getElementById('footerConnection');
    if (fc) {
      fc.className = 'footer-connection off';
      fc.textContent = '控制台与主程序失联了';
    }
  }
}

/// 轮询节奏：平时 1.2 秒；下载更新期间临时提到 250ms。
/// 后端下载进度就是每 250ms 采一次样（download.rs 的 TICK），节奏不跟上，
/// 进度条会一跳一跳地走。自调度而不是 setInterval：间隔要随状态变。
function pullInterval() {
  const u = (snap && snap.update) || {};
  return u.phase === 'downloading' ? 250 : 1200;
}

function schedulePull() {
  setTimeout(() => {
    pull();
    schedulePull();
  }, pullInterval());
}

function esc(text) {
  return String(text == null ? '' : text).replace(/[&<>"]/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;'}[c]));
}

/// 图标：<svg class="icon"><use href="#i-名字"/></svg>。
/// symbol 都预埋在 index.html 顶部的 sprite 里，这里只管引用。
/// extra 是附加类名（busy-icon / btn-sm 里的小尺寸等）。
/// 注意：凡是 JS 会重写的按钮文案（主按钮 / KVM 电源 / 切换中…），
/// 都不能用 textContent —— 那会把图标抹掉，必须走 innerHTML + 本助手 + esc()。
function icon(name, extra) {
  return '<svg class="icon' + (extra ? ' ' + extra : '') + '" aria-hidden="true"><use href="#i-' + name + '"/></svg>';
}

/// 一句轻提示（放在顶栏状态行里，默认 3.2 秒后淡出；带动作钮 6.5 秒 ——
/// 要留给用户"读完再点一下"的时间，设计稿 toast undo 同款 6500ms）。
/// 以前这里静默 return，点了没反应 —— 用户分不清"程序坏了"和"确实切不了"。
/// action = { text, fn } 时在提示行里挂一个动作钮：撤销摆位、去配对这类
/// "刚做完的事还能反悔"的场景（设计稿 app.js toast 的 undo 分支）。
/// fn 是闭包，不能走 onclick 字符串，挂到节点上。
function flash(title, sub, action) {
  const line = document.getElementById('status');
  const text = sub ? `${title} · ${sub}` : title;
  line.innerHTML = esc(text) + (action && action.text
    ? ' <button type="button" class="toast-action">' + esc(action.text) + '</button>'
    : '');
  // DOM 桩里 querySelector 返回 null，跳过接线不报错
  const btn = line.querySelector && line.querySelector('button.toast-action');
  if (btn && action && typeof action.fn === 'function') btn.onclick = action.fn;
  line.style.display = '';
  const life = action && action.text ? 6500 : 3200;
  // 轮询 1.2s 一次，会把这一行改回后端状态 —— 记下截止时间让 pull() 让路
  flashUntil = Date.now() + life;
  clearTimeout(flash.timer);
  flash.timer = setTimeout(() => {
    line.innerHTML = '';
    line.style.display = 'none';
    flashUntil = 0;
  }, life);
}

/// ── P2 壳层：四页切换 ──────────────────────────────────────────
/// 四页写死在 index.html 里（workspace / devices / transfers / settings），
/// 同一时刻只显示一页。行为跟设计稿 app.js 的 navigate() 一致：
/// 旧页 hidden、新页显示并放一次入场动画、导航高亮跟过去、
/// aria-current 跟着挪（读屏软件靠它认"当前在哪页"）、内容区回顶部。
/// 取元素走 getElementById 而不是 querySelectorAll 扫 class —— 四页和
/// 四个导航项都有固定 ID，按 ID 取更直白；panelcheck 的 DOM 桩里
/// querySelectorAll 返回空数组，按 ID 才测得着行为。
const PAGES = ['workspace', 'devices', 'transfers', 'settings'];

function showPage(page) {
  if (PAGES.indexOf(page) < 0) page = 'workspace';
  for (const p of PAGES) {
    const view = document.getElementById(p + 'View');
    if (view) view.hidden = p !== page;
    const nav = document.getElementById('nav-' + p);
    if (nav) {
      nav.classList.toggle('selected', p === page);
      if (p === page) nav.setAttribute('aria-current', 'page');
      else nav.removeAttribute('aria-current');
    }
  }
  // 自绘标题栏左侧的页名：跟着导航走。取 nav-item 的 title —— 那是给
  // 图标栏档的悬停提示，正好就是页面全名，不用再维护第二份页面名映射。
  const heading = document.getElementById('titlebarPage');
  if (heading) {
    const nav = document.getElementById('nav-' + page);
    heading.textContent = (nav && nav.title) || '';
  }
  // 入场动画：先摘掉再挂回去，否则连着点同一页不会重播。
  // offsetWidth 强制一次回流，动画才重新起跑（浏览器里的标准做法；
  // DOM 桩里这个属性是 undefined，void 掉不影响）。
  const cur = document.getElementById(page + 'View');
  if (cur) {
    cur.classList.remove('view-enter');
    void cur.offsetWidth;
    cur.classList.add('view-enter');
  }
  // 换页把滚动位置带回顶部：上一页滚到一半，下一页接着那个位置很违和。
  const box = document.getElementById('viewContainer');
  if (box) box.scrollTop = 0;
  // 进设备页按配对态选默认 pane：没配对（或码已填、对端还没验过）的人
  // 第一件事是填码 → 配对表单；已连接的人多半是来摆位的 → 布局。
  // renderSceneAlert 的「去配对」因此自动落到表单上，不用每个入口各写一遍。
  // snap 可能还没拉到（启动首次导航就跑在 pull() 之前）—— 那时不选，
  // 只挂欠账（devicePaneAuto），render() 首跑后 autoDevicePane() 补选。
  if (page === 'devices') {
    if (snap) {
      devicePaneAuto = false;
      showDevicePane(snap.paired && snap.peer_verified > 0 ? 'layout' : 'pair');
    } else {
      devicePaneAuto = true;
    }
  }
}

/// 设备页详情列的两个 pane：'layout'（摆位，默认）/ 'pair'（配对表单）。
/// 页内状态 —— 切页不重置它，只有**进入**设备页时按配对态挑默认的
/// （见 showPage 末尾）。名册列的「连接新设备 / 管理配对」和表单里的
/// 「返回布局」手动切。设计稿 state.devicePane 同款。
/// byUser = 用户手动切的入口（三个 onclick）—— 销掉自动选择的欠账，
/// 之后的轮询 render 不再按配对态抢回用户特意切看的 pane。
let devicePane = 'layout';
let devicePaneAuto = false;

function showDevicePane(pane, byUser) {
  devicePane = pane === 'pair' ? 'pair' : 'layout';
  if (byUser) devicePaneAuto = false;
  const layout = document.getElementById('deviceLayoutPane');
  const pair = document.getElementById('devicePairPane');
  if (layout) layout.hidden = devicePane !== 'layout';
  if (pair) pair.hidden = devicePane !== 'pair';
  if (devicePane === 'pair') {
    // 这个 pane 唯一的动作就是填码：打开就把焦点送进码位
    const code = document.getElementById('code');
    if (code && typeof code.focus === 'function') code.focus({ preventScroll: true });
  }
}

/// pull() 到位后补一次按配对态选 pane。挂在 render() 里调 —— 1.2s 一轮的
/// 轮询每次都过，但欠账一命中就销，实际只跑一次；用户手动切过（已销账）
/// 时静默跳过。修的是时序坑：showPage('devices') 常比首次 pull() 先跑
/// （启动导航、场景警示的「去配对」都是这个顺序），那时 snap 还是 null
/// 选不了 pane，而 pull 完成后没有任何地方补刀 —— 未配对的人打开面板
/// 会停在摆位页，要去填码还得自己找到「管理配对」。
function autoDevicePane() {
  if (!devicePaneAuto || !snap) return;
  devicePaneAuto = false;
  showDevicePane(snap.paired && snap.peer_verified > 0 ? 'layout' : 'pair');
}

/// 侧栏宽档：窗口 < 950px 收成 65px 图标栏（.compact-nav），
/// ≥ 950px 展开成 178px 带文字栏。阈值跟设计稿的容器查询一致。
/// 为什么走 JS 而不是 @container：容器查询的样式改不到容器元素自己
/// 的 grid-template-columns，和设计稿同一机制（它也是 JS 切类）。
/// ResizeObserver 没有的时候（老 WebView2 / 面板自测的 DOM 桩）静默
/// 跳过：默认档就是 820px 窗口的图标栏，行为不破。
function updateNavCompact() {
  const app = document.querySelector('.app');
  if (!app) return;
  const w = app.clientWidth || window.innerWidth || 0;
  app.classList.toggle('compact-nav', w < 950);
}

/// ── P2.5 设置页：三个常驻选项卡 ─────────────────────────────────
/// 快捷键 / 启动与恢复 / 外观与关于。和 showPage 同一个路子：面板写死在
/// index.html 里，切 tab 只换 hidden 和选中态。aria-selected 跟着挪
/// （读屏软件靠它认"当前在哪格"）。共享类控件（共用键鼠、光标速度、
/// 剪贴板同步）与工作台共用后端状态，轮询同时刷新两处开关。
const SETTINGS_TABS = ['shortcut', 'startup', 'appearance'];

function showSettingsTab(tab) {
  if (SETTINGS_TABS.indexOf(tab) < 0) tab = 'shortcut';
  for (const t of SETTINGS_TABS) {
    const panel = document.getElementById(t + 'Panel');
    if (panel) panel.hidden = t !== tab;
    const btn = document.getElementById('tab-' + t);
    if (btn) {
      btn.classList.toggle('selected', t === tab);
      btn.setAttribute('aria-selected', String(t === tab));
    }
  }
}

/// ── P2.5 主题三态 + 减少动态效果 ─────────────────────────────────
/// 后端还没有主题字段，两个偏好都只存前端 localStorage（key 见下）。
/// 默认 'system' = 跟 P2.5 之前的行为完全一致（那时 CSS 直接跟
/// prefers-color-scheme，现在由这里解析后写到 <html data-theme>，
/// 深色令牌挂在 :root[data-theme="dark"] 上）。
/// index.html 的头内脚本负责首屏 paints 前落值防闪；这里管后续变化。
const THEME_KEY = 'inputswitch.panel.theme';
const MOTION_KEY = 'inputswitch.panel.reduce-motion';

function storedTheme() {
  try {
    const v = localStorage.getItem(THEME_KEY);
    return (v === 'light' || v === 'dark' || v === 'system') ? v : 'system';
  } catch (e) { return 'system'; }
}

function systemDark() {
  return typeof matchMedia === 'function'
    && matchMedia('(prefers-color-scheme: dark)').matches;
}

function applyTheme() {
  const t = storedTheme();
  const dark = t === 'dark' || (t === 'system' && systemDark());
  document.documentElement.dataset.theme = dark ? 'dark' : 'light';
  // 分段控件选中态跟当前档位走（系统偏好变化时也要同步）
  ['light', 'dark', 'system'].forEach(v => {
    const b = document.getElementById('themeOpt-' + v);
    if (b) {
      b.classList.toggle('selected', v === t);
      b.setAttribute('aria-pressed', String(v === t));
    }
  });
}

function setTheme(v) {
  if (['light', 'dark', 'system'].indexOf(v) < 0) v = 'system';
  try { localStorage.setItem(THEME_KEY, v); } catch (e) { /* 存不进去也当场生效 */ }
  applyTheme();
}

function applyReduceMotion() {
  let on = false;
  try { on = localStorage.getItem(MOTION_KEY) === '1'; } catch (e) { /* 当没存过 */ }
  document.documentElement.classList.toggle('reduced-motion', on);
  const b = document.getElementById('motionBtn');
  if (b) {
    b.classList.toggle('on', on);
    b.setAttribute('aria-checked', String(on));
  }
}

function toggleReduceMotion() {
  let on = false;
  try { on = localStorage.getItem(MOTION_KEY) === '1'; } catch (e) { /* 当没存过 */ }
  try { localStorage.setItem(MOTION_KEY, on ? '0' : '1'); } catch (e) { /* 存不进去也当场生效 */ }
  applyReduceMotion();
}

/// ── 弹窗可访问性：Esc 关、点遮罩关、开时进框、关时还焦 ──────────
/// 设计稿用原生 <dialog>：showModal() 自带焦点圈禁和 Esc，close 事件里
/// 还焦，点遮罩按 rect 判界（app.js:406-417）。生产是 div 遮罩 —— 轮询
/// 刷新和 panelcheck 的 DOM 桩都围着 style.display 转，换成 <dialog>
/// 要动整套刷新逻辑。不换结构，把这三件事手工补上，行为对齐。
/// modalStack 是栈不是单值：帮助弹窗能转去排查弹窗（先关帮助再开排查），
/// Esc 一次只关最上面那个，关完焦点按打开顺序逐层还回去。
const modalStack = [];

/// 打开遮罩。记下打开者（关闭时还焦），把焦点送进框里 —— 键盘用户和
/// 读屏软件靠它知道"现在在这个对话框里"，不会还在背景页上乱跳。
function openModal(id) {
  const mask = document.getElementById(id);
  // 已经开着就别重复入栈：重复 push 会让 Esc 关一次还剩一层鬼影
  if (!mask || mask.style.display === '') return;
  modalStack.push({ id: id, opener: document.activeElement });
  mask.style.display = '';
  const box = mask.querySelector && mask.querySelector('.modal');
  const first = box && box.querySelector
    ? box.querySelector('button, input, select, textarea, [href]') : null;
  if (first && typeof first.focus === 'function') first.focus({ preventScroll: true });
  else if (box && typeof box.focus === 'function') box.focus({ preventScroll: true });
}

/// 关闭遮罩，焦点还给打开它的那枚钮（节点还在文档树上的话；
/// 打开者已被轮询重画掉就还不了，静默跳过）。
function closeModal(id) {
  const mask = document.getElementById(id);
  if (!mask) return;
  mask.style.display = 'none';
  // 按 id 摘栈，不是无脑 pop 栈顶：旧路数（直接改 display 关的）会在栈里
  // 留下不匹配的残留，pop 错层会把别层 opener 的焦点还到错误的地方。
  let entry = null;
  for (let i = modalStack.length - 1; i >= 0; i--) {
    if (modalStack[i].id === id) { entry = modalStack.splice(i, 1)[0]; break; }
  }
  if (entry && entry.opener && typeof entry.opener.focus === 'function'
    && entry.opener.isConnected !== false) {
    entry.opener.focus({ preventScroll: true });
  }
}

/// 栈顶那个还开着的遮罩。关掉的条目可能残留在栈里（直接改 display 的
/// 老路数），逐层往下找第一个真开着的。
function topModal() {
  for (let i = modalStack.length - 1; i >= 0; i--) {
    const m = document.getElementById(modalStack[i].id);
    if (m && m.style.display !== 'none') return modalStack[i].id;
  }
  return null;
}

// Esc 关最上面的弹窗。没有弹窗开着时直接返回 —— 不抢热键录制里的 Esc
// （录制逻辑自己消费它），也不碰别处对 Escape 的处理。
document.addEventListener('keydown', e => {
  if (!e || e.key !== 'Escape') return;
  const id = topModal();
  if (!id) return;
  e.preventDefault();
  closeModal(id);
});

/* ── 自绘窗口边框 ──────────────────────────────────────────────────
   只有无边框的原生窗口会带 ?frame=none（panelwin.rs 拼上去的）。浏览器里
   打开、桩服务截图时没有这个参数 —— 三键不出现、拖拽热区不挂，因为那里
   根本没有窗口可以动，画出来就是假按钮。

   拖拽和缩放为什么要绕后端一圈：WebView2 是铺满客户区的子窗口，鼠标消息
   全落在它身上，tao 写在顶层窗口里的 WM_NCHITTEST 收不到（探针实测：客户区
   四边的 WindowFromPoint 全返回 Chrome_RenderWidgetHostHWND，顶层一次都
   没被问到）。所以页面只负责判断"这一下是按在标题栏还是边框上"，真正动手
   交给后端调 window.drag_window() / drag_resize_window() —— 之后由系统的
   移动 / 尺寸循环接管到松手，边缘吸附、Win+方向键、双击最大化全都和拖
   原生标题栏一样。 */
const SELF_DRAWN_FRAME = (function () {
  try {
    return new URLSearchParams(location.search).get('frame') === 'none';
  } catch (e) {
    return false;   // 桩环境里没有 location，当成浏览器打开
  }
})();

let frameReady = false;

/// 请后端做一次窗口操作。action 是 drag / min / max / close / resize，
/// resize 时带方向（n / s / e / w / ne / nw / se / sw）。
function windowCtl(action, dir) {
  if (!SELF_DRAWN_FRAME) return;
  const query = '/api/window?action=' + encodeURIComponent(action)
    + (dir ? '&dir=' + encodeURIComponent(dir) : '');
  fetch(query, { method: 'POST' }).catch(() => {});
}

/// 把三键和边缘热区挂出来，并接上拖拽。只跑一次。
function initFrame() {
  if (!SELF_DRAWN_FRAME || frameReady) return;
  frameReady = true;

  const controls = document.getElementById('winControls');
  if (controls) controls.hidden = false;
  const handles = document.getElementById('frameHandles');
  if (handles) handles.hidden = false;

  // 标题栏拖拽：从空白处按下才拖。按钮、输入框、折叠标题上按下照旧是点它
  // —— 否则点「更新」会变成拖窗口。
  // 按 ID 取而不是 querySelector('.app-titlebar')：面板自测的 DOM 桩里
  // querySelector 恒返回 null（壳层逻辑一律走 getElementById），用类选择器
  // 等于把这段绑定排除了测试。
  const bar = document.getElementById('appTitlebar');
  if (bar) {
    bar.addEventListener('mousedown', function (e) {
      if (e.button !== 0) return;
      if (e.target && e.target.closest
        && e.target.closest('button, a, input, select, textarea, summary, [contenteditable]')) return;
      e.preventDefault();
      windowCtl('drag');
    });
  }

  // 八向缩放热区
  if (handles) {
    handles.addEventListener('mousedown', function (e) {
      const dir = e.target && e.target.dataset ? e.target.dataset.dir : '';
      if (!dir || e.button !== 0) return;
      e.preventDefault();
      windowCtl('resize', dir);
    });
  }
}

if (document.readyState === 'loading') {
  document.addEventListener('DOMContentLoaded', initFrame);
} else {
  initFrame();
}

// 基础层：共享状态（snap / busy / codeDirty / editingPort / flashUntil）、
// 取数通道（api / post / pull）、两个小工具（esc / icon）和 P2 起的
// 壳层逻辑（showPage 切页 / updateNavCompact 侧栏宽档）。
// 其余四个脚本都依赖这里的全局变量与函数。
let snap = null;
let busy = false;
/// 用户动过输入框就别再被后台轮询覆盖，否则清空的码会自己弹回来
let codeDirty = false;
/// 正在给某个输入口改名。轮询每 1.2 秒把 #ports 整个重建一次，
/// 重建会换掉 contenteditable 的节点 —— 用户打两个字输入就没了
/// （实测：聚焦后打字，等一轮轮询，节点被替换、文字回到旧值）。
/// 改名期间跳过那一列的重建，和 codeDirty 保护配对码输入框是同一个模式。
let editingPort = false;
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
    document.getElementById('pairBanner').textContent = '控制台与主程序失联了';
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

/// 一句轻提示（放在顶栏状态行里，3 秒后淡出）。
/// 以前这里静默 return，点了没反应 —— 用户分不清"程序坏了"和"确实切不了"。
function flash(title, sub) {
  const line = document.getElementById('status');
  line.textContent = sub ? `${title} · ${sub}` : title;
  line.style.display = '';
  // 轮询 1.2s 一次，会把这一行改回后端状态 —— 记下截止时间让 pull() 让路
  flashUntil = Date.now() + 3200;
  clearTimeout(flash.timer);
  flash.timer = setTimeout(() => {
    line.textContent = '';
    line.style.display = 'none';
    flashUntil = 0;
  }, 3200);
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

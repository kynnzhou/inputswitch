// 截图验收准备：把 assets/panel 复制到 /tmp/panel-shot，并在 index.html 的
// 自测注入点之后插入一段 fetch 桩脚本 —— 不起真后端，直接喂一份带 transfers
// 的夹具快照。参数：
//   ?theme=dark  深色（直接落 data-theme，桩在 applyTheme 之后跑）
//   ?empty=1     空态（transfers: []）
//   ?page=N      落页后翻到第 N 页（transferPage 的增量语义）
//   ?view=N      P4 起落哪一页（workspace / devices / transfers / settings，
//                默认 transfers —— 和老用法一致）
//   ?scene=multi P4 工作台分段器切到多屏共享
//   ?ports=1     P4 打开「管理输入源」弹窗
//   ?noinputs=1  P4 空态：有显示器但没有输入口（场景警示走「去扫描」档）
//   ?unpaired=1  未配对（场景警示走「去配对」档）
const fs = require('fs');
const path = require('path');

const SRC = 'D:/AItool/winapp/inputswitch/assets/panel';
const DST = '/tmp/panel-shot';

fs.rmSync(DST, { recursive: true, force: true });
fs.mkdirSync(DST, { recursive: true });
for (const f of fs.readdirSync(SRC)) {
  fs.copyFileSync(path.join(SRC, f), path.join(DST, f));
}

const transfers = [
  { id: 1, peer: 'CHINAMI-0DFF1JU', name: '产品设计稿-v3.pdf', total: 5242880, sent: 3145728, phase: 'sending', error: '', started_at: 0 },
  { id: 2, peer: 'CHINAMI-0DFF1JU', name: '会议录音-1028.m4a', total: 18874368, sent: 9437184, phase: 'paused', error: '', started_at: 0 },
  { id: 3, peer: 'CHINAMI-MC5K9SJ', name: '报销单汇总.xlsx', total: 102400, sent: 102400, phase: 'done', error: '', started_at: 0 },
  { id: 6, peer: 'CHINAMI-MC5K9SJ', name: '合同扫描件.pdf', total: 2097152, sent: 2097152, phase: 'done', error: '', started_at: 0 },
  { id: 4, peer: 'CHINAMI-YET', name: '旧版安装包.exe', total: 73400320, sent: 0, phase: 'failed', error: '连接中断', started_at: 0 },
  { id: 5, peer: 'CHINAMI-0DFF1JU', name: '临时Notes.txt', total: 2048, sent: 1024, phase: 'cancelled', error: '', started_at: 0 },
];

const fixture = {
  node: 'DESKTOP-岩',
  kvm: {
    on: true, state: '空闲', peer_alive: true, active: '',
    follow_link: true, will_follow: true, recv_scale: 1.1,
    auto_resume: true, keep_alive: true,
    links: [
      { node: 'CHINAMI-0DFF1JU', side: 'right', mode: 'edge', alive: true, active: false, addr: '192.168.0.41' },
      { node: 'CHINAMI-MC5K9SJ', side: '', mode: 'follow', alive: false, active: false, addr: '192.168.0.40' },
      { node: 'CHINAMI-ZZZZ', side: '', mode: 'follow', alive: true, active: false, addr: '192.168.0.42' },
    ],
  },
  peers: [
    { node: 'CHINAMI-0DFF1JU', addr: '192.168.0.41', input: 'dp1', label: 'DP1', value: 0x12, paired: true, knows_port: true, needs_pair: false, kvm_on: true },
    { node: 'CHINAMI-MC5K9SJ', addr: '192.168.0.40', input: '', label: '', value: 0, paired: true, knows_port: false, needs_pair: false, kvm_on: false },
    { node: 'CHINAMI-YET', addr: '192.168.0.43', input: '', label: '', value: 0, paired: false, knows_port: false, needs_pair: false, kvm_on: true },
  ],
  inputs: [
    { name: 'usbc', label: 'USB-C（本机）', value: 15, current: true, presence: 'occupied', verified: true },
    { name: 'dp', label: 'DP（对端主机）', value: 16, current: false, presence: 'occupied', verified: true },
    { name: 'hdmi1', label: 'HDMI-1', value: 17, current: false, presence: 'idle', verified: true },
  ],
  inputs_hidden: [], perms: { capture: true, inject: true },
  autostart: true, clipboard: true, hotkey: 'Ctrl+Alt+S',
  paired: true, peer_verified: 2,
  // P4 工作台场景要用的字段：共用显示器型号 / 本机有屏 / 支持性结论 /
  // 热键目标（展示名）/ 本机输入口号 / 上次检测时间
  display: 'P275MV PLUS', has_display: true,
  support: '✓ 完整支持：切换与共用键鼠都可用',
  target: 'DP（对端主机）', own_value: 15, port_note: '2026-10-28 10:24',
  inbox: 'D:\\InputSwitch\\Inbox',
  update: { phase: 'idle', current: '0.6.89', latest: '', notes: '', pub_date: '', skipped: '', downloaded: 0, total: 0, bytes_per_sec: 0, auto_restart_in: 0 },
  transfers: transfers,
};

const inject = `
<script>
(function(){
  var q = new URLSearchParams(location.search);
  if (q.get('theme') === 'dark') document.documentElement.dataset.theme = 'dark';
  var fix = ${JSON.stringify(fixture)};
  if (q.get('empty') === '1') fix.transfers = [];
  if (q.get('noinputs') === '1') fix.inputs = [];
  if (q.get('unpaired') === '1') { fix.paired = false; fix.peer_verified = 0; }
  // 更新弹窗的阶段要能拨：夹具默认 idle（只有一个按钮），而 available 一档
  // 是三按钮行（下载并安装 + 忽略本次 + 关闭），正是主次最容易糊的地方。
  if (q.get('upd')) {
    fix.update.phase = q.get('upd');
    fix.update.latest = '0.6.90';
    fix.update.notes = '修复了切换后键鼠不回本机的问题';
    fix.update.pub_date = '2026-10-27T00:00:00Z';
    fix.update.total = 8811520;
    fix.update.downloaded = 3145728;
  }
  window.fetch = function(url){
    var u = String(url || '');
    if (u.indexOf('/api/state') >= 0) return Promise.resolve({ ok: true, json: function(){ return Promise.resolve(fix); } });
    return Promise.resolve({ ok: true, json: function(){ return Promise.resolve({ ok: true }); } });
  };
  pull();
  var view = q.get('view') || 'transfers';
  showPage(view);
  // 翻页/探针都要等 pull() 的微任务把 snap 填上再跑（同步读 snap 还是 null）
  setTimeout(function () {
    var page = Number(q.get('page') || 0);
    if (page > 0) transferPage(page);
    if (q.get('scene') === 'multi') setWorkspaceScene('multi');
    if (q.get('ports') === '1') openPortsModal();
    // ?call=openHelp|openDiagnostics|openScanModal|openUpdate —— 弹窗类的
    // 截图夹具。白名单而不是 eval：这文件会被顺手跑在各种工作区里。
    var calls = {
      openHelp: function () { openHelp(); },
      openDiagnostics: function () { openDiagnostics(); },
      openScanModal: function () { openScanModal(); },
      openUpdate: function () { openUpdate(); },
    };
    if (calls[q.get('call')]) calls[q.get('call')]();
    if (q.get('probe') === '1') {
      try {
        var box = document.querySelector('.app');
        var n = ((snap && snap.transfers) || []).length;
        document.title = 'PROBE app=' + box.clientWidth + 'x' + box.clientHeight
          + ' vp=' + window.innerWidth + 'x' + window.innerHeight
          + ' pageSize=' + transferPageSize()
          + ' rows=' + n
          + ' pages=' + Math.max(1, Math.ceil(n / transferPageSize()));
      } catch (e) { document.title = 'PROBE-ERR ' + e.message; }
    }
    // ?audit=1 几何审计：把「被裁切 / 越界 / 点击区过小 / 被盖住 / 空占位」
    // 五类可疑点抓成 JSON 塞进 <pre id="probeOut">，配合 --dump-dom 取回。
    // 为什么不用视觉模型看截图：它对「紧挨」和「重叠」的判读实测会错两次，
    // 而 getBoundingClientRect 不会说谎。
    if (q.get('audit') === '1') {
      var out = document.createElement('pre');
      out.id = 'probeOut';
      try { out.textContent = JSON.stringify(auditUI(), null, 1); }
      catch (e) { out.textContent = 'AUDIT-ERR ' + e.message; }
      document.body.appendChild(out);
    }
    // ?btn=1 按钮账本：把屏上每个按钮的实测样式摊平成一列，用来判"主次有没有
    // 体现" —— 靠眼睛看截图读不出「两个描边和一个实心谁更重」，但底色 / 边框 /
    // 高度 / 宽度是硬数字。只取 :not([hidden]) 且可见的。
    if (q.get('btn') === '1') {
      var b = document.createElement('pre');
      b.id = 'btnOut';
      try { b.textContent = JSON.stringify(buttonLedger(), null, 1); }
      catch (e) { b.textContent = 'BTN-ERR ' + e.message; }
      document.body.appendChild(b);
    }
  }, 1500);
})();

/// 几何审计。判据都取「用户真的会看到/点到」的定义：
function auditUI() {
  var issues = [];
  var vw = window.innerWidth, vh = window.innerHeight;
  function name(el) {
    if (!el) return '?';
    var s = el.tagName.toLowerCase();
    if (el.id) s += '#' + el.id;
    var cls = (el.className && el.className.baseVal === undefined ? String(el.className) : '');
    if (cls) s += '.' + cls.trim().split(/\\s+/).slice(0, 2).join('.');
    return s;
  }
  function visible(el) {
    var st = getComputedStyle(el);
    if (st.display === 'none' || st.visibility === 'hidden' || st.opacity === '0') return false;
    return el.getClientRects().length > 0;
  }
  function rect(el) {
    var r = el.getBoundingClientRect();
    return { x: Math.round(r.left), y: Math.round(r.top), w: Math.round(r.width), h: Math.round(r.height) };
  }

  // 1. 文本被裁：叶子节点自己 overflow 切掉了内容（省略号不算缺陷，但
  //    scrollWidth 超出的同时没写 text-overflow 就是硬裁）。
  document.querySelectorAll('body *').forEach(function (el) {
    if (!visible(el) || el.children.length) return;
    var st = getComputedStyle(el);
    var txt = (el.textContent || '').trim();
    if (!txt) return;
    var clippedX = (st.overflowX === 'hidden' || st.overflow === 'hidden') && el.scrollWidth - el.clientWidth > 2;
    var clippedY = (st.overflowY === 'hidden' || st.overflow === 'hidden') && el.scrollHeight - el.clientHeight > 2;
    if (clippedX && st.textOverflow !== 'ellipsis') {
      issues.push({ t: 'clip-x', el: name(el), text: txt.slice(0, 40), over: el.scrollWidth - el.clientWidth, box: rect(el) });
    }
    if (clippedY) {
      issues.push({ t: 'clip-y', el: name(el), text: txt.slice(0, 40), over: el.scrollHeight - el.clientHeight, box: rect(el) });
    }
  });

  // 2. 越过视口：只查不该滚动的壳层（侧栏/顶栏/页脚/命令栏），
  //    内容区本来就是滚动容器，单独算在里面。
  ['.app-titlebar', '.sidebar', '.topbar', '.app-footer', '.win-controls', '.frame-handles'].forEach(function (sel) {
    var el = document.querySelector(sel);
    if (!el || !visible(el)) return;
    var r = el.getBoundingClientRect();
    if (r.right > vw + 1 || r.left < -1 || r.bottom > vh + 1 || r.top < -1) {
      issues.push({ t: 'oob', el: name(el), box: rect(el), vp: vw + 'x' + vh });
    }
  });

  // 3. 点击区过小（< 24×22 基本点不准；触屏标准是 44，桌面放宽）
  document.querySelectorAll('button, summary, a[href]').forEach(function (el) {
    if (!visible(el)) return;
    var r = el.getBoundingClientRect();
    if (r.width < 24 || r.height < 22) {
      issues.push({ t: 'tiny', el: name(el), text: (el.textContent || '').trim().slice(0, 24), w: Math.round(r.width), h: Math.round(r.height) });
    }
  });

  // 4. 被盖住：中心点做一次命中测试，最上层既不是自己也不是后代 ⇒ 点不到。
  document.querySelectorAll('button, summary').forEach(function (el) {
    if (!visible(el)) return;
    var r = el.getBoundingClientRect();
    if (r.width < 4 || r.height < 4) return;
    var cx = r.left + r.width / 2, cy = r.top + r.height / 2;
    if (cx < 0 || cy < 0 || cx > vw || cy > vh) return;
    var top = document.elementFromPoint(cx, cy);
    if (!top) return;
    if (top !== el && !el.contains(top) && !(top.closest && top.closest('button, summary') === el)) {
      issues.push({ t: 'covered', el: name(el), text: (el.textContent || '').trim().slice(0, 24), by: name(top) });
    }
  });

  // 5. 空占位：可见元素里只有 — / 空 / 未加载文案，说明数据没接上
  document.querySelectorAll('.v, .setting-value, .stat, [id$="Copy"]').forEach(function (el) {
    if (!visible(el)) return;
    var t = (el.textContent || '').trim();
    if (t === '' || t === '—' || t === '-') {
      issues.push({ t: 'empty', el: name(el), text: t });
    }
  });

  return {
    vp: vw + 'x' + vh,
    view: (document.querySelector('.main-nav .nav-item.selected') || {}).title || '',
    count: issues.length,
    issues: issues,
  };
}

/// 按钮账本：每个可见按钮一行，字段都是"用户能感觉到的差别"。
/// tier 是推出来的视觉档位 —— 底色越深越重，边框其次，纯文字最轻。
function buttonLedger() {
  var out = [];
  var nodes = document.querySelectorAll('button');
  for (var i = 0; i < nodes.length; i++) {
    var el = nodes[i];
    var r = el.getBoundingClientRect();
    if (!r.width || !r.height) continue;             // 隐藏的跳过
    if (el.closest('[hidden]')) continue;
    var cs = getComputedStyle(el);
    if (cs.display === 'none' || cs.visibility === 'hidden') continue;
    var bg = cs.backgroundColor;
    var bd = cs.borderTopWidth + ' ' + cs.borderTopStyle + ' ' + cs.borderTopColor;
    // 有底色 = 实心；有可见边 = 描边；全透明无边框 = 纯文字
    // 注意：这段是塞进模板字符串的，正则里的 \( 会被 JS 字符串吞掉一个反斜杠，
    // 到浏览器就变成捕获组、永远匹配不上（第一次跑 22 个全判成 solid）。
    // 用字符串比较，别用正则。
    function isClear(c) { return c === 'rgba(0, 0, 0, 0)' || c === 'transparent' || c === ''; }
    var solid = !isClear(bg);
    var bordered = parseFloat(cs.borderTopWidth) > 0 && cs.borderTopStyle !== 'none'
      && !isClear(cs.borderTopColor);
    var tier = solid ? 'solid' : (bordered ? 'outline' : 'text');
    // 图标钮：无文字且接近正方形
    var txt = (el.textContent || '').replace(/\s+/g, ' ').trim();
    var iconOnly = !txt && r.width <= 44 && Math.abs(r.width - r.height) <= 8;
    out.push({
      box: el.closest('[class]') ? (el.closest('[class]').className || '').split(' ')[0] : '',
      cls: el.className || '',
      txt: txt.slice(0, 16),
      tier: iconOnly ? 'icon' : tier,
      bg: solid ? bg : '',
      h: Math.round(r.height), w: Math.round(r.width), fs: cs.fontSize,
    });
  }
  // 按档位聚一次，方便一眼看出"这一页有几个最重的"
  var tally = {};
  out.forEach(function (b) { tally[b.tier] = (tally[b.tier] || 0) + 1; });
  return { vp: window.innerWidth + 'x' + window.innerHeight, tally: tally, buttons: out };
}
</script>
</body>`;

let html = fs.readFileSync(path.join(DST, 'index.html'), 'utf8');
html = html.replace('</body>', inject);
fs.writeFileSync(path.join(DST, 'index.html'), html);
console.log('shot dir ready:', DST);

// 静态服务（只喂这一份面板）
const http = require('http');
const mime = { '.html': 'text/html; charset=utf-8', '.css': 'text/css; charset=utf-8', '.js': 'text/javascript; charset=utf-8' };
http.createServer((req, res) => {
  let p = decodeURIComponent((req.url || '/').split('?')[0]);
  if (p === '/') p = '/index.html';
  // index.html 引用 /panel.css，磁盘上是 style.css —— 和生产路由同一个映射
  // （panel.rs 的 ("GET", "/panel.css") => PANEL_CSS）。不映射就是无样式页面。
  if (p === '/panel.css') p = '/style.css';
  fs.readFile(path.join(DST, p), (err, buf) => {
    if (err) { res.writeHead(404); res.end('nf'); return; }
    res.writeHead(200, { 'Content-Type': mime[path.extname(p)] || 'application/octet-stream' });
    res.end(buf);
  });
}).listen(8971, '127.0.0.1', () => console.log('serving on http://127.0.0.1:8971'));

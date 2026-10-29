// 面板 UI 的离线校验：不起浏览器、不开 WebView。
// 一、把四个 JS 源件合起来做语法检查（node --check 等价）
// 二、塞进一个极简 DOM 桩里真的跑一遍 renderKvm，断言渲染出来的 HTML
//
// 用法：node tools/panelcheck.js assets/panel        （拆分后的源目录）
//       node tools/panelcheck.js 旧的.html            （兼容老的单文件）
//
// 为什么不用真浏览器：本机 Chromium/WebView 在沙箱里跑 headless 会被
// 拒（code_sign_clone 目录不能删），截图这条路走不通。这个桩断言的是
// **渲染出来的字符串**，虽然粗糙，但"卡片跑进哪一栏、标签写的是什么"
// 这类最容易说谎的地方正好都是字符串。
const fs = require('fs');
const path = require('path');
const vm = require('vm');

// 源目录模式下 JS 的加载顺序 —— 必须和 index.html 里 <script src> 的顺序一致
//（core 的全局变量要让后面几个文件看得见；update.js 要在 actions.js 之前，
// 这样 actions.js 启动时调的 pull() → render() → renderUpdate() 一定有定义）。
const JS_ORDER = ['core.js', 'render.js', 'kvm.js', 'update.js', 'actions.js'];

const target = process.argv[2] || 'assets/panel';
let html, code;
if (fs.existsSync(target) && fs.statSync(target).isDirectory()) {
  const read = (f) => fs.readFileSync(path.join(target, f), 'utf8');
  const js = JS_ORDER.map(read);
  // 断言既要查 DOM（onclick 计数、顶栏结构）也要查 CSS（.peerrow）和 JS
  // （saveLabel / contenteditable），所以 html 是三者的拼接。
  html = [read('index.html'), read('style.css'), ...js].join('\n');
  code = js.join('\n;\n');
} else {
  // 老的单文件面板（拆分前）：抠 <script> 块。留着只为对旧产物做核对。
  html = fs.readFileSync(target, 'utf8');
  const blocks = [...html.matchAll(/<script>([\s\S]*?)<\/script>/g)].map(m => m[1]);
  if (!blocks.length) { console.error('没找到 <script>'); process.exit(1); }
  code = blocks.join('\n;\n');
}

// ---- 语法检查 ----
try { new vm.Script(code, { filename: 'panel.html' }); }
catch (e) { console.error('语法错误：', e.message); process.exit(1); }
console.log('✓ 语法检查通过', `(${code.split('\n').length} 行)`);

// ---- 极简 DOM 桩 ----
const made = {};
function mkEl(id) {
  const el = {
    id, innerHTML: '', textContent: '', style: {}, dataset: {},
    className: '', value: '', checked: false, children: [],
    classList: {
      _s: new Set(),
      add(c) { this._s.add(c); }, remove(c) { this._s.delete(c); },
      contains(c) { return this._s.has(c); },
      // 第三参数（force）要支持：面板里全是 classList.toggle('on', !!x) 这种写法，
      // 少了 force 就变成"每次都翻面"，断言会随机通过
      toggle(c, force) {
        const want = force === undefined ? !this._s.has(c) : !!force;
        if (want) this._s.add(c); else this._s.delete(c);
        return want;
      },
    },
    appendChild(c) { this.children.push(c); return c; },
    removeChild() {}, remove() {}, blur() {},
    // P5 批3：focus 可计数 —— 弹窗关闭后"焦点还给打开者"要能观测
    focus() { this.focusCount = (this.focusCount || 0) + 1; },
    // 属性单独存：.sw 开关的状态写在 aria-checked 上，桩里得能读回来
    _attrs: {},
    setAttribute(k, v) { this._attrs[k] = String(v); },
    getAttribute(k) { return k in this._attrs ? this._attrs[k] : null; },
    removeAttribute(k) { delete this._attrs[k]; },
    addEventListener() {}, removeEventListener() {},
    querySelector() { return null; }, querySelectorAll() { return []; },
    getBoundingClientRect() { return { left: 0, top: 0, width: 10, height: 10 }; },
    closest() { return null; },
    setPointerCapture() {}, releasePointerCapture() {},
    getContext() { return null; },
  };
  return el;
}
// P4 起预建表跟着面板结构走：旧 #ports 列表 / kvmHandBtn / kvmState 已删，
// 新增工作台场景组（sceneAlert / sceneSingle / sceneMulti / 分段器 / 共享条）
// 和管理输入源弹窗（portsModal / portsList / portsNote）。
// 批4 设备页重构：#peers 富卡片废除，新增名册列与详情双 pane 一串 ID。
const ids = ['kvmGrid', 'kvmLooseBox', 'kvmGridHint', 'kvmLinks',
  'kvmFlash', 'deviceRows', 'deviceLayoutPane', 'devicePairPane',
  'deviceLocalName', 'deviceListCount', 'placementNote', 'selectedDetail',
  'pairSummaryText', 'pairHelp', 'pairSaveBtn',
  'display', 'identity', 'support', 'status', 'hotkey',
  'target', 'node', 'permBanner', 'permText',
  'sceneAlert', 'sceneSingle', 'sceneMulti', 'sceneOpt-single', 'sceneOpt-multi',
  'kvmSwitch', 'kvmCopy', 'clipBtn', 'clipCopy',
  'wsTopology', 'wsLooseBox', 'portsModal', 'portsList', 'portsNote',
  'kvmScale09', 'kvmScale10', 'kvmScale11', 'kvmScale12', 'kvmScaleHint'];
ids.forEach(id => { made[id] = mkEl(id); });
// savePorts 要从 #portsList 里收集输入框。桩的 querySelectorAll 默认返回空数组，
// 这里按选择器返回预置的假字段（dataset.portName + value），才能验"只打改过的口"。
let portFields = [];

// P2：壳体的 .app 要能被 querySelector 拿到（updateNavCompact 量它的宽）。
// 假 app 带 clientWidth（默认 800 —— panelwin.rs 窗口 820，走图标栏档）
// 和真 classList；断言里改 clientWidth 再调函数，就能验宽档切换。
const appEl = mkEl('app');
appEl.clientWidth = 800;
// P5 批2：单屏场景的 .screen-flow（switchTo 往它挂 .is-switching，
// 信号动画 + 显示器描边高亮都挂在这个祖先上）
const flowEl = mkEl('screen-flow');
// P5 批3：document 级监听要能被断言触发（Esc 关弹窗走的是 document
// keydown）。捕获进 listeners，测试用 document.__fire(type, event) 派发。
const listeners = {};
const document = {
  getElementById: (id) => made[id] || (made[id] = mkEl(id)),
  createElement: (tag) => mkEl('new-' + tag),
  querySelector: (sel) => (sel === '.app' ? appEl : sel === '.screen-flow' ? flowEl : null),
  querySelectorAll: (sel) => (sel === '#portsList input[data-port-name]' ? portFields : []),
  addEventListener: (type, fn) => { (listeners[type] = listeners[type] || []).push(fn); },
  removeEventListener: () => {},
  __fire: (type, event) => { (listeners[type] || []).slice().forEach(fn => fn(event)); },
  body: mkEl('body'),
  activeElement: null,
  documentElement: mkEl('html'),
};

// P2.5：localStorage 要能在断言里回读（主题/减少动态效果的落库验证），
// 所以做成 ctx 和测试共用的有状态桩；matchMedia 的深浅色偏好可拨
// （__setDarkPref），验 system 档的解析。
const lsStore = {};
const lsStub = {
  getItem: (k) => (k in lsStore ? lsStore[k] : null),
  setItem: (k, v) => { lsStore[k] = String(v); },
  removeItem: (k) => { delete lsStore[k]; },
};
let darkPref = false;

const ctx = {
  document,
  window: {},
  console,
  fetch: () => Promise.resolve({ ok: true, json: () => Promise.resolve({}) }),
  setTimeout: (fn) => 0,
  clearTimeout: () => {},
  setInterval: () => 0,
  clearInterval: () => {},
  requestAnimationFrame: () => 0,
  localStorage: lsStub,
  matchMedia: (q) => ({
    matches: q.indexOf('dark') >= 0 ? darkPref : false,
    addEventListener: () => {},
  }),
  navigator: { clipboard: { writeText: () => Promise.resolve() } },
  location: { href: '' },
  alert: () => {},
};
ctx.__setDarkPref = (v) => { darkPref = !!v; };
ctx.window = ctx;
ctx.globalThis = ctx;
vm.createContext(ctx);
try { vm.runInContext(code, ctx, { filename: 'panel.html' }); }
catch (e) { console.error('运行期报错：', e.message); process.exit(1); }
console.log('✓ 脚本在 DOM 桩里跑起来了');

// ---- 造一份真实拓扑：Mac 侧（0DFF1JU 有门在右，MC5K9SJ 无门共用屏）----
const snap = {
  node: 'Mac 本机',
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
    { node: 'CHINAMI-YET', addr: '192.168.0.43', input: '', label: '', value: 0, paired: true, knows_port: false, needs_pair: false, kvm_on: true },
  ],
  inputs: [], inputs_hidden: [], perms: { capture: true, inject: true },
  autostart: true, clipboard: true,
  // P2 起页脚显示切屏热键，夹具给一个具体值
  hotkey: 'Ctrl+Alt+S',
  // 自动更新：available 是徽章唯一该亮的阶段
  update: {
    phase: 'available', current: '0.6.89', latest: '0.6.90',
    notes: '修复了切换后键鼠不回本机的问题', pub_date: '2026-10-27T00:00:00Z',
    url: '', checksum: '', error: '', code: '', skipped: '',
    checked_at: 0, downloaded: 0, total: 0, bytes_per_sec: 0, auto_restart_in: 0,
  },
};

ctx.snap = snap;
ctx.__fixture = snap;
try { vm.runInContext('snap = __fixture; render(); renderKvm(snap);', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('renderKvm 报错：', e.message); process.exit(1); }

const grid = made['kvmGrid'].innerHTML;
const loose = made['kvmLooseBox'].innerHTML;
const links = made['kvmLinks'].innerHTML;
// 名册列紧凑行（批4 起对端信息从 #peers 富卡片收成这里）
const roster = made['deviceRows'].innerHTML;

let bad = 0;
function ok(cond, label) {
  console.log((cond ? '✓ ' : '✗ ') + label);
  if (!cond) bad++;
}

// 1. 有门的对端必须落在格子里（右侧），不能进托盘
ok(/CHINAMI-0DFF1JU/.test(grid) && !/CHINAMI-0DFF1JU/.test(loose),
  '0DFF1JU（有门/右侧）画在格子里，不在托盘');
ok(/右侧/.test(grid), '格子上标了「右侧」');
// 2. 没有门的对端（真 follow 链路）进托盘，标签是「共用一块屏」
ok(/data-node="CHINAMI-MC5K9SJ"/.test(loose), 'MC5K9SJ（follow）画在托盘里');
ok(/title="CHINAMI-MC5K9SJ · 共用一块屏/.test(loose), 'MC5K9SJ 的标签是「共用一块屏」');
// 3. 未配链路的对端不能也叫「共用一块屏」
ok(/data-node="CHINAMI-YET"/.test(loose), '未配链路的对端也在托盘里');
ok(/title="CHINAMI-YET · 未摆位/.test(loose), '未配链路的标签是「未摆位」，不是「共用一块屏」');
ok(!/title="CHINAMI-YET · 共用一块屏/.test(loose), '未配链路的没有被冒认成共用一块屏');
// 4. 链路行：不通的时候说清是"键鼠没开"而不是含糊的"没回应"
ok(/CHINAMI-MC5K9SJ[\s\S]*?共用键鼠没开/.test(links), '链路行点破「它的共用键鼠没开」');
ok(/CHINAMI-0DFF1JU[\s\S]*?右侧 · 多屏共享/.test(links), '链路行照旧显示「右侧 · 多屏共享」');
ok(!/没回应/.test(links.replace(/共用键鼠没开/g, '')), '不该出现含糊的「没回应」');
// 5. 名册行带上键鼠开关状态（不开时心跳不来，表现和关机一样，必须点破）。
//    正向钉死开着的对端那行的小字就是「右侧」—— 写成否定式（"后面不出现
//    键鼠没开"）会懒爬过行界匹配到下一行，测了个寂寞。
ok(/data-peer="CHINAMI-MC5K9SJ"[\s\S]*?<small>共用一块屏 · 键鼠没开<\/small>/.test(roster),
  '名册行标出「键鼠没开」');
ok(/data-peer="CHINAMI-0DFF1JU"[\s\S]*?<small>右侧<\/small>/.test(roster),
  '开着的对端名册行小字是「右侧」（键鼠态不带后缀）');
// 6. 有 follow 链路时，多屏场景（设计稿 multiScene）里那个"把键鼠交给对端"
//    按钮必须不画 —— 画面一走面板就看不见，点了没反应。P4 起交接钮归
//    multiSceneHtml 管，判据读场景面板的渲染结果。
try { vm.runInContext("setWorkspaceScene('multi')", ctx, { filename: 'panel.html' }); }
catch (e) { console.error('setWorkspaceScene 报错：', e.message); process.exit(1); }
ok(made['sceneSingle'].hidden === true && made['sceneMulti'].hidden === false,
  '分段器切到多屏：单屏藏、多屏显示');
ok(made['sceneOpt-multi'].classList.contains('selected')
  && made['sceneOpt-multi'].getAttribute('aria-pressed') === 'true'
  && !made['sceneOpt-single'].classList.contains('selected'),
  '分段器选中态与 aria-pressed 跟到多屏');
const wsTop = made['wsTopology'].innerHTML;
const wsLoose = made['wsLooseBox'].innerHTML;
ok(/slot-right" data-drop="right"><div class="screen" data-node="CHINAMI-0DFF1JU"/.test(wsTop),
  '多屏摆位图：有门的对端落在右槽（和设备页 3×3 同一份归集逻辑）');
ok(/slot-self/.test(wsTop) && /<strong>本机<\/strong>/.test(wsTop) && /Mac 本机/.test(wsTop),
  '多屏摆位图：中心槽是本机');
ok(/data-node="CHINAMI-MC5K9SJ"/.test(wsLoose), '多屏托盘：follow 对端进共用屏栏');
// 槽位类名必须和链路 side 一一对应且都有 grid-area：CSS 里写成 up/down
// 而 JS 生成 top/bottom 时，上下两槽会丢掉 grid-area 掉进自动排列 ——
// 这种脱钩肉眼才看得出，这里按类名逐槽查 CSS（html 拼了 style.css）。
['top', 'left', 'self', 'right', 'bottom'].forEach(s => {
  ok(new RegExp('\\.slot-' + s + '\\s*\\{[^}]*grid-area').test(html),
    `多屏摆位图 .slot-${s} 有 grid-area（类名与链路 side 对齐）`);
});
ok(!/wsHandOver/.test(made['sceneMulti'].innerHTML),
  '有 follow 链路时多屏场景不画交接按钮');

// ---- 第二幕：会话中（键鼠已从共用屏那台转到并排那台）----
// 这一段检查 v0.6.60 才有的两件事：链路行的按钮该说「转给它」而不是
// 「交给它」，摆位图上起点那台要标「起点」（对面推回来会落到这里）。
const snap2 = JSON.parse(JSON.stringify(snap));
snap2.kvm.state = '键鼠已交给对端';
snap2.kvm.active = 'CHINAMI-0DFF1JU';
snap2.kvm.root = 'CHINAMI-MC5K9SJ';
// 第二幕演多屏共享拓扑（没有 follow 链路）：这样才验得到"会话中隐藏交接
// 按钮"这条新规则 —— 有 follow 链路时它本来就因为别的原因藏着。
snap2.kvm.follow_link = false;
snap2.kvm.will_follow = false;
snap2.kvm.links = snap2.kvm.links.map(l => Object.assign({}, l, {
  active: l.node === 'CHINAMI-0DFF1JU',
  can_retarget: l.node === 'CHINAMI-MC5K9SJ',
}));
// 起点那台得在线，否则「转给它」不该出现 —— 这里它在线
snap2.kvm.links.forEach(l => { if (l.node === 'CHINAMI-MC5K9SJ') l.alive = true; });
ctx.__fixture = snap2;
try { vm.runInContext('snap = __fixture; render(); renderKvm(snap);', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('第二幕 renderKvm 报错：', e.message); process.exit(1); }

const links2 = made['kvmLinks'].innerHTML;
const grid2 = made['kvmGrid'].innerHTML;
const loose2 = made['kvmLooseBox'].innerHTML;

// 7. 会话中：握着键鼠那行不显示按钮，别台显示「转给它」
ok(/CHINAMI-0DFF1JU[\s\S]*?键鼠在这台/.test(links2), '握着键鼠那台显示「键鼠在这台」');
ok(/CHINAMI-MC5K9SJ[\s\S]*?转给它/.test(links2), '会话中点起点的按钮是「转给它」（改指向，不是新建会话）');
ok(/CHINAMI-ZZZZ[\s\S]*?交给它/.test(links2), '不能改指向的那台仍显示「交给它」');
// 8. 摆位图上起点那台带「起点」标
ok(/CHINAMI-MC5K9SJ[\s\S]*?起点/.test(loose2 + grid2), '起点那台标了「起点」');
// 8b. 多屏场景跟着同一幕数据重画：会话中（active 非空）交接钮不画，
//     收回提示（Ctrl+Alt+Esc）一并收掉 —— 按钮藏了就不留说明。
const multi2 = made['sceneMulti'].innerHTML;
// 顶栏「键鼠在谁手上」在这一幕当场捕获 —— 后面的幕会改 kvm.active，
// 断言若晚跑读到的就是别的幕的状态（原来恰好排在会话中那幕之后）。
const hdrKeys2 = made['hdrKeys'].textContent;
const hdrPeer2 = made['hdrPeer'].innerHTML;
ok(!/wsHandOver/.test(multi2), '会话中（active 非空）多屏场景不画交接按钮');
ok(!/Ctrl\+Alt\+Esc/.test(multi2), '按钮不画时收回提示一并收掉');
ok(/键鼠当前在 <strong>CHINAMI-0DFF1JU<\/strong>/.test(multi2),
  '多屏场景说明键鼠当前在哪台（会话中是对端）');
// 8c. 没有 follow 链路、也不在会话中：交接钮出现，点它打到第一条活链路
//     （和设备页链路行的「交给它」同一个 /api/kvm-handover）。
const snap2b = JSON.parse(JSON.stringify(snap2));
snap2b.kvm.active = '';
ctx.__fixture = snap2b;
try { vm.runInContext('snap = __fixture; render();', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('第二幕 render 报错：', e.message); process.exit(1); }
const multi2b = made['sceneMulti'].innerHTML;
ok(/onclick="wsHandOver\(\)"/.test(multi2b), '可交接时多屏场景画出交接按钮');
ok(/Ctrl\+Alt\+Esc/.test(multi2b), '交接钮上写清收回方式（Ctrl+Alt+Esc）');
let handUrl = '';
ctx.fetch = (url) => { handUrl = url; return Promise.resolve({ ok: true, json: () => Promise.resolve({}) }); };
try { vm.runInContext('wsHandOver()', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('wsHandOver 报错：', e.message); process.exit(1); }
ok(handUrl === '/api/kvm-handover?node=CHINAMI-0DFF1JU',
  '交接打到第一条活链路（/api/kvm-handover?node=…）');
// 9. 接收手感档位：高亮当前那一档（fixture 里是 1.1），其余不高亮。
//    v0.6.83 起是分段控件：选中项挂 .sel（以前是 green / ghost 两个按钮）。
ok(made['kvmScale11'].classList.contains('sel'), '倍率 1.1 那一档高亮');
ok(!made['kvmScale10'].classList.contains('sel'), '1.0 那一档没有高亮');
ok(/×1\.1/.test(made['kvmScaleHint'].textContent), '提示里写清当前是 ×1.1');
// 10. 「收回键鼠」按钮已删：发送中光标被钉死点不到，被控时按一下鼠标就已
//     物理抢回 —— UI 上留着只会误导。接口 /api/kvm-takeback 仍保留给脚本。
//     真正能用的收回方式（Ctrl+Alt+Esc）写在多屏场景的交接钮上（见下面
//     会话中/可交接两组判据）。
ok(!/onclick="post\('\/api\/kvm-takeback'\)"/.test(html), '页面上不再有「收回键鼠」按钮');
// 12. 启动/守护三连：三个开关各自表态（v0.6.83 起是 .sw 开关 + aria-checked，
//     不再是"已开启 / 已关闭"两个按钮冒充状态）
ok(made['resumeBtn'].getAttribute('aria-checked') === 'true', '自动恢复开着时开关是 on');
ok(made['guardBtn'].getAttribute('aria-checked') === 'true', '崩溃守护开着时开关是 on');
ok(made['resumeBtn'].classList.contains('on') && made['guardBtn'].classList.contains('on'),
  '两个开关都带 .on');
ok(/已开启/.test(made['subBtn'].textContent), '三项都开时副机模式显示「已开启」');
ok(/不会一开机就抢键鼠/.test(made['bootHint'].textContent),
  '提示里说清"自动恢复不是开机就抢键鼠"');
// 13. 角色开关已删（v0.6.68）：`role` 不参与行为决策，只影响一句文案；
//     两个长得像"模式开关"的按钮跟上面"没有全局开关"那句话自相矛盾。
ok(!/kvmConsole|kvmRemote/.test(html), '页面上不再有「键鼠插在本机 / 键鼠在对面」按钮');
// 14. 顶部状态条：画面 / 键鼠 / 对端 三个事实（v0.6.69）
ok(/3 台在线/.test(hdrPeer2) && hdrKeys2 === 'CHINAMI-0DFF1JU',
  '状态条显示在线台数与键鼠现在在谁手上');
// 15. 主按钮**永远显示**（v0.6.83，用户明确要求）：不能切的时候只是换文案 +
//     挂 .is-off 并保持可点，点下去由 flash() 说明原因。绝不再 disabled / 隐藏。
ok(made['mainSwitch'].style.display !== 'none', '没有可切目标时主按钮仍然显示');
ok(!made['mainSwitch'].disabled, '没有可切目标时主按钮也不 disabled');
ok(made['mainSwitch'].classList.contains('is-off'), '此时挂 .is-off（灰蓝，但仍可见可点）');
// P1 起主按钮文案带图标（icon('swap') + <span>），JS 走 innerHTML 赋值。
// 桩的 innerHTML / textContent 是两个独立属性、不会互相同步，所以判据读
// innerHTML —— 断言意图（"文案说清是没有其他输入口"）不变。
ok(/<span>没有其他输入口<\/span>/.test(made['mainSwitch'].innerHTML),
  '文案说清是"没有其他输入口"（P1 起读 innerHTML，图标进驻按钮）');
ok(made['mainSwitch'].dataset.action === 'none', '动作标成 none，quickSwitch 据此给提示');
// 17. 维护动作只在顶部出现一次，且三个动作平铺（v0.6.83 新增「扫描输入口」——
//     以前说明里让人点，页面上却没有这个按钮）。批3 起扫描多一道确认
//     （它会真的逐个切口，屏幕会黑几次）：入口都改走确认弹窗。html 是
//     index.html + style.css + 五段 JS 的拼接，openScanModal 入口共两处 ——
//     顶栏那一枚（静态）+ 场景警示里的「去扫描」（render.js 现算）。
ok((html.match(/onclick="openScanModal\(\)"/g) || []).length === 2,
  '「扫描输入口」两个入口都走确认弹窗（顶栏那枚 + 警示条「去扫描」）');
ok((code.match(/onclick="openScanModal\(\)"/g) || []).length === 1,
  'JS 侧只有一个入口（场景警示「去扫描」，顶栏那枚在静态骨架里）');
ok((code.match(/post\('\/api\/scan'\)/g) || []).length === 1,
  '/api/scan 只有一处发起（confirmScan —— 两个入口都先过确认弹窗）');
ok((html.match(/onclick="post\('\/api\/rescan'\)"/g) || []).length === 1, '「重新检测」只有一个入口');
ok((html.match(/onclick="post\('\/api\/check'\)"/g) || []).length === 1, '「检测能否切换」只有一个入口');
// 18. 顶栏只有一个实心蓝（主按钮），维护按钮全是描边 —— 这是 B 方案的约定
const topbar = (html.split('<header class="topbar glass">')[1] || '').split('</header>')[0];
ok((topbar.match(/class="[^"]*primary/g) || []).length === 1, '顶栏只有主按钮一个是实心蓝');
ok(/id="mainSwitch"[^>]*class="primary btn-lg"/.test(topbar), '实心蓝那个就是 mainSwitch');
// 18b. 全页也只许这一个实心蓝（v0.6.85）：配对卡的「保存并连接」和共用键鼠的
//      「开启」以前也是 primary，一屏三个蓝块，主按钮的焦点地位被稀释。
//      P3 例外：更新弹窗/过渡层里的主操作。P4 例外：管理输入源弹窗的
//      「保存名称」（savePorts）、批3 的「开始扫描」（confirmScan）、批4
//      设备页配对表单的「保存并连接」（pairSaveBtn —— 都带不可逆或高
//      感知代价：装更新 / 批量改名 / 屏幕黑几次 / 建立设备信任）。遮罩
//      或整页换上时顶栏主按钮根本看不见，「任何一刻屏上只有一个蓝块」
//      的约定没破 —— 破的只是"蓝按钮只能出现在静态骨架里"这个实现细节。
//      判据按行算：带 primary 的行必须同时带 mainSwitch（静态那一个）
//      或四类主操作之一。
const blueLines = html.split('\n').filter(l => /class="[^"]*\bprimary\b/.test(l));
ok(blueLines.length >= 1 && blueLines.every(l => /mainSwitch|updateAction|savePorts|confirmScan|pairSaveBtn/.test(l)),
  '实心蓝只有顶栏主按钮和四类主操作（弹窗/pane 盖上时顶栏那个被遮住）');
ok(/id="supportLine"/.test(topbar), '支持性说明在顶栏（只在不是 ✓ 时出现）');
// 19. 对端信息的两块布局（批4 起富卡片废，信息收成名册行 + 选中详情）：
//     名册行 flex 起排、选中详情两端对齐 —— 名字再长也挤不动按钮
ok(/\.device-row \{[\s\S]*?display: flex/.test(html)
  && /\.selected-detail \{[\s\S]*?justify-content: space-between/.test(html),
  '名册行 flex、选中详情两端对齐（信息再多也不挤按钮）');
// 16. 输入口改名搬进「管理输入源」弹窗（P4）：输入框 + 批量保存
//     （savePorts → /api/label，清空 = 恢复默认名）。行是 JS 拼的，
//     桩里的 appendChild 不聚合 innerHTML，只能查源码。
ok(/function savePorts/.test(html) && /\/api\/label/.test(html)
  && /data-port-name/.test(html) && /type="text"/.test(html),
  '输入口名字在弹窗里改（savePorts + /api/label + data-port-name）');
// 20. 「该口不存在」的行：切换和设为热键都必须禁用。只禁切换不够 ——
//     热键是全局的，一按就黑屏，而且换回来比点错按钮更难。
//     夹具要放进 vm 上下文里再触发渲染，直接赋值给 Node 作用域的变量不会生效。
const snap3 = JSON.parse(JSON.stringify(snap2));
snap3.has_display = true;
snap3.inputs = [
  { name: 'usbc', label: 'USB-C（这台 Mac）', value: 15, current: true, presence: 'occupied', verified: true },
  { name: 'dp', label: 'DP', value: 16, current: false, presence: 'invalid', verified: true },
  { name: 'hdmi1', label: 'HDMI-1', value: 17, current: false, presence: 'idle', verified: true },
];
ctx.__fixture = snap3;
// 显式切回单屏：前面的幕把分段器留在了多屏，renderWorkspaceScene 只画
// 当前场景 —— 不切回来 sceneSingle 里还是第一幕的旧内容。
try { vm.runInContext("snap = __fixture; setWorkspaceScene('single'); render();", ctx, { filename: 'panel.html' }); }
catch (e) { console.error('第三幕 render 报错：', e.message); process.exit(1); }
// P4：口状态的两个视角 —— 管理弹窗的行（"设为热键"的禁用规则）和单屏场景的
// 输入源条（切换的禁用规则）。行都是 JS 拼的 innerHTML，按 data-port-name 切。
try { vm.runInContext('openPortsModal()', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('openPortsModal 报错：', e.message); process.exit(1); }
ok(made['portsModal'].style.display === '', 'openPortsModal 打开弹窗');
const rowsHtml = made['portsList'].innerHTML;
const rowOf = (name) => (rowsHtml.split('<div class="port-row">').find(r => r.indexOf('data-port-name="' + name + '"') >= 0) || '');
const deadRow = rowOf('dp');
ok(/0x10 · 该口不存在/.test(deadRow), '不存在的口在弹窗行里标了「该口不存在」');
ok(/设为热键<\/button>/.test(deadRow) && /disabled/.test(deadRow),
  '不存在的口：设为热键 disabled（热键一按就黑屏，比点错按钮更糟）');
ok(/title="显示器固件不接受这个输入口"/.test(deadRow), '不存在的口给了解释');
const curRow = rowOf('usbc');
ok(/disabled/.test(curRow) && /title="这是当前输入源"/.test(curRow),
  '当前口不能设为热键（给了理由）');
const liveRow = rowOf('hdmi1');
ok(!/disabled/.test(liveRow) && /<button class="btn-sm" onclick="setTarget\(17\)">设为热键/.test(liveRow),
  '有信号的口可以设为热键');
const strip3 = made['sceneSingle'].innerHTML;
ok(/source-button current" onclick="switchTo\(15\)"[^>]*disabled/.test(strip3),
  '当前口在输入源条里打勾且禁点');
ok(/DP · 该口不存在/.test(strip3) && /onclick="switchTo\(16\)"[^>]*disabled/.test(strip3),
  '不存在的口在输入源条里禁用并说明原因');
ok(/onclick="switchTo\(17\)"/.test(strip3) && !/onclick="switchTo\(17\)"[^>]*disabled/.test(strip3),
  '无信号的口在输入源条里可点（切过去看看）');
ok(/onclick="openPortsModal\(\)"/.test(strip3), '输入源条带「管理」入口（弹窗）');
try { vm.runInContext('closePortsModal()', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('closePortsModal 报错：', e.message); process.exit(1); }
ok(made['portsModal'].style.display === 'none', 'closePortsModal 关掉弹窗');

// ---- 自动更新：徽章 / 弹窗 / 重启过渡层（P3）----
// 第一幕（fixture 原样：available）：徽章亮、弹窗画出更新说明、过渡层藏着
ok(made['updateDot'].classList.contains('on'), '有新版本时顶栏徽章亮');
ok(made['restartOverlay'].style.display === 'none', '不在倒计时时过渡层藏掉');
const updBody = made['updateBody'].innerHTML;
ok(made['updateTitle'].textContent === '发现新版本 v0.6.90', '弹窗标题写清新版本号');
ok(/0\.6\.89 → <b>v0\.6\.90<\/b>/.test(updBody), '弹窗写出 当前 → 最新');
ok(/修复了切换后键鼠不回本机的问题/.test(updBody), '更新说明原样进弹窗');
ok(/发布于 2026-10-27/.test(updBody), '发布日期取日期段（不带时间戳）');
ok(/onclick="updateAction\('download'\)"/.test(updBody), 'available 给「下载并安装」');
ok(/onclick="updateAction\('dismiss'\)"/.test(updBody), 'available 给「忽略本次」');
// openUpdate 要真的把遮罩显示出来（否则点了没反应）
try { vm.runInContext('openUpdate()', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('openUpdate 报错：', e.message); process.exit(1); }
ok(made['updateModal'].style.display === '', 'openUpdate 打开弹窗');
try { vm.runInContext('closeUpdate()', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('closeUpdate 报错：', e.message); process.exit(1); }
ok(made['updateModal'].style.display === 'none', 'closeUpdate 关掉弹窗');
// 五个动作都走同一个入口，页面里不许有第二种拼法
ok((html.match(/onclick="updateAction\('/g) || []).length >= 5,
  '弹窗里五类动作都走 updateAction()');

// 第二幕：up_to_date —— 徽章灭，弹窗说"已是最新"
// （弹窗只在打开时刷新内容，所以每幕都先 openUpdate —— 和真实用法一致）
const snapB = JSON.parse(JSON.stringify(snap));
snapB.update = { phase: 'up_to_date', current: '0.6.89', latest: '0.6.89',
  notes: '', pub_date: '', skipped: '', downloaded: 0, total: 0, bytes_per_sec: 0, auto_restart_in: 0 };
ctx.__fixture = snapB;
try { vm.runInContext('snap = __fixture; render(); openUpdate();', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('第二幕 render 报错：', e.message); process.exit(1); }
ok(!made['updateDot'].classList.contains('on'), '已是最新时徽章不亮');
ok(/已是最新/.test(made['updateBody'].innerHTML), '已是最新时弹窗说实话');
ok(made['restartOverlay'].style.display === 'none', 'up_to_date 也不该有过渡层');

// 第三幕：downloading —— 进度条按字节数算宽，轮询临时加速
const snapC = JSON.parse(JSON.stringify(snapB));
snapC.update = { phase: 'downloading', current: '0.6.89', latest: '0.6.90',
  notes: '', pub_date: '', skipped: '', downloaded: 25 * 1024 * 1024, total: 100 * 1024 * 1024,
  bytes_per_sec: 3 * 1024 * 1024, auto_restart_in: 0 };
ctx.__fixture = snapC;
try { vm.runInContext('snap = __fixture; render(); openUpdate();', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('第三幕 render 报错：', e.message); process.exit(1); }
ok(/width:25%/.test(made['updateBody'].innerHTML), '下载进度按已下载/总数算宽');
ok(/25\.0 MB \/ 100 MB/.test(made['updateBody'].innerHTML), '进度写成人话（MB）');
ok(/3\.0 MB\/s/.test(made['updateBody'].innerHTML), '瞬时速率一起显示');
ok(/SHA-256/.test(made['updateBody'].innerHTML), '下载中说清"通过了才能装"');
ok(vm.runInContext('pullInterval()', ctx) === 250, '下载中轮询临时提到 250ms');
ok(!made['updateDot'].classList.contains('on'), '下载中徽章不亮（弹窗在说这事）');

// 第四幕：restarting —— 过渡层盖住，倒计时数字是后端给的秒数
const snapD = JSON.parse(JSON.stringify(snapC));
snapD.update = { phase: 'restarting', current: '0.6.89', latest: '0.6.90',
  notes: '', pub_date: '', skipped: '', downloaded: 100 * 1024 * 1024, total: 100 * 1024 * 1024,
  bytes_per_sec: 0, auto_restart_in: 5 };
ctx.__fixture = snapD;
try { vm.runInContext('snap = __fixture; render(); openUpdate();', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('第四幕 render 报错：', e.message); process.exit(1); }
ok(made['restartOverlay'].style.display === '', '倒计时中过渡层显示');
// 桩不做 textContent 的字符串强转（浏览器会），所以这里自己 String() 一下
ok(String(made['restartNum'].textContent) === '5', '过渡层显示后端剩的秒数（5）');
ok(/onclick="updateAction\('cancel'\)"/.test(made['updateBody'].innerHTML),
  '弹窗里也能取消重启');
ok(!made['updateDot'].classList.contains('on'), '倒计时中徽章不亮');

// 第五幕：available + 已忽略 —— 徽章灭，但弹窗里仍能装、能撤销忽略
const snapE = JSON.parse(JSON.stringify(snap));
snapE.update.skipped = '0.6.90';
ctx.__fixture = snapE;
try { vm.runInContext('snap = __fixture; render(); openUpdate();', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('第五幕 render 报错：', e.message); process.exit(1); }
ok(!made['updateDot'].classList.contains('on'), '忽略了这个版本徽章就不亮');
ok(/已忽略这个版本/.test(made['updateBody'].innerHTML), '弹窗说明"已忽略"');
ok(/恢复提示/.test(made['updateBody'].innerHTML), '忽略过的人给「恢复提示」（再点一次撤销）');
ok(/onclick="updateAction\('download'\)"/.test(made['updateBody'].innerHTML),
  '忽略过也仍然能装（用户可能改主意）');

// ---- P2 壳体重构：侧栏 + 四页 + 页脚 ----
// 21. 四页容器与导航四项（源码级：DOM 写死在 index.html 里）
ok(/<div class="view-container" id="viewContainer">/.test(html), '四页容器 viewContainer 在册');
ok((html.match(/class="page-view[ "]/g) || []).length === 4, '四页各一个 .page-view');
['workspace', 'devices', 'transfers', 'settings'].forEach(p => {
  ok(new RegExp(`id="${p}View"`).test(html), `${p} 页容器在册`);
  ok(new RegExp(`id="nav-${p}"`).test(html), `${p} 导航项在册`);
});
ok(!/id="workspaceView"[^>]*hidden/.test(html), '工作台页默认可见');
['devices', 'transfers', 'settings'].forEach(p => {
  ok(new RegExp(`id="${p}View"[^>]*hidden`).test(html), `${p} 页默认隐藏`);
});
// 顶栏重排后三件套原样保留（下一步提示 / toast / 支持性说明）
ok(/id="nextStep"/.test(topbar) && /id="status"/.test(topbar) && /id="supportLine"/.test(topbar),
  '顶栏下一步提示、toast、支持性说明原样保留');
// 22. showPage 切页行为（真调函数，看 DOM 状态）
try { vm.runInContext("showPage('devices')", ctx, { filename: 'panel.html' }); }
catch (e) { console.error('showPage 报错：', e.message); process.exit(1); }
ok(made['devicesView'].hidden === false && made['workspaceView'].hidden === true,
  'showPage：新页显示、旧页隐藏');
ok(made['nav-devices'].classList.contains('selected')
  && !made['nav-workspace'].classList.contains('selected'),
  'showPage：导航高亮跟到新页');
ok(made['nav-devices'].getAttribute('aria-current') === 'page'
  && made['nav-workspace'].getAttribute('aria-current') === null,
  'showPage：aria-current 只挂新页（读屏软件定位用）');
try { vm.runInContext("showPage('bogus')", ctx, { filename: 'panel.html' }); }
catch (e) { console.error('showPage 报错：', e.message); process.exit(1); }
ok(made['workspaceView'].hidden === false, '未知页名回落工作台（不白屏）');
// 23. 侧栏宽档：默认 800px（820 窗口）走图标栏，拖宽过 950 展开
ok(appEl.classList.contains('compact-nav'), '默认窗宽侧栏走图标栏档（.compact-nav）');
appEl.clientWidth = 1000;
try { vm.runInContext('updateNavCompact()', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('updateNavCompact 报错：', e.message); process.exit(1); }
ok(!appEl.classList.contains('compact-nav'), '拖宽过 950px 侧栏展开成文字栏');
appEl.clientWidth = 800;
vm.runInContext('updateNavCompact()', ctx, { filename: 'panel.html' });
ok(appEl.classList.contains('compact-nav'), '拖窄回 950 以下收回图标栏');
// 24. 页脚三件 + 配对状态 / 热键 / 未配对直达按钮
ok(/id="footerConnection"/.test(html) && /id="footerMessage"/.test(html),
  '页脚配对状态与消息两个容器在册');
ok(/<kbd>Ctrl<\/kbd><kbd>Alt<\/kbd><kbd>Esc<\/kbd>/.test(html) && /紧急断开键鼠/.test(html),
  '页脚常驻紧急断开键鼠提示');
const snapP = JSON.parse(JSON.stringify(snap));
snapP.paired = true; snapP.peer_verified = 2;
ctx.__fixture = snapP;
try { vm.runInContext('snap = __fixture; render();', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('P2 render 报错：', e.message); process.exit(1); }
ok(made['footerConnection'].className === 'footer-connection'
  && /已配对 · 已连接/.test(made['footerConnection'].innerHTML),
  '页脚配对状态：已连接（绿）');
ok(made['footerMessage'].textContent === '切屏热键 Ctrl+Alt+S', '页脚显示切屏热键');
ok(made['navNode'].textContent === 'Mac 本机', '侧栏本机名跟顶栏同一个值');
ok(made['deviceCount'].textContent === '3', '设备徽标显示在线台数');
// 未配对：页脚转黄，场景警示给「去配对」直达设备页（P4 起横幅变警示条）
ctx.__fixture = snap;   // 原夹具没有 paired 字段 → 未配对
try { vm.runInContext('snap = __fixture; render();', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('P2 render 报错：', e.message); process.exit(1); }
ok(made['footerConnection'].className === 'footer-connection off'
  && /未配对/.test(made['footerConnection'].innerHTML), '未配对时页脚转黄');
ok(/onclick="showPage\('devices'\)"/.test(made['sceneAlert'].innerHTML),
  '未配对时场景警示带「去配对」直达按钮（跳设备页）');

// ---- P2.5 设置页三选项卡 + 工作台共享条 ----
// 25. 三个标签 + 三个面板在册，默认只显示快捷键（源码级）
ok(/class="settings-tabs"/.test(html) && /role="tablist"/.test(html), '设置页有选项卡条（tablist）');
['shortcut', 'startup', 'appearance'].forEach(t => {
  ok(new RegExp(`id="tab-${t}"`).test(html), `${t} 标签在册`);
  ok(new RegExp(`id="${t}Panel"`).test(html), `${t} 面板在册`);
});
ok(!/id="shortcutPanel"[^>]*hidden/.test(html), '快捷键面板默认可见');
['startup', 'appearance'].forEach(t => {
  ok(new RegExp(`id="${t}Panel"[^>]*hidden`).test(html), `${t} 面板默认隐藏`);
});
// 既有元素 ID 全保留 —— 这是 render.js 零改动的依据，少一个轮询就写空
['hotkey', 'target', 'display', 'identity', 'autoBtn', 'resumeBtn',
  'guardBtn', 'bootHint', 'subBtn'].forEach(id => {
  ok(new RegExp(`id="${id}"`).test(html), `${id} 仍在册（render.js 不用动）`);
});
// 剪贴板开关搬到工作台：共享类控件住工作台，设置页不复制
const workspaceHtml = html.split('id="workspaceView"')[1].split('id="devicesView"')[0];
const settingsHtml = html.split('id="settingsView"')[1].split('id="transfersView"')[0];
ok(/id="clipBtn"/.test(workspaceHtml), '剪贴板开关住在工作台（P4 起在共享双开关条里）');
ok(/id="settingsClipSwitch"/.test(settingsHtml) && /id="settingsKvmSwitch"/.test(settingsHtml),
  '设置页按设计稿提供共享开关，使用独立 ID 并共用后端状态');
ok(/onclick="sendFilesTo\(\)"/.test(workspaceHtml),
  '工作台有快捷发送（发文件 + 去传输页，走发送门禁 canSend）');
// showSettingsTab 切 tab 行为（真调函数，看 DOM 状态）
try { vm.runInContext("showSettingsTab('startup')", ctx, { filename: 'panel.html' }); }
catch (e) { console.error('showSettingsTab 报错：', e.message); process.exit(1); }
ok(made['startupPanel'].hidden === false && made['shortcutPanel'].hidden === true,
  'showSettingsTab：新面板显示、旧面板隐藏');
ok(made['tab-startup'].classList.contains('selected')
  && !made['tab-shortcut'].classList.contains('selected'),
  'showSettingsTab：选中态跟到新标签');
ok(made['tab-startup'].getAttribute('aria-selected') === 'true'
  && made['tab-shortcut'].getAttribute('aria-selected') === 'false',
  'showSettingsTab：aria-selected 同步（读屏软件定位用）');
try { vm.runInContext("showSettingsTab('bogus')", ctx, { filename: 'panel.html' }); }
catch (e) { console.error('showSettingsTab 报错：', e.message); process.exit(1); }
ok(made['shortcutPanel'].hidden === false, '未知标签回落快捷键（不白屏）');

// ---- P2.5 主题三态 + 减少动态效果 ----
// 26. 深色令牌挂 data-theme，不再直接跟系统（三态由 JS 解析）
ok(/:root\[data-theme="dark"\]/.test(html), '深色令牌挂 :root[data-theme="dark"]');
ok(!/@media \(prefers-color-scheme: dark\)/.test(html), 'CSS 不再直接跟系统深浅色');
ok(/reduced-motion/.test(html) && /:root\.reduced-motion/.test(html), '减少动态效果有类名档（:root.reduced-motion）');
// 两个偏好 key + 头内防闪脚本（在 body 之前，首屏 paints 前落值）
ok(/inputswitch\.panel\.theme/.test(html) && /inputswitch\.panel\.reduce-motion/.test(html),
  '两个偏好 key 在册（theme / reduce-motion）');
const headHtml = html.split('<body')[0];
ok(/inputswitch\.panel\.theme/.test(headHtml), '主题防闪脚本在 head 里（首屏 paints 前落值）');
// 外观选项卡：三态分段控件 + 减少动态效果开关
['light', 'dark', 'system'].forEach(v => {
  ok(new RegExp(`id="themeOpt-${v}"`).test(html), `外观分段控件 ${v} 在册`);
});
ok(/id="motionBtn"/.test(html), '减少动态效果开关在册');
// 默认（没存过）= system：OS 浅色 → 浅色（保 P2.5 之前的行为）
try { vm.runInContext('applyTheme()', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('applyTheme 报错：', e.message); process.exit(1); }
ok(document.documentElement.dataset.theme === 'light', '默认跟随系统：OS 浅色时是浅色');
ok(made['themeOpt-system'].classList.contains('selected'), '分段控件选中「跟随系统」');
ctx.__setDarkPref(true);
vm.runInContext('applyTheme()', ctx, { filename: 'panel.html' });
ok(document.documentElement.dataset.theme === 'dark', 'system 档跟 OS 深色走');
ctx.__setDarkPref(false);
try { vm.runInContext("setTheme('light')", ctx, { filename: 'panel.html' }); }
catch (e) { console.error('setTheme 报错：', e.message); process.exit(1); }
ok(document.documentElement.dataset.theme === 'light'
  && lsStore['inputswitch.panel.theme'] === 'light', 'setTheme(light) 落库并切浅色');
ctx.__setDarkPref(true);
vm.runInContext('applyTheme()', ctx, { filename: 'panel.html' });
ok(document.documentElement.dataset.theme === 'light', '存了 light 就不再跟 OS 变');
vm.runInContext("setTheme('dark')", ctx, { filename: 'panel.html' });
ok(document.documentElement.dataset.theme === 'dark'
  && lsStore['inputswitch.panel.theme'] === 'dark', 'setTheme(dark) 落库并切深色');
ok(made['themeOpt-dark'].classList.contains('selected')
  && !made['themeOpt-light'].classList.contains('selected')
  && made['themeOpt-dark'].getAttribute('aria-pressed') === 'true',
  '分段控件选中态跟当前主题');
vm.runInContext("setTheme('bogus')", ctx, { filename: 'panel.html' });
ok(lsStore['inputswitch.panel.theme'] === 'system', '非法主题值回落 system（不写坏档）');
// 减少动态效果：开关 → html 类 + 开关态，再点回去
try { vm.runInContext('toggleReduceMotion()', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('toggleReduceMotion 报错：', e.message); process.exit(1); }
ok(document.documentElement.classList.contains('reduced-motion'), '减少动态效果开：html 挂 .reduced-motion');
ok(made['motionBtn'].classList.contains('on')
  && made['motionBtn'].getAttribute('aria-checked') === 'true', '开关态与 aria-checked 同步');
vm.runInContext('toggleReduceMotion()', ctx, { filename: 'panel.html' });
ok(!document.documentElement.classList.contains('reduced-motion'), '再点一次关回去');

// ---- P2.5 帮助与排查弹窗 ----
// 27. 两个静态弹窗 + 顶栏入口（源码级）
ok(/id="helpModal"/.test(html) && /id="diagModal"/.test(html), '帮助与排查两个弹窗在册');
ok(/class="modal-mask" id="helpModal"/.test(html) && /class="modal-mask" id="diagModal"/.test(html),
  '两个弹窗复用 modal-mask（z-40 盖过壳体）');
ok(/id="helpHotkey"/.test(html), '帮助弹窗有热键位（render.js 每轮刷新）');
ok(/onclick="closeHelp\(\);openDiagnostics\(\)"/.test(html), '帮助弹窗能转去排查弹窗');
/* 按 id 切：顶栏还带别的属性（id 就是给 core.js 的拖拽绑定用的），
   拿「整个标签字面量」当锚点会一改就 blank。 */
const titlebarHtml = html.split('id="appTitlebar"')[1].split('</header>')[0];
ok(/id="updateBtn"/.test(titlebarHtml) && /id="winControls"/.test(titlebarHtml),
  '主区顶栏含更新入口与窗口三键');
// 一体化：品牌进侧栏顶部，顶栏只属于主区。旧的通栏品牌块不能留第二份 ——
// 两处品牌会出现两个 logo，且暗示顶栏还是通栏的（那就等于没一体化）。
ok(/class="side-brand"/.test(html.split('</aside>')[0]),
  '品牌在侧栏顶部（侧栏从窗口上沿开始）');
ok(!/class="brand"/.test(html), '不残留第二处品牌块');
['workspace', 'devices', 'transfers', 'settings'].forEach(page => {
  const section = html.split('id="' + page + 'View"')[1].split('</section>')[0];
  ok(/class="view-heading"/.test(section) && /<h1>/.test(section), page + ' 页面保留设计稿页头');
});
ok(/class="icon-help"/.test(titlebarHtml) && /onclick="openHelp\(\)"/.test(titlebarHtml),
  '设计稿标题栏提供帮助入口');
ok((titlebarHtml.match(/class="icon-help"/g) || []).length === 1, '标题栏帮助入口只有一个');

// ---- 自绘窗口边框（无边框原生窗口才有的那一套）----------------------
// 判据全在 ?frame=none：浏览器打开、桩服务截图都没有这个参数，那里不该
// 出现任何窗口钮或拖拽热区（点了也没窗口可动 = 假按钮）。
['winMin', 'winMax', 'winClose'].forEach(id => {
  ok(new RegExp(`id="${id}"`).test(titlebarHtml), `窗口钮 ${id} 在顶栏里`);
});
ok(/id="winControls"[^>]*\bhidden\b/.test(html),
  '三键默认 hidden（浏览器里打开时没有窗口可控制）');
ok((html.match(/data-dir="(n|s|e|w|ne|nw|se|sw)"/g) || []).length === 8,
  '八向缩放热区方向齐全（四边 + 四角）');
ok(/SELF_DRAWN_FRAME/.test(code) && /'frame'\) === 'none'/.test(code),
  'core.js 按 ?frame=none 判定「我在原生无边框窗口里」');
ok(/function windowCtl/.test(code) && /\/api\/window\?action=/.test(code),
  'windowCtl 走 /api/window（拖拽与缩放都靠它）');
/* 拖拽绑定必须按 ID 取容器：DOM 桩的 querySelector 恒返回 null，
   querySelector('.app-titlebar') 会让这段绑定在自测里根本不执行 ——
   以后谁把按钮排除列表删了也测不出来。 */
ok(/id="appTitlebar"/.test(html) && /getElementById\('appTitlebar'\)/.test(code),
  '标题栏拖拽按 ID 取容器（桩里 querySelector 恒 null，类选择器等于不测）');
ok(/closest\('button, a, input, select, textarea, summary/.test(code),
  '拖拽排除交互元素（点「更新」不会变成拖窗口）');
ok(/function renderWindowControls/.test(code) && /snap\.maximized/.test(code),
  '最大化钮图标跟快照翻面（Win+↑ 改的态也跟得上）');
ok(/\.sidebar \{ grid-column: 1; grid-row: 1 \/ 3; \}/.test(html),
  '侧栏贯通两行：左列从顶到底是一整块「窗外」');
ok(/\.app-titlebar \{[^}]*grid-column: 2; grid-row: 1;/.test(html),
  '顶栏只占第二列第一行（不再是一条横栏压住侧栏）');
ok(!/\.app-main \{[^}]*border-radius/.test(html),
  '纸的圆角归顶栏（主区再画一次就是双圆角）');
ok(/id="titlebarPage"/.test(titlebarHtml),
  '顶栏左侧有页名位（右侧按钮靠 space-between 自然贴边）');
ok(/getElementById\('titlebarPage'\)/.test(code),
  'showPage 换页时同步顶栏页名');
// 开合行为（真调函数，看遮罩 display）
try { vm.runInContext('openHelp()', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('openHelp 报错：', e.message); process.exit(1); }
ok(made['helpModal'].style.display === '', 'openHelp 打开帮助弹窗');
try { vm.runInContext('closeHelp()', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('closeHelp 报错：', e.message); process.exit(1); }
ok(made['helpModal'].style.display === 'none', 'closeHelp 关掉帮助弹窗');
try { vm.runInContext('openDiagnostics()', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('openDiagnostics 报错：', e.message); process.exit(1); }
ok(made['diagModal'].style.display === '', 'openDiagnostics 打开排查弹窗');
try { vm.runInContext('closeDiagnostics()', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('closeDiagnostics 报错：', e.message); process.exit(1); }
ok(made['diagModal'].style.display === 'none', 'closeDiagnostics 关掉排查弹窗');
// 帮助弹窗的热键位跟着 snap.hotkey 走（夹具里是 Ctrl+Alt+S）
ctx.__fixture = snap;
try { vm.runInContext('snap = __fixture; render();', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('helpHotkey render 报错：', e.message); process.exit(1); }
ok(made['helpHotkey'].textContent === 'Ctrl+Alt+S', '帮助弹窗显示当前切屏热键');

// ---- P3 传输页：设计稿四块结构 + 传输记录列表 ----
// 28. transfersView 结构在册（源码级）：transfer-target / drop-zone /
//     transfers-list / inbox-line 四块 + 侧栏徽标；旧的一行字容器已删
const transfersHtml = html.split('id="transfersView"')[1].split('id="settingsView"')[0];
ok(/class="transfer-target"/.test(transfersHtml) && /id="recipient"/.test(transfersHtml)
  && /id="recipientTag"/.test(transfersHtml), '传输页有「发送到」选择器与配对标');
ok(/class="drop-zone" id="dropZone"/.test(transfersHtml), '拖放区在册');
ok(/onclick="sendFilesTo\(\)"/.test(transfersHtml)
  && /onclick="sendFolderTo\(\)"/.test(transfersHtml)
  && /onclick="sendTestTo\(\)"/.test(transfersHtml),
  '拖放区三个入口：选择文件 / 选择文件夹 / 发送测试文件');
ok(/id="transferRows"/.test(transfersHtml) && /id="transferPagination"/.test(transfersHtml),
  '传输记录列表与分页容器在册');
['filterAll', 'filterActive', 'filterDone'].forEach(id => {
  ok(new RegExp(`id="${id}"`).test(transfersHtml), `筛选按钮 ${id} 在册`);
});
ok(/id="inboxPath"/.test(transfersHtml), '接收目录行在册（inboxPath）');
ok(/class="nav-count transfer-count" id="transferCount"/.test(html), '侧栏传输徽标在册');
ok(!/id="inbox"/.test(html) && !/id="transfer"/.test(html),
  '旧的 #inbox / #transfer 一行字已删（被传输记录列表取代）');
// 新按钮一个实心蓝都不许有（蓝行铁律对新增 markup 一视同仁）
ok(!/class="[^"]*\bprimary\b/.test(transfersHtml), '传输页新按钮没有实心蓝（主按钮只许顶栏那一个）');

// 行为：夹具带三条不同阶段的传输，render() 之后列表按设计稿语义出内容
const snapT = JSON.parse(JSON.stringify(snap));
snapT.transfers = [
  { id: 1, peer: 'CHINAMI-0DFF1JU', name: '季度报告.pdf', total: 3 * 1024 * 1024, sent: 1024 * 1024, phase: 'sending', error: '', started_at: 0 },
  { id: 2, peer: 'CHINAMI-MC5K9SJ', name: 'a<b>&c.txt', total: 2048, sent: 2048, phase: 'done', error: '', started_at: 0 },
  { id: 3, peer: 'CHINAMI-ZZZZ', name: '演示视频.mp4', total: 0, sent: 0, phase: 'failed', error: '连接中断', started_at: 0 },
];
ctx.__fixture = snapT;
try { vm.runInContext('snap = __fixture; render();', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('P3 render 报错：', e.message); process.exit(1); }
const rowsT = made['transferRows'].innerHTML;
ok(/3 MB · 发往 CHINAMI-0DFF1JU/.test(rowsT), '传输行写清文件名 · 大小 · 发往对端');
ok(/正在发送 · 33%/.test(rowsT), '进行中显示百分比（sent/total 四舍五入）');
ok(/--progress:0\.33/.test(rowsT), '进度条按 sent/total 给 --progress');
ok(/a&lt;b&gt;&amp;c\.txt/.test(rowsT), '文件名经 esc 转义（< > & 不直出）');
ok(/已完成/.test(rowsT), '终态显示 已完成');
ok(/transferAction\(1,'pause'\)/.test(rowsT) && /transferAction\(1,'cancel'\)/.test(rowsT),
  '进行中的行给暂停/取消');
ok(!/transferAction\(2,/.test(rowsT), '已完成的行没有任何操作钮');
// 选择器 / 配对标 / 拖放区标题 / 徽标 / 接收目录
ok(/CHINAMI-0DFF1JU/.test(made['recipient'].innerHTML)
  && /CHINAMI-MC5K9SJ/.test(made['recipient'].innerHTML), '选择器列出所有对端');
ok(made['recipient'].value === 'CHINAMI-0DFF1JU', '默认选第一台对端');
ok(made['recipientTag'].className === 'tag green' && /已配对/.test(made['recipientTag'].innerHTML),
  '配对标：选中对端已配对（夹具三台都 paired）');
ok(made['dropPeer'].textContent === '把文件拖到这里，交给 CHINAMI-0DFF1JU',
  '拖放区标题跟着选中的对端走');
ok(made['transferCount'].textContent === '1' && made['transferCount'].hidden === false,
  '侧栏徽标 = 进行中条数（sending+paused），有时亮出');
ok(made['inboxPath'].textContent === '接收至 —', '接收目录行跟随 snap.inbox');
// 分页：桩的 .app 没有 clientHeight → 每页 2 条；三条分两页
ok(!/演示视频\.mp4/.test(rowsT), '第一页只出 2 条（每页 2 条）');
ok(/1 \/ 2/.test(made['transferPagination'].innerHTML), '分页显示 1 / 2（当前页 / 总页数）');
try { vm.runInContext('transferPage(1)', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('transferPage 报错：', e.message); process.exit(1); }
ok(/演示视频\.mp4/.test(made['transferRows'].innerHTML), '翻页后出第三条');
ok(/连接中断/.test(made['transferRows'].innerHTML)
  && /transferAction\(3,'retry'\)/.test(made['transferRows'].innerHTML),
  '失败的行显示连接中断并给重试');
ok(/2 \/ 2/.test(made['transferPagination'].innerHTML), '翻页后分页显示 2 / 2');
// 筛选：done 档只留已完成；选中态和 aria-pressed 同步
try { vm.runInContext("setTransferFilter('done')", ctx, { filename: 'panel.html' }); }
catch (e) { console.error('setTransferFilter 报错：', e.message); process.exit(1); }
ok(made['filterDone'].classList.contains('selected')
  && made['filterDone'].getAttribute('aria-pressed') === 'true'
  && !made['filterAll'].classList.contains('selected'),
  '筛选按钮选中态与 aria-pressed 同步（读屏软件定位用）');
ok(/a&lt;b&gt;&amp;c\.txt/.test(made['transferRows'].innerHTML)
  && !/季度报告\.pdf/.test(made['transferRows'].innerHTML),
  '「已完成」档只留 done，进行中和失败都被滤掉');
// 控制类动作打到 /api/transfer（一个入口四个动作），发送类带 node 参数
let lastUrl = '';
ctx.fetch = (url) => { lastUrl = url; return Promise.resolve({ ok: true, json: () => Promise.resolve({}) }); };
try { vm.runInContext("transferAction(1, 'pause')", ctx, { filename: 'panel.html' }); }
catch (e) { console.error('transferAction 报错：', e.message); process.exit(1); }
ok(lastUrl === '/api/transfer?action=pause&id=1', '暂停打到 /api/transfer?action=pause&id=1');
try { vm.runInContext('sendFilesTo()', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('sendFilesTo 报错：', e.message); process.exit(1); }
ok(lastUrl === '/api/send?node=CHINAMI-0DFF1JU', '选择文件带着选择器里的对端发');
ok(made['transfersView'].hidden === false && made['workspaceView'].hidden === true,
  '发送后跳到传输页（接下来要看的是记录）');

// ---- P4 工作台按设计稿重做：场景分段 + 场景警示 + 双场景 + 共享条 + 弹窗 ----
// 29. 静态骨架在册（源码级）：页头与分段器 / 场景警示 / 两个场景容器 /
//     共享双开关 / 快捷传输 / 管理输入源弹窗。
const wsHtml = html.split('id="workspaceView"')[1].split('id="devicesView"')[0];
ok(/class="view-heading"/.test(wsHtml) && /<h1>工作台<\/h1>/.test(wsHtml)
  && /在设备之间，轻松接续你的工作。/.test(wsHtml), '工作台页头（设计稿 pageHeading）');
ok(/id="sceneOpt-single"/.test(wsHtml) && /id="sceneOpt-multi"/.test(wsHtml)
  && /onclick="setWorkspaceScene\('single'\)"/.test(wsHtml)
  && /onclick="setWorkspaceScene\('multi'\)"/.test(wsHtml),
  '场景分段器在册（单屏切换 / 多屏共享）');
ok(/id="sceneAlert"/.test(wsHtml) && /id="sceneSingle"/.test(wsHtml)
  && /id="sceneMulti"/.test(wsHtml), '场景警示与两个场景容器在册');
ok(/id="sceneMulti" hidden/.test(wsHtml) && !/id="sceneSingle" hidden/.test(wsHtml),
  '多屏场景默认隐藏（分段器默认单屏）');
ok(/class="sharing-strip"/.test(wsHtml) && /id="kvmSwitch"/.test(wsHtml)
  && /id="clipBtn"/.test(wsHtml) && /id="kvmCopy"/.test(wsHtml)
  && /id="clipCopy"/.test(wsHtml), '共享双开关（共用键鼠 + 剪贴板）各带状态文案');
ok(/class="quick-transfer"/.test(wsHtml)
  && /onclick="sendFilesTo\(\)"/.test(wsHtml),
  '快捷传输条走 sendFilesTo（和传输页同一个发送门禁 canSend）');
ok(/class="modal-mask" id="portsModal"/.test(html) && /id="portsList"/.test(html)
  && /id="portsNote"/.test(html), '管理输入源弹窗在册（复用 modal-mask）');
ok(/onclick="savePorts\(\)"/.test(html) && /onclick="closePortsModal\(\)"/.test(html),
  '弹窗有保存与取消入口');
ok(!/id="kvmState"/.test(html) && !/id="kvmPower"/.test(html) && !/id="ports"/.test(html)
  && !/id="pairBanner"/.test(html), '旧共用键鼠卡 / 行内改名列表 / 配对横幅已删（搬迁完成）');

// 30. 一切正常时（已配对 · 已验证 · 有对端在线 · 有输入口 · 完整支持）：
//     场景警示整块消失，单屏场景画共用显示器头 + 双机卡片 + 输入源条。
const snapW = JSON.parse(JSON.stringify(snap));
snapW.paired = true; snapW.peer_verified = 2;
snapW.has_display = true; snapW.display = 'P275MV PLUS';
snapW.support = '✓ 完整支持：切换与共用键鼠都可用';
snapW.own_value = 15; snapW.target = 'HDMI-1';
snapW.port_note = '2026-10-28 10:24';
snapW.inputs = [
  { name: 'usbc', label: 'USB-C（这台 Mac）', value: 15, current: true, presence: 'occupied', verified: true },
  { name: 'hdmi1', label: 'HDMI-1', value: 17, current: false, presence: 'occupied', verified: true },
  { name: 'dp', label: 'DP', value: 16, current: false, presence: 'invalid', verified: true },
];
ctx.__fixture = snapW;
try { vm.runInContext("snap = __fixture; setWorkspaceScene('single'); render();", ctx, { filename: 'panel.html' }); }
catch (e) { console.error('P4 render 报错：', e.message); process.exit(1); }
ok(made['sceneAlert'].innerHTML === '', '一切正常时场景警示整块消失（不占版面）');
const single = made['sceneSingle'].innerHTML;
ok(/共用显示器 <strong>P275MV PLUS<\/strong>/.test(single), '单屏场景头写清共用显示器型号');
ok(/tag green">连接正常/.test(single), '状态标：连接正常（绿）');
ok(/computer-display active" onclick="wsSwitch\('local'\)"/.test(single),
  '本机卡是"当前"（active + wsSwitch(local)）');
ok(/computer-display" onclick="wsSwitch\('peer'\)"/.test(single)
  && !/computer-display" onclick="wsSwitch\('peer'\)"[^>]*disabled/.test(single),
  '对端卡可点（有"非当前、有主机"的口时）');
ok(/画面到哪，键鼠就跟到哪/.test(single), 'flow 说明：跟随画面时键鼠跟过去');
ok(/source-button current" onclick="switchTo\(15\)"[^>]*disabled/.test(single)
  && /HDMI-1 · 有主机/.test(single) && /DP · 该口不存在/.test(single),
  '输入源条：当前口禁点、有主机的口可点、不存在的口禁点并说明');
ok(made['kvmSwitch'].classList.contains('on')
  && made['kvmSwitch'].getAttribute('aria-checked') === 'true',
  '共享条：共用键鼠开关态跟随后端');
ok(made['kvmCopy'].textContent === '随画面切换，自动交接',
  '共享条文案：单屏场景说"随画面切换，自动交接"');
ok(made['clipBtn'].getAttribute('aria-checked') === 'true'
  && made['clipCopy'].textContent === '文字只同步到当前使用的设备',
  '共享条：剪贴板开关态与文案跟随后端');
ok(made['settingsKvmSwitch'].getAttribute('aria-checked') === 'true'
  && made['settingsClipSwitch'].getAttribute('aria-checked') === 'true',
  '设置页共享开关与工作台读取同一份已开启状态');
vm.runInContext('snap.kvm.on = false; snap.clipboard = false; renderWorkspaceScene();', ctx);
ok(['kvmSwitch', 'settingsKvmSwitch', 'clipBtn', 'settingsClipSwitch']
  .every(id => made[id].getAttribute('aria-checked') === 'false'),
  '后端关闭共享后，工作台与设置页四个开关一起更新');
vm.runInContext('snap.kvm.on = true; snap.clipboard = true; renderWorkspaceScene();', ctx);

// 31. 单屏场景的卡片切换：只信本机 inputs 的 value（对端自报的 value 是
//     它自己那块屏的编号，跨机不可信）。画面在本机时点本机卡 = 一句话打发。
let swUrl = '';
ctx.fetch = (url) => { swUrl = url; return Promise.resolve({ ok: true, json: () => Promise.resolve({}) }); };
try { vm.runInContext("wsSwitch('local')", ctx, { filename: 'panel.html' }); }
catch (e) { console.error('wsSwitch 报错：', e.message); process.exit(1); }
ok(swUrl === '' && /画面已经在本机/.test(made['status'].innerHTML),
  '画面在本机时点本机卡：不发请求，一句话说清');
try { vm.runInContext("wsSwitch('peer')", ctx, { filename: 'panel.html' }); }
catch (e) { console.error('wsSwitch 报错：', e.message); process.exit(1); }
ok(swUrl === '/api/switch?value=17', '点对端卡：切到"非当前、有主机"的那个口');

// 32. blind（画面在别处）：对端卡变成"当前"，本机卡变成"请对端切回"
//     （ask-peer —— 本机 DDC 不通，指令得由正在显示画面的那台发）。
const snapBlind = JSON.parse(JSON.stringify(snapW));
snapBlind.has_display = false;
ctx.__fixture = snapBlind;
try { vm.runInContext('snap = __fixture; render();', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('P4 render 报错：', e.message); process.exit(1); }
ok(/tag warn">画面在别处/.test(made['sceneSingle'].innerHTML),
  'blind 时状态标：画面在别处（黄）');
ok(/computer-display active" onclick="wsSwitch\('peer'\)"[^>]*aria-pressed="true"/.test(made['sceneSingle'].innerHTML),
  'blind 时对端卡是"当前"（aria-pressed 跟着事实走）');
swUrl = '';
try { vm.runInContext("wsSwitch('local')", ctx, { filename: 'panel.html' }); }
catch (e) { console.error('wsSwitch 报错：', e.message); process.exit(1); }
ok(swUrl === '/api/ask-peer?value=15', 'blind 时点本机卡：请对端切回（ask-peer）');
swUrl = '';
try { vm.runInContext("wsSwitch('peer')", ctx, { filename: 'panel.html' }); }
catch (e) { console.error('wsSwitch 报错：', e.message); process.exit(1); }
ok(swUrl === '' && /这台屏不在本机/.test(made['status'].innerHTML),
  'blind 时点对端卡：不发请求，一句话说清画面在对端');

// 33. 场景警示六个分支各带直达动作（设计稿 sceneAlert 的生产分支）
const alertOf = (over) => {
  ctx.__fixture = Object.assign(JSON.parse(JSON.stringify(snapW)), over);
  vm.runInContext('snap = __fixture; render();', ctx, { filename: 'panel.html' });
  return made['sceneAlert'].innerHTML;
};
ok(/去配对/.test(alertOf({ paired: false })), '未配对：给「去配对」');
ok(/去设备页/.test(alertOf({ peer_verified: 0 })), '没验过身份：给「去设备页」');
ok(/没通过校验/.test(alertOf({ peer_verified: 0, rejected: 3 })),
  '校验失败：说清"没通过校验"的条数');
ok(/查看设备/.test(alertOf({ peers: [] })), '没有对端：给「查看设备」');
ok(/已离线/.test(alertOf({ kvm: Object.assign({}, snapW.kvm, { peer_alive: false }) })),
  '对端离线：给「查看设备」');
ok(/去扫描/.test(alertOf({ inputs: [] })), '有显示器但没有输入口：给「去扫描」');
const badSupport = alertOf({ support: '✗ 切换需要 MCCS 支持' });
ok(/排查/.test(badSupport) && /class="alert-inline error"/.test(badSupport),
  '不支持切换：红档 + 「排查」');

// 34. 批量改名：只打改过的口；没改动就关弹窗 + 一句话打发
//     （桩的输入框由 portFields 提供 —— querySelectorAll 按选择器返回它）
portFields = [
  (() => { const f = mkEl('f-usbc'); f.dataset.portName = 'usbc'; f.value = 'USB-C（这台 Mac）'; return f; })(),
  (() => { const f = mkEl('f-hdmi1'); f.dataset.portName = 'hdmi1'; f.value = 'HDMI-1（对端）'; return f; })(),
  (() => { const f = mkEl('f-dp'); f.dataset.portName = 'dp'; f.value = ''; return f; })(),
];
let labelCalls = [];
ctx.fetch = (url, opt) => {
  labelCalls.push(url + ' ' + (opt && opt.body));
  return Promise.resolve({ ok: true, json: () => Promise.resolve({}) });
};
try { vm.runInContext('savePorts();', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('savePorts 报错：', e.message); process.exit(1); }
ok(labelCalls.length === 2
  && /^\/api\/label \{"name":"hdmi1","label":"HDMI-1（对端）"\}$/.test(labelCalls[0])
  && /^\/api\/label \{"name":"dp","label":""\}$/.test(labelCalls[1]),
  '批量改名只打改过的口（hdmi1 改名 + dp 清空恢复默认，usbc 没动）');
portFields = [];
labelCalls = [];
try { vm.runInContext('savePorts();', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('savePorts 报错：', e.message); process.exit(1); }
ok(labelCalls.length === 0 && made['portsModal'].style.display === 'none'
  && /名称没有改动/.test(made['status'].innerHTML),
  '没有改动：不发请求、关弹窗、一句话说清');

// 35. 打字保护：聚焦输入框期间轮询不重建列表（重建会换掉 input 节点，
//     打了一半的字被抹掉 —— 同 codeDirty 保护配对码的模式）。
try { vm.runInContext('openPortsModal(); editingPorts = true;', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('openPortsModal 报错：', e.message); process.exit(1); }
made['portsList'].innerHTML = '<marker/>';
try { vm.runInContext('render();', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('P4 render 报错：', e.message); process.exit(1); }
ok(made['portsList'].innerHTML === '<marker/>',
  '聚焦输入框时轮询不重建列表（editingPorts 守卫）');
try { vm.runInContext('editingPorts = false; render();', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('P4 render 报错：', e.message); process.exit(1); }
ok(made['portsList'].innerHTML !== '<marker/>', '失焦后轮询恢复重建');


// 36. P5 批1：版式对齐设计稿（CSS 源码断言，防回归）。
//     背景：2026-10-28 设计稿 vs 生产差距审查的 B 层修复。
ok(/--sidebar: #f4f5f8/.test(html) && /--sidebar: #1b2029/.test(html),
  '--sidebar 令牌落地（浅 #f4f5f8 / 深 #1b2029，设计稿 app.css:7/:35）');
ok(/\.sidebar \{[^}]*background: var\(--sidebar\)/.test(html)
  && /\.app-footer \{[^}]*background: var\(--sidebar\)/.test(html),
  '侧栏与页脚底色用 --sidebar（不再和画布 stage 混用）');
ok(/padding: 17px 24px 10px/.test(html), '命令栏内边距 = DESIGN.md:251 明文值 17/24/10');
ok(/padding: 23px 26px 18px/.test(html), '内容区内边距 = DESIGN.md:251 明文值 23/26/18');
ok(/\.compact-nav \.view-container \{ padding: 20px 23px 16px; \}/.test(html),
  '≤950 档（820 生产窗常态）内容区 20/23/16（设计稿 app.css:357）');
/* 断言盯的是「纵向填充不断裂」这个意图，不是某个字面属性：原来写死
   height: 100%，加了全局标题栏 + 页头之后固定 100% 会把 flex:1 的场景
   面板压到塌（矮窗档还显式 min-height:0），标题被压进内容区、本机名与
   「输入源」行叠在一起。改成 min-height:100% + height:auto 后实现变了
   但意图不变 —— 至少占满、超出则交给 .view-container 滚动。 */
ok(/\.page-view \{[^}]*min-height: 100%/.test(html),
  '.page-view min-height:100%（纵向填充不断裂，内容超高时转滚动，设计稿 app.css:143）');
ok(/\.scene-panel \{[^}]*flex: 1/.test(html),
  '.scene-panel flex:1（场景面板拉伸、共享条贴底，设计稿 app.css:150）');
/* 状态行第一项必须是「本机 + 机器名」的完整 stat。它一度被改成
   <span id="node" hidden> 的空壳 —— render.js 照旧往里写 snap.node，
   屏幕上却什么都没有，顶栏认不出这是哪台电脑（.stat.node .v 的样式
   还留着，说明只是结构被改坏了）。 */
ok(/<div class="stat node">[\s\S]{0,300}?id="node"/.test(html)
  && !/<span id="node" hidden/.test(html),
  '状态行第一项是「本机 + 机器名」（不是 hidden 的空 span）');
ok(/@media \(max-width: 950px\) \{[\s\S]{0,700}?\.command-top \.stat \.v \{ max-width: 96px/.test(html),
  '≤950 档状态行收窄（项宽 96px + 分隔 9px，不与主按钮抢宽度）');
ok(/@media \(max-width: 720px\) \{[\s\S]{0,120}?\.command-top \{ flex-wrap: wrap; \}/.test(html),
  '≤720 档主按钮整行换行（状态行四项不再被 overflow:hidden 从两端裁掉）');
ok(/\.is-switching \.signal \{ opacity: 1/.test(html)
  && /@keyframes signal-travel/.test(html),
  '切换信号动画激活（.is-switching .signal + signal-travel，设计稿 app.css:174/344）');
ok(/input:focus-visible, select:focus-visible/.test(html),
  '焦点环覆盖 input/select（设计稿 app.css:60）');
ok(/^input, select \{/m.test(html) && /min-height: 34px/.test(html)
  && /select \{ cursor: pointer; \}/.test(html),
  'input/select 基础规则（34px 高/边/圆角/底，设计稿 app.css:62-63）');
ok(/\.status-dot\.off \{ background: var\(--muted\); \}/.test(html),
  '.status-dot.off 变体（画面在别处时圆点转灰，设计稿 app.css:87）');
ok(/button\.primary \{[^}]*min-height: 37px/.test(html),
  '.primary 37px 基准高度（设计稿 app.css:78）');
ok(/\.shared-screen-tray\.aim/.test(html)
  && /1px dashed var\(--line-strong\)/.test(html),
  '共用屏托盘虚线放置框 + .aim 高亮（设计稿 app.css:239-242）');
ok(/\.segmented \.icon \{ width: 15px/.test(html),
  '分段控件图标 15px 基础规则（设计稿 app.css:149）');
ok(/\.view-heading h1 \{ font-size: 24px/.test(html),
  '页头 h1 24px（设计稿 app.css:68）');
ok(/kbd \{[^}]*border-bottom-width: 2px/.test(html),
  '全局 kbd 底边 2px（设计稿 app.css:92）');

// 37. P5 批2：交互修复（点击放置 / 撤销 / 焦点保持 / 校验 / 回执 / 门禁）。
//     2026-10-28 设计稿 vs 生产差距审查的 A/B 层交互项。
//     注意 flash() 从 textContent 改成 innerHTML（要挂动作钮），桩里两者
//     互不同步，判据一律读 innerHTML。
// 37.1 flash 动作钮：撤销 / 直达动作（设计稿 toast undo 同款，6500ms）
try { vm.runInContext("flash('已放到右侧', '键鼠推过去', { text: '撤销', fn: function(){} })", ctx, { filename: 'panel.html' }); }
catch (e) { console.error('flash 动作钮报错：', e.message); process.exit(1); }
ok(/已放到右侧 · 键鼠推过去/.test(made['status'].innerHTML)
  && /<button type="button" class="toast-action">撤销<\/button>/.test(made['status'].innerHTML),
  'flash 第三参渲染动作钮（撤销这类"做完还能反悔"的场景）');

// 37.2 空配对码被拦下：空码直发 = 解除配对（后端 Pair("") 语义）——
//      点「保存并连接」却把配对拆了，是静默的事故
try { vm.runInContext("document.getElementById('code').value = '   '; savePair()", ctx, { filename: 'panel.html' }); }
catch (e) { console.error('savePair 报错：', e.message); process.exit(1); }
ok(/先填一个配对码/.test(made['status'].innerHTML)
  && /去取消配对/.test(made['status'].innerHTML),
  '空配对码被拦下，并给一条去取消配对的直路');

// 37.3 随机生成：六位 + 标 codeDirty（否则下一轮轮询把它冲掉）
try { vm.runInContext('genCode()', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('genCode 报错：', e.message); process.exit(1); }
const genCode = made['code'].value;
try { vm.runInContext('render()', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('render 报错：', e.message); process.exit(1); }
ok(/^[0-9]{6}$/.test(genCode) && made['code'].value === genCode,
  '随机生成六位码，且标 dirty 后轮询不覆盖');

// 37.4 切换进行态：主按钮 aria-busy + 场景面板 .is-switching（信号动画）
try { vm.runInContext('switchTo(17)', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('switchTo 报错：', e.message); process.exit(1); }
ok(made['mainSwitch'].getAttribute('aria-busy') === 'true'
  && /切换中…/.test(made['mainSwitch'].innerHTML)
  && flowEl.classList.contains('is-switching'),
  '切换中：aria-busy + 场景面板 is-switching（设计稿同款进行态）');
try { vm.runInContext('render()', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('render 报错：', e.message); process.exit(1); }
ok(made['mainSwitch'].getAttribute('aria-busy') === null,
  '轮询把按钮换回正常态时摘掉 aria-busy（不永久念"忙"）');

// 37.5 发送门禁：没对端 / 对端设了码而本机没设，提前说清并给直路
//      （后端 resolve_peer / receive_one 会拒，但拒绝只写进后端状态行）
const curFix = ctx.__fixture;
const peersBackup = curFix.peers;
ctx.__peersBackup = peersBackup;
// 指路设备页的导航在动作钮闭包里（桩不点钮），改用 fetch 计数证明门禁拦住了：
// 放行时 sendFilesTo 会 post('/api/send…')，被拦下一次请求都不该发出
const gateUrls = [];
ctx.fetch = (url) => { gateUrls.push(url); return Promise.resolve({ ok: true, json: () => Promise.resolve({}) }); };
try { vm.runInContext('snap.peers = []; sendFilesTo()', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('sendFilesTo 报错：', e.message); process.exit(1); }
ok(gateUrls.length === 0
  && /还没有在线的对端/.test(made['status'].innerHTML)
  && /查看设备/.test(made['status'].innerHTML),
  '没有对端：发送被拦下并指路设备页（一次请求都没发出）');
try { vm.runInContext("snap.peers = __peersBackup; snap.peers[0].needs_pair = true;"
  + " document.getElementById('recipient').value = 'CHINAMI-0DFF1JU'; sendFilesTo()", ctx, { filename: 'panel.html' }); }
catch (e) { console.error('sendFilesTo 报错：', e.message); process.exit(1); }
ok(/对端设了配对码，本机没设/.test(made['status'].innerHTML)
  && /去配对/.test(made['status'].innerHTML),
  '对端设了码而本机没设：发送被拦下（文件会在握手处被拒）');
try { vm.runInContext('snap.peers[0].needs_pair = false', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('恢复夹具报错：', e.message); process.exit(1); }

// 37.6 点击放置：选中态 + aim 高亮 + 文案（pointerup 的 !moved 分支以前是
//      "什么都不做"，提示文案却写着「点它再点格子」—— 兑现不了的承诺）
ok(/kvmSelected/.test(code) && /已选中/.test(code)
  && /点一台对端选中它，再点方向放下/.test(made['kvmGridHint'].textContent),
  '提示文案对得上点击放置（点一台 → 再点方向）');
ok(/放到这里/.test(code) && /点这里放下/.test(code),
  '选中后空格子文案变「放到这里」');
ok(/\.screen\.sel/.test(html) && /\.grid \.cell\.aim/.test(html)
  && /\.topology-slot\.aim/.test(html),
  '选中卡片 .sel + 空格子 .aim 高亮样式在册');
ok(/function canSend/.test(code) && /needs_pair/.test(code),
  '发送门禁在册（无对端 / 对端要求配对）');
ok(/function resetHotkey/.test(code) && /\/api\/hotkey-reset/.test(code)
  && /onclick="resetHotkey\(\)"/.test(html),
  '恢复默认热键入口（前端 resetHotkey + 后端 /api/hotkey-reset）');

// 37.7 场景面板焦点保持：内容没变就不碰 DOM（照抄传输列表的缓存套路）。
//      轮询 1.2s 整段重画会把用户正盯着的按钮节点换掉，也会闪断
//      switchTo 挂上的 .is-switching 动画。
try { vm.runInContext("workspaceUi.scene = 'single'; renderWorkspaceScene()", ctx, { filename: 'panel.html' }); }
catch (e) { console.error('renderWorkspaceScene 报错：', e.message); process.exit(1); }
made['sceneSingle'].innerHTML = '<marker/>';
try { vm.runInContext('renderWorkspaceScene()', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('renderWorkspaceScene 报错：', e.message); process.exit(1); }
ok(made['sceneSingle'].innerHTML === '<marker/>',
  '场景内容没变时轮询不重画（缓存挡掉，焦点不丢）');
ok(/function paintScene/.test(code) && /data-scene-focus/.test(code)
  && /sceneCache/.test(code) && /preventScroll/.test(code),
  '场景面板重绘后把焦点还给原来那枚钮（锚点还焦）');

// 37.8 placeOn：乐观落格（同步）+ 成功 flash 挂撤销（异步续体里）。
//      撤销是设计稿 placePeer 的 undo toast：把自己放回原位、被顶走的那台归位。
try { vm.runInContext("placeOn('left', 'CHINAMI-MC5K9SJ')", ctx, { filename: 'panel.html' }); }
catch (e) { console.error('placeOn 报错：', e.message); process.exit(1); }
ok(/data-node="CHINAMI-MC5K9SJ"/.test(made['kvmGrid'].innerHTML)
  && !/CHINAMI-MC5K9SJ/.test(made['kvmLooseBox'].innerHTML),
  'placeOn 后卡片当场落进左格、离开托盘（乐观更新不等轮询）');

// ---- 38. P5 批3：弹窗可访问性 + 扫描确认 + 维护三钮 busy ----
// 前几幕测试留下两个开着的遮罩（updateModal / portsModal），先收干净，
// 否则栈里带着残留，Esc 测试关的是别人的弹窗。
try { vm.runInContext("closePortsModal(); closeUpdate();", ctx, { filename: 'panel.html' }); }
catch (e) { console.error('收弹窗报错：', e.message); process.exit(1); }
ok(made['portsModal'].style.display === 'none' && made['updateModal'].style.display === 'none'
  && vm.runInContext('topModal()', ctx) === null,
  '开局栈是干净的（closeModal 按 id 摘栈，不误伤别层）');

// 38.1 三件套在册（源码级）
ok(/function openModal/.test(code) && /function closeModal/.test(code)
  && /function topModal/.test(code) && /addEventListener\('keydown'/.test(code),
  '弹窗三件套在册（openModal/closeModal/topModal + document keydown）');
ok((html.match(/aria-modal="true"/g) || []).length === 5,
  '五个弹窗都声明 aria-modal（读屏软件知道背景不可交互）');
ok(/onclick="if\(event\.target===this\)closeModal\('updateModal'\)"/.test(html)
  && /onclick="if\(event\.target===this\)closeModal\('scanModal'\)"/.test(html),
  '遮罩点击关闭：点空白处关、点框里不关（event.target 判界）');
ok(/querySelector\('\.modal'\)/.test(code) && /preventScroll/.test(code),
  '开弹窗把焦点送进框（键盘/读屏用户知道"现在在对话框里"）');

// 38.2 Esc 逐层关：帮助转排查是两个叠着的遮罩，Esc 一次只关最上面
//      （合成事件带 target —— kvm.js 的方向键兜底监听也挂在 keydown 上，
//        真实浏览器的 event.target 必有，桩里自己造一个）
const escEvent = () => ({ key: 'Escape', preventDefault() {}, target: { closest: () => null } });
try { vm.runInContext('openHelp(); openDiagnostics();', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('开关弹窗报错：', e.message); process.exit(1); }
ok(made['helpModal'].style.display === '' && made['diagModal'].style.display === '',
  '帮助转排查：先关帮助再开排查，两个遮罩接力（栈记录打开顺序）');
document.__fire('keydown', escEvent());
ok(made['diagModal'].style.display === 'none' && made['helpModal'].style.display === '',
  'Esc 只关最上面那个（排查关掉，帮助还在）');
document.__fire('keydown', escEvent());
ok(made['helpModal'].style.display === 'none',
  '再按 Esc 关剩下那个（栈逐层收，不是一次全关）');
document.__fire('keydown', escEvent());
ok(vm.runInContext('topModal()', ctx) === null,
  '没有弹窗时 Esc 不误伤（热键录制里的 Escape 归录制逻辑自己消费）');

// 38.3 焦点还回：关弹窗把焦点还给打开它的那枚钮（桩的 focus 可计数）
document.activeElement = made['status'];
const focusBefore = made['status'].focusCount || 0;
try { vm.runInContext('openHelp()', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('openHelp 报错：', e.message); process.exit(1); }
try { vm.runInContext('closeHelp()', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('closeHelp 报错：', e.message); process.exit(1); }
ok((made['status'].focusCount || 0) === focusBefore + 1,
  '关弹窗把焦点还给打开它的那枚钮（键盘用户接着 Tab 不乱跳）');
try { vm.runInContext('openModal("helpModal")', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('openModal 报错：', e.message); process.exit(1); }
ok(made['helpModal'].style.display === '',
  'openModal 直接开（字符串参数也认，onclick 里就用它）');
try { vm.runInContext('closeModal("helpModal")', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('closeModal 报错：', e.message); process.exit(1); }
ok(made['helpModal'].style.display === 'none', 'closeModal 直接关');

// 38.4 扫描确认弹窗：先问再扫（设计稿 maintain('scan') 同款 ——
//      扫描真的会逐个切口，屏幕会黑几次，不能让人莫名其妙看黑屏）
ok(/id="scanModal"/.test(html) && /开始扫描/.test(html) && /暂不扫描/.test(html)
  && /屏幕可能短暂变黑几次/.test(html),
  '扫描确认弹窗在册（黑屏警告 + 两个出口）');
ok(/id="maintScan" onclick="openScanModal\(\)"/.test(html),
  '顶栏扫描钮走确认弹窗（不再直接 post /api/scan）');
try { vm.runInContext('openScanModal()', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('openScanModal 报错：', e.message); process.exit(1); }
ok(made['scanModal'].style.display === '', 'openScanModal 打开确认弹窗');
const scanUrls = [];
ctx.fetch = (url) => { scanUrls.push(url); return Promise.resolve({ ok: true, json: () => Promise.resolve({}) }); };
try { vm.runInContext('confirmScan()', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('confirmScan 报错：', e.message); process.exit(1); }
ok(scanUrls.length === 1 && /\/api\/scan/.test(scanUrls[0])
  && made['scanModal'].style.display === 'none',
  '点「开始扫描」才发 /api/scan，且弹窗随即关上');
scanUrls.length = 0;
try { vm.runInContext('openScanModal(); closeModal("scanModal");', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('暂不扫描报错：', e.message); process.exit(1); }
ok(scanUrls.length === 0 && made['scanModal'].style.display === 'none',
  '「暂不扫描」：一个请求都不发，只关窗');

// 38.5 维护三钮 busy：后端引擎线程串行跑探测类动作，一个没完别接第二个
//      （设计稿 app.js:113 的 data-maintain 同款；连点只是白排队，
//      扫描还会多黑几次屏）。主切换按钮"永远显示"的铁律不适用这三个。
ok(/id="maintRescan"/.test(html) && /id="maintScan"/.test(html)
  && /id="maintCheck"/.test(html),
  '三个维护钮有固定 ID（render.js 按 ID 拨禁用态）');
ok(/\.flat-group button:disabled/.test(html),
  '禁用态有样式（置灰 + 禁光标，不是裸 disabled）');
try { vm.runInContext('snap.maint_busy = true; render();', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('maint_busy render 报错：', e.message); process.exit(1); }
ok(made['maintRescan'].disabled === true && made['maintScan'].disabled === true
  && made['maintCheck'].disabled === true,
  'maint_busy：三个维护钮一起禁用（防连点白排队）');
try { vm.runInContext('snap.maint_busy = false; render();', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('maint_busy render 报错：', e.message); process.exit(1); }
ok(made['maintRescan'].disabled === false && made['maintScan'].disabled === false
  && made['maintCheck'].disabled === false,
  '动作结束：三钮恢复可点（后端 Drop 守卫复位，轮询跟上来）');

// 39. 设备页重构（设计稿 renderDevices 的生产版）：名册列 + 详情双 pane。
//     #peers 富卡片和页首配对大卡已废 —— 对端信息收成名册紧凑行、详细
//     信息搬进 selected-detail；全部既有 ID 保留，摆位交互归 kvm.js。
//     开头造一份干净 fixture：snapW 在 :802 被永久改成"已配对且验证"
//     （工作台场景段落的夹具），这里基于它删掉 paired 两字段回到未配对态，
//     并清掉 37.5 段给 peers[0] 打上的 needs_pair 标记（发送门禁的测试数据）。
const devFix = JSON.parse(JSON.stringify(snapW));
delete devFix.paired;
delete devFix.peer_verified;
devFix.peers.forEach(p => { p.needs_pair = false; });
ctx.__fixture = devFix;
try { vm.runInContext('snap = __fixture; render(); renderKvm(snap);', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('设备页 render 报错：', e.message); process.exit(1); }
const devRoster = made['deviceRows'].innerHTML;
// 39.1 结构：devices-layout 就位，旧结构与死样式清零
ok(/<div class="devices-layout">/.test(html) && /class="device-list"/.test(html)
  && /id="deviceRows"/.test(html) && /id="deviceLayoutPane"/.test(html)
  && /id="devicePairPane"/.test(html),
  '设备页是 devices-layout 结构（名册列 + 详情双 pane）');
ok(!/id="peers"/.test(html) && !/class="pairrow"/.test(html) && !/class="empty-fill"/.test(html),
  '旧 #peers 富卡片 / 配对卡结构已废除');
ok(!/\.pairrow\b|\.peerrow\b|empty-fill/.test(html),
  '死样式（pairrow / peerrow / empty-fill）已清');
// 39.2 名册列：本机行 / 计数 / 三台对端一行一台
ok(made['deviceLocalName'].textContent === 'Mac 本机', '名册列本机行显示本机名');
ok(made['deviceListCount'].textContent === '3', '名册列「已知设备」计数 = 在线对端数');
ok(/data-peer="CHINAMI-0DFF1JU"[\s\S]*?右侧/.test(devRoster)
  && /data-peer="CHINAMI-MC5K9SJ"[\s\S]*?共用一块屏 · 键鼠没开/.test(devRoster)
  && /data-peer="CHINAMI-YET"[\s\S]*?未摆位/.test(devRoster),
  '名册行写摆位与键鼠态：右侧 / 共用一块屏·键鼠没开 / 未摆位');
ok(/onclick="selectDevice\('CHINAMI-0DFF1JU'\)"/.test(devRoster),
  '名册行点击选中（selectDevice，和摆位图共用 kvmSelected）');
// 39.3 默认态：配对摘要 / 摆位提示 / 选中详情
ok(made['pairSummaryText'].textContent === '尚未配对', '未配对：摘要写「尚未配对」');
ok(/共用一块屏与多屏布局可以同时使用/.test(made['placementNote'].innerHTML),
  '没选中：摆位提示是默认文案');
ok(/选择一台设备/.test(made['selectedDetail'].innerHTML)
  && /未配对/.test(made['selectedDetail'].innerHTML),
  '没选中：详情写「选择一台设备」+ 未配对 tag');
// 39.4 选中态：名册高亮 + 三块文字 + 摆位图一起变（纯本地，不等轮询）
try { vm.runInContext("selectDevice('CHINAMI-0DFF1JU')", ctx, { filename: 'panel.html' }); }
catch (e) { console.error('selectDevice 报错：', e.message); process.exit(1); }
ok(/class="device-row selected" data-peer="CHINAMI-0DFF1JU"/.test(made['deviceRows'].innerHTML)
  && /aria-pressed="true"/.test(made['deviceRows'].innerHTML),
  '选中：名册行高亮 + aria-pressed 跟手');
ok(/已选 CHINAMI-0DFF1JU · 点方向放置，Delete 移回未摆位/.test(made['placementNote'].innerHTML),
  '选中：摆位提示换成「已选…点方向放置」');
ok(/192\.168\.0\.41/.test(made['selectedDetail'].innerHTML)
  && /DP1/.test(made['selectedDetail'].innerHTML)
  && /共用键鼠已开/.test(made['selectedDetail'].innerHTML)
  && /发文件/.test(made['selectedDetail'].innerHTML),
  '选中：详情展开地址 + 对口名 + 键鼠态 + 发文件钮（旧 #peers 卡片的信息在这）');
ok(/class="screen sel"/.test(made['kvmGrid'].innerHTML),
  '选中：3×3 摆位图里那台同样亮（两套图同一份 kvmSelected）');
try { vm.runInContext("selectDevice('CHINAMI-0DFF1JU')", ctx, { filename: 'panel.html' }); }
catch (e) { console.error('selectDevice 报错：', e.message); process.exit(1); }
ok(!/device-row selected/.test(made['deviceRows'].innerHTML)
  && /共用一块屏与多屏布局可以同时使用/.test(made['placementNote'].innerHTML)
  && /选择一台设备/.test(made['selectedDetail'].innerHTML),
  '再点一次取消选中：名册和三块文字回默认态');
// 39.5 双 pane 切换与按配对态选默认 pane
try { vm.runInContext("showDevicePane('pair')", ctx, { filename: 'panel.html' }); }
catch (e) { console.error('showDevicePane 报错：', e.message); process.exit(1); }
ok(made['deviceLayoutPane'].hidden === true && made['devicePairPane'].hidden === false,
  'showDevicePane(pair)：布局藏、配对表单显示');
ok(made['pairHelp'].textContent === '最多 32 个字符。建议使用较长的配对码。',
  '未配对：配对表单是 idle 文案');
try { vm.runInContext("showDevicePane('layout')", ctx, { filename: 'panel.html' }); }
catch (e) { console.error('showDevicePane 报错：', e.message); process.exit(1); }
ok(made['deviceLayoutPane'].hidden === false && made['devicePairPane'].hidden === true,
  'showDevicePane(layout)：布局显示、配对表单藏');
// 未配对进设备页 = 落配对表单（第一件事是填码）；已配对且验过 = 落布局
try { vm.runInContext("showPage('devices')", ctx, { filename: 'panel.html' }); }
catch (e) { console.error('showPage 报错：', e.message); process.exit(1); }
ok(made['devicePairPane'].hidden === false && made['deviceLayoutPane'].hidden === true,
  '未配对：进设备页直接落配对表单');
ctx.__pairedFix = Object.assign(JSON.parse(JSON.stringify(devFix)), { paired: true, peer_verified: 2 });
try { vm.runInContext("snap = __pairedFix; render(); showPage('devices');", ctx, { filename: 'panel.html' }); }
catch (e) { console.error('已配对 render 报错：', e.message); process.exit(1); }
ok(made['devicePairPane'].hidden === true && made['deviceLayoutPane'].hidden === false,
  '已配对且验过：进设备页落摆位布局');
ok(made['pairSummaryText'].textContent === '设备身份已验证', '已连接：摘要写「设备身份已验证」');
ok(made['pairHelp'].textContent === '最多 32 个字符。建议使用较长的配对码。'
  && made['code'].getAttribute('readonly') === null && made['pairSaveBtn'].disabled === false,
  '已连接：表单回 idle（码可改、主钮可点）');
// waiting：码已保存、对端没验过 —— 只读 + 禁用 + aria-busy
ctx.__waitingFix = Object.assign(JSON.parse(JSON.stringify(devFix)), { paired: true, peer_verified: 0 });
try { vm.runInContext("snap = __waitingFix; render();", ctx, { filename: 'panel.html' }); }
catch (e) { console.error('waiting render 报错：', e.message); process.exit(1); }
ok(made['pairHelp'].textContent === '配对码已保存，正在等待对端确认身份…'
  && made['code'].getAttribute('readonly') === '' && made['code'].getAttribute('aria-busy') === 'true'
  && made['pairSaveBtn'].disabled === true,
  'waiting：表单只读 + 主钮禁用 + aria-busy（等对端确认期间不改码）');
ok(made['pairSummaryText'].textContent === '配对码已填 · 等对端确认', 'waiting：摘要写「等对端确认」');
// 39.6 时序坑（Edge 截图验收抓到的真 bug）：showPage('devices') 比首次
//     pull() 先跑时 snap 还是 null —— 只挂欠账不选 pane，render() 首跑
//     后按配对态补选。未配对的人不能打开面板停在摆位页、自己找「管理配对」。
try { vm.runInContext("showPage('workspace'); snap = null; showPage('devices');", ctx, { filename: 'panel.html' }); }
catch (e) { console.error('showPage(snap=null) 报错：', e.message); process.exit(1); }
ok(made['deviceLayoutPane'].hidden === false && made['devicePairPane'].hidden === true,
  'snap 未到位：进设备页只挂欠账，pane 保持原样不抛错');
try { vm.runInContext('snap = __fixture; render();', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('pull 到位 render 报错：', e.message); process.exit(1); }
ok(made['devicePairPane'].hidden === false && made['deviceLayoutPane'].hidden === true,
  'pull 到位后 render() 补选：未配对落配对表单（不等用户手点管理配对）');
// 用户手动切过的不被轮询抢回：waiting 态自动选会是 pair，手动切 layout 后 render 必须保持
try { vm.runInContext("showDevicePane('layout', true); snap = __waitingFix; render();", ctx, { filename: 'panel.html' }); }
catch (e) { console.error('手动切 render 报错：', e.message); process.exit(1); }
ok(made['deviceLayoutPane'].hidden === false && made['devicePairPane'].hidden === true,
  '用户手动切过：轮询 render 不按配对态抢回（byUser 销账）');
// 还原干净 fixture 给收尾段（placeOn 的 undo toast 只依赖 links）
try { vm.runInContext('snap = __fixture; render(); renderKvm(snap);', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('还原 fixture 报错：', e.message); process.exit(1); }

// 40. 设备页响应式（设计稿 @container 720 / 520 的移植，按生产窗口档
//     720 / 600 落地）。媒体查询桩环境不生效，按源码钉规则在位；
//     窄窗语义：720 名册压行+「连接新设备」让位（配对入口还有「管理配对」），
//     600 名册横排成设备带、对端行 39px 纯文字、添加入口收成图标钮。
ok(/@media \(max-width: 720px\) \{[\s\S]*?\.device-list \.device-row \{ min-height: 48px; \}/.test(html),
  '720 档：名册对端行压到 48px（本机行 42px 同档）');
ok(/@media \(max-width: 720px\) \{[\s\S]*?\.device-list \.add-device \{ display: none; \}/.test(html),
  '720 档：「连接新设备」让位（配对入口还有名册底的「管理配对」）');
ok(/@media \(max-width: 720px\) \{[\s\S]*?\.detail-heading p \{ display: none; \}/.test(html),
  '720 档：详情列说明文字收行');
ok(/@media \(max-width: 720px\) \{[\s\S]*?\.loosetray \{ min-height: 32px; padding-block: 3px; \}/.test(html),
  '720 档：摆位托盘压矮');
ok(/@media \(max-width: 600px\) \{[\s\S]*?\.devices-layout \{ grid-template-columns: 1fr; gap: 9px; \}/.test(html),
  '600 档：名册列与详情列改单列堆叠');
ok(/@media \(max-width: 600px\) \{[\s\S]*?\.device-list \.list-label, \.device-list \.local-row, \.pair-summary \{ display: none; \}/.test(html),
  '600 档：本机行/标签/配对摘要让位，名册横排成设备带');
ok(/@media \(max-width: 600px\) \{[\s\S]*?#deviceRows \{ display: flex; gap: 6px; align-items: center; \}/.test(html),
  '600 档：横排落到 #deviceRows（.device-list 的直接子元素是行容器，只 flex 容器层横排的是整块）');
ok(/@media \(max-width: 600px\) \{[\s\S]*?\.device-row \{ padding: 5px; min-height: 39px; margin: 0; width: auto; flex: none; \}/.test(html),
  '600 档：对端行压成 39px + width:auto/flex:none（不被基础规则 width:100% 挤缩）');
ok(/@media \(max-width: 600px\) \{[\s\S]*?\.device-list \.add-device \{ display: inline-flex; margin: 0; width: 30px; padding: 3px; font-size: 0; \}/.test(html),
  '600 档：「连接新设备」收成 30px 图标钮（同具体性压过 720 档的 display:none）');

(async () => {
  try { vm.runInContext("placeOn('right', 'CHINAMI-ZZZZ')", ctx, { filename: 'panel.html' }); }
  catch (e) { console.error('placeOn 报错：', e.message); process.exit(1); }
  // 微任务队列跑干：placeOn 的 await 续体（pull + flash 都在里面）
  await new Promise(r => setImmediate(r));
  ok(/已放到右侧/.test(made['status'].innerHTML) && /撤销/.test(made['status'].innerHTML),
    'placeOn 成功 flash 挂「撤销」（设计稿 placePeer 的 undo toast）');
  console.log(bad ? `\n${bad} 项不通过` : '\n全部通过');
  process.exit(bad ? 1 : 0);
})();

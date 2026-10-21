// 面板 UI 的离线校验：不起浏览器、不开 WebView。
// 一、把 panel.html 里的 <script> 抽出来做语法检查（node --check 等价）
// 二、塞进一个极简 DOM 桩里真的跑一遍 renderKvm，断言渲染出来的 HTML
//
// 用法：node tools/panelcheck.js assets/panel.html
//
// 为什么不用真浏览器：本机 Chromium/WebView 在沙箱里跑 headless 会被
// 拒（code_sign_clone 目录不能删），截图这条路走不通。这个桩断言的是
// **渲染出来的字符串**，虽然粗糙，但"卡片跑进哪一栏、标签写的是什么"
// 这类最容易说谎的地方正好都是字符串。
const fs = require('fs');
const vm = require('vm');

const html = fs.readFileSync(process.argv[2], 'utf8');
const blocks = [...html.matchAll(/<script>([\s\S]*?)<\/script>/g)].map(m => m[1]);
if (!blocks.length) { console.error('没找到 <script>'); process.exit(1); }
const code = blocks.join('\n;\n');

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
      contains(c) { return this._s.has(c); }, toggle(c) { this._s.has(c) ? this._s.delete(c) : this._s.add(c); },
    },
    appendChild(c) { this.children.push(c); return c; },
    removeChild() {}, remove() {}, focus() {}, blur() {},
    setAttribute(k, v) { this.dataset[k] = v; }, removeAttribute() {},
    getAttribute() { return null; },
    addEventListener() {}, removeEventListener() {},
    querySelector() { return null; }, querySelectorAll() { return []; },
    getBoundingClientRect() { return { left: 0, top: 0, width: 10, height: 10 }; },
    closest() { return null; },
    setPointerCapture() {}, releasePointerCapture() {},
    getContext() { return null; },
  };
  return el;
}
const ids = ['kvmGrid', 'kvmLooseBox', 'kvmGridHint', 'kvmLinks', 'kvmLinkHint',
  'kvmFlash', 'kvmState', 'kvmHandBtn', 'kvmHandHint', 'peers', 'inbox', 'transfer',
  'display', 'identity', 'support', 'status', 'hotkey', 'target', 'port', 'node',
  'permCapture', 'permInject', 'pairState', 'clipboard', 'autostart', 'main',
  'sub', 'advice', 'peersHint', 'displayNote', 'kvmFollow', 'kvmEdge', 'kvmModeHint'];
ids.forEach(id => { made[id] = mkEl(id); });

const document = {
  getElementById: (id) => made[id] || (made[id] = mkEl(id)),
  createElement: (tag) => mkEl('new-' + tag),
  querySelector: () => null,
  querySelectorAll: () => [],
  addEventListener: () => {},
  body: mkEl('body'),
  activeElement: null,
  documentElement: mkEl('html'),
};

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
  localStorage: { getItem: () => null, setItem: () => {} },
  navigator: { clipboard: { writeText: () => Promise.resolve() } },
  location: { href: '' },
  alert: () => {},
};
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
};

ctx.snap = snap;
ctx.__fixture = snap;
try { vm.runInContext('snap = __fixture; render(); renderKvm(snap);', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('renderKvm 报错：', e.message); process.exit(1); }

const grid = made['kvmGrid'].innerHTML;
const loose = made['kvmLooseBox'].innerHTML;
const links = made['kvmLinks'].innerHTML;
// 对端卡片是 createElement + appendChild 拼出来的，innerHTML 在子节点上
const peers = made['peers'].children.map(c => c.innerHTML).join('\n') + made['peers'].innerHTML;
const handBtn = made['kvmHandBtn'].style.display;

let bad = 0;
function ok(cond, label) {
  console.log((cond ? '✓ ' : '✗ ') + label);
  if (!cond) bad++;
}

// 1. 有门的对端必须落在格子里（右侧），不能进托盘
ok(/CHINAMI-0DFF1JU/.test(grid) && !/CHINAMI-0DFF1JU/.test(loose),
  '0DFF1JU（有门/右侧）画在格子里，不在托盘');
ok(/右侧/.test(grid), '格子上标了「右侧」');
// 2. 没有门的对端（真 follow 链路）进托盘，标签是「没有门」
ok(/data-node="CHINAMI-MC5K9SJ"/.test(loose), 'MC5K9SJ（follow）画在托盘里');
ok(/title="CHINAMI-MC5K9SJ · 没有门/.test(loose), 'MC5K9SJ 的标签是「没有门」');
// 3. 未配链路的对端不能也叫「没有门」
ok(/data-node="CHINAMI-YET"/.test(loose), '未配链路的对端也在托盘里');
ok(/title="CHINAMI-YET · 未摆位/.test(loose), '未配链路的标签是「未摆位」，不是「没有门」');
ok(!/title="CHINAMI-YET · 没有门/.test(loose), '未配链路的没有被冒认成共用一块屏');
// 4. 链路行：不通的时候说清是"键鼠没开"而不是含糊的"没回应"
ok(/CHINAMI-MC5K9SJ[\s\S]*?共用键鼠没开/.test(links), '链路行点破「它的共用键鼠没开」');
ok(/CHINAMI-0DFF1JU[\s\S]*?右侧 · 多屏共享/.test(links), '链路行照旧显示「右侧 · 多屏共享」');
ok(!/没回应/.test(links.replace(/共用键鼠没开/g, '')), '不该出现含糊的「没回应」');
// 5. 对端卡片带上键鼠开关状态
ok(/CHINAMI-MC5K9SJ[\s\S]*?共用键鼠没开/.test(peers), '局域网对端卡片标出「共用键鼠没开」');
ok(/CHINAMI-0DFF1JU[\s\S]*?共用键鼠已开/.test(peers), '开着的对端标「共用键鼠已开」');
// 6. 有 follow 链路时，那个"交给对端"按钮必须藏掉（画面一走就点不到）
ok(handBtn === 'none', '有 follow 链路时隐藏「现在就把键鼠交给对端」');

// ---- 第二幕：会话中（键鼠已从共用屏那台转到并排那台）----
// 这一段检查 v0.6.60 才有的两件事：链路行的按钮该说「转给它」而不是
// 「交给它」，摆位图上起点那台要标「起点」（对面推回来会落到这里）。
const snap2 = JSON.parse(JSON.stringify(snap));
snap2.kvm.state = '键鼠已交给对端';
snap2.kvm.active = 'CHINAMI-0DFF1JU';
snap2.kvm.root = 'CHINAMI-MC5K9SJ';
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
// 9. 接收手感档位：高亮当前那一档（fixture 里是 1.1），其余是 ghost
ok(/green/.test(made['kvmScale11'].className), '倍率 1.1 那一档高亮');
ok(/ghost/.test(made['kvmScale10'].className), '1.0 那一档没有高亮');
ok(/x1\.1/.test(made['kvmScaleHint'].textContent), '提示里写清当前是 x1.1');

console.log(bad ? `\n${bad} 项不通过` : '\n全部通过');
process.exit(bad ? 1 : 0);

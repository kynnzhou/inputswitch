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
    removeChild() {}, remove() {}, focus() {}, blur() {},
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
const ids = ['kvmGrid', 'kvmLooseBox', 'kvmGridHint', 'kvmLinks', 'kvmLinkHint',
  'kvmFlash', 'kvmState', 'kvmHandBtn', 'kvmHandHint', 'peers', 'inbox', 'transfer',
  'display', 'identity', 'support', 'status', 'hotkey', 'target', 'port', 'node',
  'permCapture', 'permInject', 'pairState', 'clipboard', 'autostart', 'main',
  'sub', 'advice', 'peersHint', 'displayNote', 'kvmFollow', 'kvmEdge', 'kvmModeHint'];
ids.forEach(id => { made[id] = mkEl(id); });

// P2：壳体的 .app 要能被 querySelector 拿到（updateNavCompact 量它的宽）。
// 假 app 带 clientWidth（默认 800 —— panelwin.rs 窗口 820，走图标栏档）
// 和真 classList；断言里改 clientWidth 再调函数，就能验宽档切换。
const appEl = mkEl('app');
appEl.clientWidth = 800;
const document = {
  getElementById: (id) => made[id] || (made[id] = mkEl(id)),
  createElement: (tag) => mkEl('new-' + tag),
  querySelector: (sel) => (sel === '.app' ? appEl : null),
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
// 9. 接收手感档位：高亮当前那一档（fixture 里是 1.1），其余不高亮。
//    v0.6.83 起是分段控件：选中项挂 .sel（以前是 green / ghost 两个按钮）。
ok(made['kvmScale11'].classList.contains('sel'), '倍率 1.1 那一档高亮');
ok(!made['kvmScale10'].classList.contains('sel'), '1.0 那一档没有高亮');
ok(/×1\.1/.test(made['kvmScaleHint'].textContent), '提示里写清当前是 ×1.1');
// 10. 「收回键鼠」按钮已删：发送中光标被钉死点不到，被控时按一下鼠标就已
//     物理抢回 —— UI 上留着只会误导。接口 /api/kvm-takeback 仍保留给脚本。
ok(!/onclick="post\('\/api\/kvm-takeback'\)"/.test(html), '页面上不再有「收回键鼠」按钮');
ok(/Ctrl\+Alt\+Esc/.test(made['kvmHandHint'].textContent || '') ||
   /Ctrl\+Alt\+Esc/.test(html), '页面上写清了真正能用的收回方式');
// 11. 键鼠已经在别台手上时，交接按钮同样藏掉（点不到）。
//     按钮藏了就不再留说明 —— v0.6.83 起那句「转给它」的提示一并去掉，
//     它和上面 kvmHint 的机制解释重复，卡片里会出现两遍同样的话。
ok(made['kvmHandBtn'].style.display === 'none', '会话中（active 非空）也隐藏交接按钮');
ok((made['kvmHandHint'].textContent || '') === '', '按钮藏起来时说明也一起收掉');
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
ok(/3 台在线/.test(made['hdrPeer'].innerHTML), '状态条显示在线台数');
ok(made['hdrKeys'].textContent === 'CHINAMI-0DFF1JU', '状态条显示键鼠现在在谁手上');
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
//     以前说明里让人点，页面上却没有这个按钮）
ok((html.match(/onclick="post\('\/api\/scan'\)"/g) || []).length === 1, '「扫描输入口」恰好一个入口');
ok((html.match(/onclick="post\('\/api\/rescan'\)"/g) || []).length === 1, '「重新检测」只有一个入口');
ok((html.match(/onclick="post\('\/api\/check'\)"/g) || []).length === 1, '「检测能否切换」只有一个入口');
// 18. 顶栏只有一个实心蓝（主按钮），维护按钮全是描边 —— 这是 B 方案的约定
const topbar = (html.split('<header class="topbar glass">')[1] || '').split('</header>')[0];
ok((topbar.match(/class="[^"]*primary/g) || []).length === 1, '顶栏只有主按钮一个是实心蓝');
ok(/id="mainSwitch"[^>]*class="primary btn-lg"/.test(topbar), '实心蓝那个就是 mainSwitch');
// 18b. 全页也只许这一个实心蓝（v0.6.85）：配对卡的「保存并连接」和共用键鼠的
//      「开启」以前也是 primary，一屏三个蓝块，主按钮的焦点地位被稀释。
//      P3 例外：更新弹窗/过渡层里的主操作。遮罩盖上时顶栏主按钮根本看不见，
//      「任何一刻屏上只有一个蓝块」的约定没破 —— 破的只是"蓝按钮只能出现在
//      静态骨架里"这个实现细节。判据按行算：带 primary 的行必须同时带
//      mainSwitch（静态那一个）或 updateAction（弹窗动态生成的那些）。
const blueLines = html.split('\n').filter(l => /class="[^"]*\bprimary\b/.test(l));
ok(blueLines.length >= 1 && blueLines.every(l => /mainSwitch|updateAction/.test(l)),
  '实心蓝只有顶栏主按钮和更新弹窗主操作（弹窗开着时顶栏那个被遮住）');
ok(/id="supportLine"/.test(topbar), '支持性说明在顶栏（只在不是 ✓ 时出现）');
// 19. 对端行用 grid（名字长了不会把「发文件」挤到中间）
ok(/\.peerrow/.test(html), '对端行是 grid 布局');
// 16. 输入口可以就地改名（labels 以前只能手改配置文件，看着像写死的）。
//     行是 JS 拼的，桩里的 appendChild 不聚合 innerHTML，只能查源码。
ok(/function saveLabel/.test(html) && /\/api\/label/.test(html) && /contenteditable/.test(html),
  '输入口名字可就地编辑（saveLabel + /api/label）');
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
try { vm.runInContext('snap = __fixture; render();', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('第三幕 render 报错：', e.message); process.exit(1); }
const rowsHtml = made['ports'].innerHTML;
// 按行切开再找：整段正则会被行内的嵌套 </div> 提前截断。
// 注意别按文案找 —— 「该口不存在」也出现在别的行的注释里。
const rowOf = (cls) => (rowsHtml.split('<div class="port').find(r => r.startsWith(' ' + cls)) || '');
const deadRow = rowOf('dead');
ok(/<span class="tag plain">该口不存在<\/span>/.test(deadRow), '不存在的口标了「该口不存在」');
ok((deadRow.match(/disabled/g) || []).length >= 2,
  '不存在的口：切换和设为热键都 disabled');
ok(/title="显示器固件不接受这个输入口"/.test(deadRow), '不存在的口给了解释');
// 有信号那行的 class 是 "port"（没有 cur / dead 修饰）
const liveRow = (rowsHtml.split('<div class="port').find(r => r.startsWith('"'))
  || '').split(/<div class="port/)[0] || '';
ok(/HDMI-1/.test(liveRow) && (liveRow.match(/disabled/g) || []).length === 0,
  '有信号的口两个按钮都能点');

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
ok((html.match(/class="page-view"/g) || []).length === 4, '四页各一个 .page-view');
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
// 未配对：页脚转黄，横幅给「去配对」直达设备页
ctx.__fixture = snap;   // 原夹具没有 paired 字段 → 未配对
try { vm.runInContext('snap = __fixture; render();', ctx, { filename: 'panel.html' }); }
catch (e) { console.error('P2 render 报错：', e.message); process.exit(1); }
ok(made['footerConnection'].className === 'footer-connection off'
  && /未配对/.test(made['footerConnection'].innerHTML), '未配对时页脚转黄');
ok(/onclick="showPage\('devices'\)"/.test(made['pairBanner'].innerHTML),
  '未配对横幅带「去配对」直达按钮（跳设备页）');

console.log(bad ? `\n${bad} 项不通过` : '\n全部通过');
process.exit(bad ? 1 : 0);

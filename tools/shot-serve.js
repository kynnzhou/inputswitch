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
  }, 1500);
})();
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

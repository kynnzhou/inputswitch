// 动作层：所有会发 /api/* 请求的入口（切换、热键目标、配对、开关、改名、
// 改键……），以及页面自己的启动轮询。函数大多挂在 index.html 的
// onclick 上，全局查找，跨文件引用没问题。
function switchTo(value) {
  const main = document.getElementById('mainSwitch');
  // 切换中是"进行态"：图标换 refresh + busy-icon（旋转），跟设计稿一致。
  // innerHTML 而不是 textContent —— 后者会把 render() 画上去的图标抹掉。
  main.innerHTML = icon('refresh', 'busy-icon') + '<span>切换中…</span>';
  api('/api/switch?value=' + value).then(() => setTimeout(pull, 700));
}
function setTarget(value) { api('/api/target?value=' + value).then(pull); }

function quickSwitch() {
  const main = document.getElementById('mainSwitch');
  // 「现在切不了」也要给回音：点一个没有任何反应的按钮，用户会以为程序坏了
  if (main.classList.contains('is-off')) {
    if (!snap.has_display && !(snap.peers || []).length) {
      return flash('暂时切不回来', '对端不在线，得由正在显示画面的那台发指令');
    }
    return flash('这台屏没有可切的口', '先点「扫描输入口」确认别的口上有没有主机');
  }
  const value = Number(main.dataset.value || 0);
  if (!value) return;
  // 本机没有显示器时，切换指令得由对端去发（它那边 DDC 才是通的）
  if (main.dataset.action === 'peer') return askPeer(value);
  switchTo(value);
}

function askPeer(value) {
  const main = document.getElementById('mainSwitch');
  // 同样是进行态：refresh + busy-icon。innerHTML（同 switchTo）。
  main.innerHTML = icon('refresh', 'busy-icon') + '<span>已请对端切换…</span>';
  main.disabled = true;
  api('/api/ask-peer?value=' + value).then(() => setTimeout(pull, 1500));
}
function genCode() {
  const code = String(Math.floor(Math.random() * 1000000)).padStart(6, '0');
  document.getElementById('code').value = code;
}
function savePair() {
  const code = document.getElementById('code').value.trim();
  codeDirty = false;
  api('/api/pair', { code }).then(pull);
}
function unpair() {
  const ok = confirm(
    '取消配对后，本机不再验证对方身份，同一网段里的任何设备都能控制这块屏。\n\n' +
    '如果对端还留着配对码，它会拒绝本机发出的所有指令（包括请它把画面切回来）。\n' +
    '要么两边都取消，要么重新填同一个码。\n\n确定取消配对吗？'
  );
  if (!ok) return;
  codeDirty = false;
  document.getElementById('code').value = '';
  api('/api/pair', { code: '' }).then(pull);
}
function toggleKvm() {
  const on = snap && snap.kvm && snap.kvm.on;
  api('/api/kvm?on=' + (on ? 'false' : 'true')).then(pull);
}
// 模式已下沉到每台对端（由摆位决定），面板上不再有全局模式开关。
// 角色（键鼠插在哪台）已经没有 UI 入口了：role 不参与行为决策，
// 只影响一句状态文案（v0.6.68）。/api/kvm-role 保留给 CLI 与老调用方。

// 接收位移倍率。后端会重建共用键鼠实例（先把键鼠交还再停），所以点完
// 会话会断一下 —— 面板轮询会把新状态刷回来。
function setKvmScale(v) { api('/api/kvm-scale?value=' + v).then(pull); }

function toggleAutostart() { api('/api/autostart', { on: !snap.autostart }).then(pull); }
// 输入口改名走工作台「管理输入源」弹窗的批量保存（savePorts，见 P4 组）——
// 行内 contenteditable 那套（saveLabel / editingPort）随旧 #ports 列表一起删了。
function toggleAutoResume() {
  api('/api/kvm-autoresume', { on: !((snap.kvm || {}).auto_resume) }).then(pull);
}
function toggleKeepAlive() {
  api('/api/kvm-keepalive', { on: !((snap.kvm || {}).keep_alive) }).then(pull);
}
// 副机模式：三项一起开/关。判据用 auto_resume（它是这个组合里最"专有"的一项）。
function toggleSubmachine() {
  api('/api/submachine', { on: !((snap.kvm || {}).auto_resume) }).then(pull);
}
function toggleClipboard() { api('/api/clipboard', { on: !snap.clipboard }).then(pull); }
// 改键：网页里只能收到"焦点在这个页面上"的按键，录不出全局热键，
// 所以让主程序弹它自己的小窗口来收，录完这里轮询会自动刷新。
function recHotkey() {
  api('/api/hotkey-record').then(() => {
    document.getElementById('hotkey').textContent = '请到刚弹出的窗口里按…';
  });
}

// ── P4 工作台场景（设计稿 data-scene / data-switch-to / ports 的生产版）──
// 场景分段器（单屏 / 多屏）是内存态、不持久化（设计稿 state.scene 同款）：
// 它只是"当前怎么看这块屏"，不是配置。换挡只切 hidden 和选中态，
// 内容由 renderWorkspaceScene 当场重画，不等下一轮轮询。

/// 场景切换：分段器 onclick。切完补一次 renderKvm —— 摆位图只在多屏
/// 场景里，轮询 1.2s 才收敛太慢，点下去要当场看见图。
function setWorkspaceScene(v) {
  workspaceUi.scene = (v === 'multi') ? 'multi' : 'single';
  renderWorkspaceScene();
  renderKvm(snap.kvm || {});
}

/// 单屏场景的卡片切换：local = 画面回本机，peer = 去对端。
/// 安全边界：只信本机 inputs 的 value —— 对端自报的 value 是它自己
/// 那块屏的编号，两块屏的 0x0F/0x12 各算各的，拿别人的编号切自己的
/// 屏 = 黑屏（和顶栏主按钮同一套判据）。blind（本机没枚举到显示器）
/// 时指令得由对端发（ask-peer）：它那边 DDC 才是通的。
function wsSwitch(who) {
  if (!snap) return;
  if (who === 'local') {
    if (snap.has_display !== false) {
      return flash('画面已经在本机', '要看对端就点右边那张卡');
    }
    // 没显示器：请对端把画面切回本机（值 = 本机输入口号）
    const value = snap.own_value || 0;
    if (!value) return flash('还回不了本机', '没读到本机的输入口号，先扫描一次');
    if (!(snap.peers || []).some(p => p.knows_port)) {
      return flash('还回不了本机', '对端还不知道本机的输入口号，先在设备页完成一次同步');
    }
    return askPeer(value);
  }
  if (who === 'peer') {
    if (snap.has_display === false) {
      return flash('这台屏不在本机', '画面在对端，切换要由正在显示画面的那台来操作');
    }
    const peerPort = (snap.inputs || []).find(i => !i.current && i.presence === 'occupied');
    if (!peerPort) {
      return flash('对端没有画面', '先去「管理输入源」里确认哪个口接着对端主机');
    }
    return switchTo(peerPort.value);
  }
}

/// 多屏场景的键鼠交接：交给第一条活链路指向的那台（和设备页链路行的
/// 「交给它」同一个动作）。按钮的藏匿规则在 multiSceneHtml 里
/// （跟随画面 / 会话中 / 没有活链路时根本不画），这里兜底只说一句。
function wsHandOver() {
  const k = (snap && snap.kvm) || {};
  const link = (k.links || []).find(l => l.alive);
  if (!link) {
    return flash('现在交接不了', '没有活跃的键鼠链路 —— 先在设备页把设备摆到某台对端的方位上');
  }
  handOverTo(link.node);
}

/// 管理输入源弹窗：开合只切遮罩 display（和 helpModal / diagModal 同款）。
/// editingPorts 期间轮询不重建列表 —— 用户在打字，重建会把输入框换掉
/// （同 codeDirty 的套路，见 render() 末尾的守卫）。
function openPortsModal() {
  editingPorts = false;
  renderPortsModal();
  document.getElementById('portsModal').style.display = '';
}
function closePortsModal() {
  editingPorts = false;
  document.getElementById('portsModal').style.display = 'none';
}

/// 保存改名：只打改过的口，全部回来再拉一次。没改动也关弹窗，
/// 但一句话说清"什么都没变"，免得以为保存失败。清空 = 恢复默认名
/// （后端 label_for 的兜底：/api/label 空 label 即删掉自定义名）。
function savePorts() {
  // 选择器自带 #portsList 前缀，走 document 一级的查找（不依赖容器节点）
  const fields = document.querySelectorAll('#portsList input[data-port-name]');
  const inputs = (snap && snap.inputs) || [];
  const edits = [];
  for (let i = 0; i < fields.length; i++) {
    const el = fields[i];
    const name = el.dataset.portName || '';
    const text = (el.value || '').replace(/\s+/g, ' ').trim();
    const cur = inputs.find(x => x.name === name);
    if (!cur || text === (cur.label || '')) continue;
    edits.push({ name: name, label: text });
  }
  editingPorts = false;
  if (!edits.length) {
    closePortsModal();
    return flash('名称没有改动', '要恢复默认名就把那一栏清空');
  }
  Promise.all(edits.map(e => api('/api/label', e))).then(() => {
    closePortsModal();
    pull();
  });
}

// ── P3 传输页动作（设计稿 startTransfers / transferAction 的生产版）──
// 设计稿的传输是前端演示假数据；生产三个入口都打到后端：/api/send（选文件）
// / /api/send-folder（选文件夹）/ /api/send-test（后端生成临时探针 txt，
// 发完即删）。node 参数可省 —— 后端有自己的选路规则（点名 > 键鼠手握 >
// 唯一在线，多台在线又没会话会拒绝），带了就以选择器为准。

/// 选择器当前选的对端。空 = 让后端自己选路。
function transferNode() {
  const sel = document.getElementById('recipient');
  return sel && sel.value ? '?node=' + encodeURIComponent(sel.value) : '';
}
/// 换对端：记进 transferUi，拖放区标题和配对标跟手变（不等下一轮轮询）
function setTransferRecipient(node) {
  transferUi.recipient = node;
  renderTransfers();
}
/// 三个发送入口。发完都跳到传输页 —— 接下来要看的是列表，不是还停在
/// 拖放区（设计稿 startTransfers 里的 navigate('transfers') 同款）。
function sendFilesTo() { post('/api/send' + transferNode()); showPage('transfers'); }
function sendFolderTo() { post('/api/send-folder' + transferNode()); showPage('transfers'); }
function sendTestTo() { post('/api/send-test' + transferNode()); showPage('transfers'); }
/// 传输列表控制：暂停/继续/取消/重试。阶段合法性由后端注册表自己判
/// （对终态任务的操作返回 false，前端看到的就是"点了没反应"——
/// 那种任务本就不可操作，比假回 ok:true 诚实）。
function transferAction(id, action) {
  api('/api/transfer?action=' + action + '&id=' + id).then(pull);
}
/// 筛选组（设计稿 data-filter 的生产版）：换挡回第一页，按钮 .selected
/// 和 aria-pressed 同步（读屏软件靠 aria-pressed 认选中）
function setTransferFilter(v) {
  const map = { all: 'filterAll', active: 'filterActive', done: 'filterDone' };
  if (!(v in map)) v = 'all';
  transferUi.filter = v;
  transferUi.page = 0;
  Object.keys(map).forEach(k => {
    const b = document.getElementById(map[k]);
    if (!b) return;
    b.classList.toggle('selected', k === v);
    b.setAttribute('aria-pressed', String(k === v));
  });
  renderTransferList();
}
/// 翻页。页数从当前筛选算，越界夹住 —— 筛选掉一半之后页码可能已经越界了。
function transferPage(delta) {
  const pages = Math.max(1, Math.ceil(visibleTransfers().length / transferPageSize()));
  transferUi.page = Math.min(Math.max(transferUi.page + delta, 0), pages - 1);
  renderTransferList();
}

// 拖放：设计稿的 dropZone 收文件。生产适配：WebView 里 event.dataTransfer
// 只有 name/size、拿不到本地路径（浏览器安全模型），照设计稿直接发是发不
// 出去的 —— 所以 drop 降级成触发系统选择器（和点「选择文件」同一条路），
// drag-over 高亮照原样做，松手时一句话说清为什么弹的是选择器。
(function initTransferDrop() {
  if (typeof document === 'undefined' || !document.addEventListener) return;
  const zoneOf = (event) => (event.target && event.target.closest
    ? event.target.closest('#dropZone') : null);
  document.addEventListener('dragover', (event) => {
    const zone = zoneOf(event);
    if (!zone) return;
    event.preventDefault();
    zone.classList.add('drag-over');
    if (event.dataTransfer) event.dataTransfer.dropEffect = 'copy';
  });
  document.addEventListener('dragleave', (event) => {
    const zone = zoneOf(event);
    if (zone && !zone.contains(event.relatedTarget)) zone.classList.remove('drag-over');
  });
  document.addEventListener('drop', (event) => {
    const zone = zoneOf(event);
    if (!zone) return;
    event.preventDefault();
    zone.classList.remove('drag-over');
    flash('请选择要发送的文件', '网页里松手拿不到文件路径，改用系统选择器');
    sendFilesTo();
  });
})();

// 帮助 / 排查弹窗：静态内容，开合只切遮罩的 display（和 update.js 的
// openUpdate/closeUpdate 同一个路子）。两个都是 body 级 fixed 遮罩，
// z-index 40 自然盖过壳体；同一时刻只开一个，帮助转排查时先关帮助。
function openHelp() {
  document.getElementById('helpModal').style.display = '';
}
function closeHelp() {
  document.getElementById('helpModal').style.display = 'none';
}
function openDiagnostics() {
  document.getElementById('diagModal').style.display = '';
}
function closeDiagnostics() {
  document.getElementById('diagModal').style.display = 'none';
}

// 启动：先拉一次，之后自调度轮询（节奏随下载状态变，见 core.js）
pull();
schedulePull();
// P2：侧栏宽档取初值，之后跟窗口宽度走。ResizeObserver 缺失时
// （老 WebView2 / 自测 DOM 桩）保持默认图标栏档 —— 820px 窗口
// 本来就走那档，行为不破；用户把窗口拖宽超过 950px 才需要它。
updateNavCompact();
const appBox = document.querySelector('.app');
if (appBox && typeof ResizeObserver === 'function') {
  new ResizeObserver(() => {
    updateNavCompact();
    // 设计稿 updateDimensions 的同款动作：窗口高度跨过 600px 时传输列表的
    // 每页条数在 1/2 之间换挡，要立刻重画一次（轮询 1.2s 才收敛太慢，
    // 拖窗口的过程中列表会跳）。renderTransferList 的内容缓存会挡掉
    // "没变就不碰 DOM"，所以这里无条件调也是安全的。
    const view = document.getElementById('transfersView');
    if (view && !view.hidden) renderTransferList();
  }).observe(appBox);
}
// P2.5：主题三态 + 减少动态效果。applyTheme 读 localStorage（默认
// system，跟 P2.5 之前直接跟系统的行为一致），头内脚本已经落过首屏值，
// 这里再同步一次分段控件选中态。system 档要跟 Windows 深浅色变化重解析；
// matchMedia 缺失（老 WebView2 / 自测 DOM 桩）时静默跳过。
applyTheme();
applyReduceMotion();
if (typeof matchMedia === 'function') {
  const mq = matchMedia('(prefers-color-scheme: dark)');
  if (mq && typeof mq.addEventListener === 'function') {
    mq.addEventListener('change', () => {
      if (storedTheme() === 'system') applyTheme();
    });
  }
}

// 动作层：所有会发 /api/* 请求的入口（切换、热键目标、配对、开关、改名、
// 改键……），以及页面自己的启动轮询。函数大多挂在 index.html 的
// onclick 上，全局查找，跨文件引用没问题。
function switchTo(value) {
  const main = document.getElementById('mainSwitch');
  main.textContent = '切换中…';
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
  main.textContent = '已请对端切换…';
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
// 输入口改名：就地编辑，回车/失焦保存，清空 = 恢复默认名。
// `labels` 以前只能手改配置文件，所以 `USB-C（这台 Mac）` 看着像写死的。
function saveLabel(el) {
  // 失焦即结束编辑。轮询从这个点开始重新接管 #ports 的重建。
  editingPort = false;
  const name = el.dataset.input || '';
  const text = (el.textContent || '').replace(/\s+/g, ' ').trim();
  if (!name || text === (el.dataset.orig || '')) return;
  api('/api/label', { name: name, label: text }).then(pull);
}
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

// 启动：先拉一次，之后自调度轮询（节奏随下载状态变，见 core.js）
pull();
schedulePull();

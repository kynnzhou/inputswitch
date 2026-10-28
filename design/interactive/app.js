(function () {
  'use strict';

  const $ = (selector, root) => (root || document).querySelector(selector);
  const $$ = (selector, root) => Array.from((root || document).querySelectorAll(selector));
  const escapeHtml = value => String(value).replace(/[&<>"']/g, character => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[character]));
  const icon = (name, extra) => '<svg class="icon ' + (extra || '') + '" aria-hidden="true"><use href="#i-' + name + '"/></svg>';
  const keys = value => value.split('+').map(key => '<kbd>' + escapeHtml(key) + '</kbd>').join('');
  const storageKey = 'inputswitch-design-preview-v1';
  const defaults = { kvm: true, clipboard: true, speed: 1.1, autostart: true, resume: true, guard: false, theme: 'light', reducedMotion: false, hotkey: 'Ctrl+Alt+S' };
  const peers = { mac: { name: 'Mac mini', os: 'macOS', address: '192.168.1.108', icon: 'monitor' }, think: { name: 'ThinkPad', os: 'Windows', address: '192.168.1.112', icon: 'laptop' } };
  const directions = { up: '上方', left: '左侧', right: '右侧', down: '下方', follow: '共用一块屏', loose: '未摆位' };
  let saved = {};
  try { saved = JSON.parse(localStorage.getItem(storageKey) || '{}'); } catch (_) { /* Private previews may reject local storage; the prototype still works. */ }
  const settings = Object.assign({}, defaults);
  Object.keys(defaults).forEach(key => {
    if (typeof saved[key] === typeof defaults[key]) settings[key] = saved[key];
  });
  if (!['light', 'dark', 'system'].includes(settings.theme)) settings.theme = 'light';
  if (![0.9, 1, 1.1, 1.2].includes(settings.speed)) settings.speed = 1.1;
  if (!/^(Ctrl|Alt|Shift|Meta)(\+(Ctrl|Alt|Shift|Meta))*\+[A-Z0-9]$/.test(settings.hotkey)) settings.hotkey = defaults.hotkey;

  let state;
  let toastTimer;
  let undoAction = null;
  let restoreFocus = null;
  let transferId = 10;
  let resizeTimer;
  let lastCompact = null;
  const scheduled = new Set();

  function initialState() {
    return {
      page: 'workspace', scene: 'single', scenario: 'ready', picture: 'local', keyboard: 'local',
      switching: false, pendingText: '', maintenance: false, epoch: 0, handoffEpoch: 0, selectedPeer: null,
      positions: { mac: 'follow', think: 'right' }, devicePane: 'layout', pairCode: '738291',
      pairPhase: 'idle', settingsTab: 'sharing', recording: false, recipient: 'mac',
      filter: 'all', transferPage: 0, updatePhase: 'idle', portLabels: { local: 'DP · 本机', mac: 'USB-C · Mac mini' },
      target: 'mac', inbox: 'D:\\InputSwitch\\Inbox', logs: [{ time: timeNow(), text: '已连接 Mac mini 和 ThinkPad' }],
      transfers: [
        { id: 1, name: '工作笔记.md', size: 24576, peer: 'mac', phase: 'done', progress: 100 },
        { id: 2, name: '项目素材.zip', size: 8388608, peer: 'think', phase: 'done', progress: 100 }
      ]
    };
  }

  function timeNow() { return new Intl.DateTimeFormat('zh-CN', { hour: '2-digit', minute: '2-digit', hour12: false, timeZone: 'Asia/Shanghai' }).format(new Date()); }
  function online() { return state.scenario !== 'offline' && state.scenario !== 'unpaired'; }
  function paired() { return state.scenario !== 'unpaired'; }
  function peerName(id) { return id === 'local' ? '本机' : peers[id] ? peers[id].name : '本机'; }
  function persist() { try { localStorage.setItem(storageKey, JSON.stringify(settings)); } catch (_) { /* Storage is optional for an offline design artifact. */ } }
  function later(callback, delay) {
    const timer = setTimeout(() => { scheduled.delete(timer); callback(); }, delay);
    scheduled.add(timer);
    return timer;
  }
  function log(message) {
    state.logs.unshift({ time: timeNow(), text: message });
    state.logs = state.logs.slice(0, 8);
    $('#footerMessage').textContent = message;
  }

  function toast(message, options) {
    const opts = options || {};
    clearTimeout(toastTimer);
    undoAction = opts.undo || null;
    const action = opts.undo ? '<button data-action="undo">撤销</button>' : opts.action ? '<button data-action="' + opts.action + '">' + escapeHtml(opts.label || '查看') + '</button>' : '';
    $('#toastStack').innerHTML = '<div class="toast" data-kind="' + (opts.pending ? 'pending' : opts.error ? 'error' : 'success') + '"' + (opts.error ? ' role="alert"' : '') + '>' + icon(opts.pending ? 'refresh' : opts.error ? 'info' : 'check-circle', opts.pending ? 'busy-icon' : '') + '<span>' + escapeHtml(message) + '</span>' + action + '<button class="icon-button" data-action="dismiss-toast" aria-label="关闭提示">' + icon('close') + '</button></div>';
    toastTimer = setTimeout(dismissToast, opts.undo ? 6500 : 4200);
    log(message);
  }
  function dismissToast() { clearTimeout(toastTimer); $('#toastStack').innerHTML = ''; undoAction = null; }
  $('#toastStack').addEventListener('mouseenter', () => clearTimeout(toastTimer));
  $('#toastStack').addEventListener('mouseleave', () => { toastTimer = setTimeout(dismissToast, 3500); });
  $('#toastStack').addEventListener('focusin', () => clearTimeout(toastTimer));

  function applyTheme() {
    const theme = settings.theme === 'system' ? (matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light') : settings.theme;
    document.documentElement.dataset.theme = theme;
    document.documentElement.dataset.reducedMotion = String(settings.reducedMotion);
    $('#previewTheme').innerHTML = icon(theme === 'dark' ? 'sun' : 'moon') + '<span>' + (theme === 'dark' ? '浅色外观' : '深色外观') + '</span>';
    $('#previewTheme').setAttribute('aria-label', theme === 'dark' ? '切换浅色外观' : '切换深色外观');
  }
  matchMedia('(prefers-color-scheme: dark)').addEventListener('change', () => { if (settings.theme === 'system') applyTheme(); });

  function switchControl(key, label) {
    return '<button class="toggle" role="switch" aria-checked="' + settings[key] + '" aria-label="' + label + '" data-setting="' + key + '" data-focus="toggle-' + key + '"></button>';
  }
  function pageHeading(title, subtitle, trailing) {
    return '<div class="view-heading"><div><h1>' + title + '</h1><p>' + subtitle + '</p></div>' + (trailing || '') + '</div>';
  }
  function renderHeader() {
    $('#pictureOwner').textContent = peerName(state.picture);
    $('#keyboardOwner').textContent = settings.kvm ? peerName(state.keyboard) : '本机';
    $('#onlineFact').innerHTML = '<i class="status-dot' + (online() ? '' : ' off') + '"></i>' + (online() ? '2 台在线' : '暂无对端');
    $('#deviceCount').textContent = online() ? '2' : '0';
    const main = $('#mainSwitch');
    const unavailable = ['offline', 'unpaired', 'noinput'].includes(state.scenario);
    main.classList.toggle('unavailable', unavailable);
    main.disabled = false;
    main.setAttribute('aria-busy', String(state.switching));
    let label = state.picture === 'local' ? '切换到 Mac mini' : '切回本机';
    if (state.scenario === 'noinput') label = '没有其他输入口';
    if (state.scenario === 'unpaired') label = '先连接一台设备';
    if (state.scenario === 'offline') label = state.picture === 'local' ? '对端暂时离线' : '请对端切回本机';
    if (state.switching) label = '正在切换…';
    main.innerHTML = icon(state.switching ? 'refresh' : 'swap', state.switching ? 'busy-icon' : '') + '<span>' + label + '</span>';
    $('.hotkey-hint').innerHTML = keys(settings.hotkey);
    $('#footerConnection').innerHTML = icon(paired() ? 'shield' : 'link') + (paired() ? (online() ? '已配对 · 连接已加密' : '已配对 · 等待设备上线') : '尚未配对 · 请先连接设备');
    const pending = state.transfers.filter(item => item.phase === 'sending' || item.phase === 'paused').length;
    $('#transferCount').textContent = pending;
    $('#transferCount').hidden = !pending;
    $$('[data-maintain]').forEach(button => { button.disabled = state.maintenance; });
  }

  function sceneAlert() {
    if (state.scenario === 'offline') return '<div class="alert-inline workspace-alert">' + icon('wifi') + '<span>对端已离线，请确认另一台电脑已开机并运行 InputSwitch。</span><button data-page="devices">查看设备</button></div>';
    if (state.scenario === 'unpaired') return '<div class="alert-inline workspace-alert">' + icon('link') + '<span>两台电脑填入相同配对码，连接后即可共享。</span><button data-action="pair">开始配对</button></div>';
    if (state.scenario === 'noinput') return '<div class="alert-inline workspace-alert">' + icon('info') + '<span>没有其他可用输入口，接好显示器后再扫描。</span><button data-maintain="scan">去扫描</button></div>';
    if (state.scenario === 'failure' && !state.switching && state.pendingText) return '<div class="alert-inline error workspace-alert">' + icon('info') + '<span>显示器未确认切换，请检查 DDC/CI 开关与连接线。</span><button data-action="diagnostics">排查</button></div>';
    return '';
  }

  function computer(id, name, os) {
    const active = state.picture === id;
    const missing = id !== 'local' && state.scenario === 'noinput';
    const available = id === 'local' || (online() && !missing);
    const caption = id === 'local' ? 'DisplayPort · 本机' : missing ? 'USB-C · 未检测到' : available ? 'USB-C · 已连接' : state.scenario === 'unpaired' ? 'USB-C · 未配对' : 'USB-C · 离线';
    return '<div class="computer"><button class="computer-display' + (active ? ' active' : '') + '" data-switch-to="' + id + '" aria-label="' + (active ? '当前画面在' : '切换画面到') + name + '" aria-pressed="' + active + '"' + (missing ? ' disabled title="未检测到这个输入口"' : '') + '><span class="screen-content">' + icon(id === 'local' ? 'windows' : 'monitor') + '<span>' + os + '</span></span></button><div class="computer-name">' + escapeHtml(name) + (active ? '<span class="tag blue">当前</span>' : '') + '</div><div class="computer-meta"><i class="status-dot' + (available ? '' : ' off') + '"></i> ' + caption + '</div></div>';
  }

  function singleScene() {
    const follows = settings.kvm && state.positions.mac === 'follow';
    const missing = state.scenario === 'noinput';
    return '<div class="scene-panel' + (state.switching ? ' is-switching' : '') + '"><div class="scene-panel-head"><span>' + icon('monitor') + '共用显示器 <strong>P275MV PLUS</strong></span><span class="tag ' + (state.switching ? 'blue' : missing ? 'warn' : online() ? 'good' : '') + '">' + (state.switching ? '等待显示器确认' : missing ? '输入口待连接' : online() ? '连接正常' : '等待连接') + '</span></div><div class="screen-flow">' + computer('local', '我的 Windows', 'Windows 11') + '<div class="flow-connection"><span class="flow-note">' + (state.switching ? state.pendingText : follows ? '画面到哪，键鼠就到哪' : '仅切换画面') + '</span><div class="connection-line"><i class="signal"></i></div>' + icon(follows ? 'keyboard' : 'swap') + '</div>' + computer('mac', 'Mac mini', 'macOS') + '</div><div class="source-strip"><span>输入源</span><button class="source-button' + (state.picture === 'local' ? ' current' : '') + '" data-switch-to="local">' + (state.picture === 'local' ? icon('check') : '') + escapeHtml(state.portLabels.local) + '</button><button class="source-button' + (state.picture === 'mac' ? ' current' : '') + '" data-switch-to="mac"' + (missing ? ' disabled title="未检测到这个输入口"' : '') + '>' + (state.picture === 'mac' ? icon('check') : '') + escapeHtml(state.portLabels.mac) + '</button><button class="source-button" disabled title="HDMI 当前没有信号，无法切换">HDMI · 无信号</button><button class="text-button port-manage" data-action="ports">管理</button></div></div>';
  }

  function topology() {
    let html = '<div class="topology" role="group" aria-label="设备摆位图：选择设备后，可点方向或用方向键移动">';
    ['up', 'left', 'self', 'right', 'down'].forEach(direction => {
      if (direction === 'self') {
        html += '<div class="topology-slot self slot-self">' + icon('windows') + '<strong>我的 Windows</strong><small>本机</small></div>';
        return;
      }
      const id = Object.keys(state.positions).find(key => state.positions[key] === direction);
      const cls = 'topology-slot slot-' + direction + (id ? ' occupied' : state.selectedPeer ? ' aim' : '') + (id && state.selectedPeer === id ? ' selected' : '');
      html += '<button class="' + cls + '" data-slot="' + direction + '"' + (id ? ' data-peer="' + id + '" draggable="true" data-focus="peer-' + id + '"' : '') + ' aria-label="' + (id ? peers[id].name + '，位于' + directions[direction] + '，点击选择，可拖动' : '放到' + directions[direction]) + '">' + icon(id ? peers[id].icon : 'plus') + '<strong>' + (id ? peers[id].name : directions[direction]) + '</strong><small>' + (id ? (online() ? '已连接' : '离线') : state.selectedPeer ? '点击放置' : '放置设备') + '</small></button>';
    });
    return html + '</div>';
  }

  function sharedTray() {
    const ids = Object.keys(state.positions).filter(id => state.positions[id] === 'follow');
    return '<div class="shared-screen-tray' + (state.selectedPeer ? ' aim' : '') + '" data-slot="follow"><span>共用一块屏 · 键鼠跟随</span>' + (ids.length ? ids.map(id => '<button class="small-button" data-peer="' + id + '" draggable="true" aria-pressed="' + (state.selectedPeer === id) + '" data-focus="tray-' + id + '">' + icon('monitor') + peers[id].name + '</button>').join('') : '<button class="small-button" data-place="follow">点此放置</button>') + '</div>';
  }

  function multiScene() {
    return '<div class="scene-panel"><div class="scene-panel-head"><span>' + icon('screens') + '多屏布局</span><button class="text-button small-button" data-page="devices">管理布局 ' + icon('chevron') + '</button></div><div class="multi-topology">' + topology() + '<div class="multi-explainer"><h3>鼠标越过边缘，<br>工作自然接续。</h3><p>按真实位置摆放设备。<br>选择设备后点方向，<br>也可以直接拖动。</p><button data-action="cross-screen">' + icon('mouse') + '体验跨屏</button></div></div><div class="source-strip"><span>' + icon('keyboard') + '</span><span class="multi-summary">键鼠当前在 <strong>' + peerName(state.keyboard) + '</strong></span><button class="text-button port-manage" data-action="cross-screen">模拟鼠标跨屏 ' + icon('arrow') + '</button></div></div>';
  }

  function renderWorkspace() {
    const tabs = '<div class="segmented" role="group" aria-label="工作场景"><button data-scene="single" data-focus="scene-single" class="' + (state.scene === 'single' ? 'selected' : '') + '" aria-pressed="' + (state.scene === 'single') + '">' + icon('monitor') + '单屏切换</button><button data-scene="multi" data-focus="scene-multi" class="' + (state.scene === 'multi' ? 'selected' : '') + '" aria-pressed="' + (state.scene === 'multi') + '">' + icon('screens') + '多屏共享</button></div>';
    $('#workspaceView').innerHTML = pageHeading('工作台', '在设备之间，轻松接续你的工作。', tabs) + sceneAlert() + (state.scene === 'single' ? singleScene() : multiScene()) +
      '<div class="sharing-strip"><div class="sharing-item">' + icon('mouse') + '<div class="sharing-copy"><strong>共用键鼠</strong><p>' + (settings.kvm ? state.scene === 'single' ? '随画面切换，自动交接' : '鼠标推到边缘即可跨屏' : '已暂停，键鼠留在本机') + '</p></div>' + switchControl('kvm', '共用键鼠') + '</div><div class="sharing-item">' + icon('clipboard') + '<div class="sharing-copy"><strong>剪贴板同步</strong><p>' + (settings.clipboard ? '文字只同步到当前使用的设备' : '已关闭，保留各自的剪贴板') + '</p></div>' + switchControl('clipboard', '剪贴板同步') + '</div></div><div class="quick-transfer">' + icon('folder') + '<p><strong>顺手传个文件</strong><span>，接着在另一台处理。</span></p><button class="small-button" data-action="quick-send">' + icon('upload') + '发送文件</button><button class="text-button secondary-transfer" data-page="transfers" title="查看传输记录">' + icon('chevron') + '</button></div>';
  }

  function deviceRow(id) {
    const peer = peers[id];
    return '<button class="device-row' + (state.selectedPeer === id && state.devicePane === 'layout' ? ' selected' : '') + '" data-peer="' + id + '" data-focus="list-' + id + '" draggable="true" aria-pressed="' + (state.selectedPeer === id) + '">' + icon(peer.icon) + '<span><strong>' + peer.name + '</strong><small>' + directions[state.positions[id]] + ' · ' + (online() ? '在线' : '离线') + '</small></span><i class="status-dot' + (online() ? '' : ' off') + '"></i></button>';
  }

  function pairingForm() {
    const waiting = state.pairPhase === 'waiting';
    return '<form class="pair-form" id="pairForm">' + icon('link') + '<h2>连接另一台电脑</h2><p>两台电脑接入同一局域网并打开 InputSwitch，填入相同配对码。</p><div><label class="field-label" for="pairCode">配对码</label><div class="input-line"><input id="pairCode" name="pairCode" value="' + escapeHtml(state.pairCode) + '" maxlength="32" autocomplete="off" spellcheck="false" placeholder="输入双方约定的配对码" aria-describedby="pairHelp"' + (waiting ? ' readonly' : '') + '><button type="button" data-action="random-code"' + (waiting ? ' disabled' : '') + '>随机生成</button></div><p class="field-help' + (state.pairPhase === 'error' ? ' error' : '') + '" id="pairHelp">' + (state.pairPhase === 'error' ? '请填写配对码，两台必须完全一致。' : waiting ? '配对码已保存，正在等待对端确认身份…' : '最多 32 个字符。建议使用较长的配对码。') + '</p></div><div class="form-actions"><button type="submit" class="strong-button"' + (waiting ? ' disabled aria-busy="true"' : '') + '>' + (waiting ? icon('refresh', 'busy-icon') + '等待确认…' : '保存并连接') + '</button><button type="button" class="text-button" data-action="pair-cancel">返回布局</button></div><p class="field-help">演示中会模拟另一台电脑的确认，不连接真实设备。</p></form>';
  }

  function renderDevices() {
    let detail;
    if (state.devicePane === 'pair') detail = pairingForm();
    else {
      const peer = state.selectedPeer ? peers[state.selectedPeer] : null;
      detail = '<div class="detail-heading"><div><h2>按桌面上的位置摆放</h2><p>选一台设备，再点方向。支持拖动、方向键和撤销。</p></div><span class="tag">一台一格</span></div>' + topology() + sharedTray() + '<div class="placement-note">' + icon('info') + (state.selectedPeer ? '已选 ' + peers[state.selectedPeer].name + ' · 点方向放置，Delete 移回未摆位' : '共用一块屏与多屏布局可以同时使用。') + '</div><div class="selected-detail"><div><strong>' + (peer ? peer.name : '选择一台设备') + '</strong><br>' + (peer ? peer.os + ' · ' + peer.address : '查看连接信息、调整位置') + '</div>' + (peer ? '<button class="text-button" data-action="send-peer">' + icon('upload') + '发文件</button>' : '<span class="tag ' + (paired() ? 'good' : 'warn') + '">' + (paired() ? '已配对' : '未配对') + '</span>') + '</div>';
      const loose = Object.keys(state.positions).filter(id => state.positions[id] === 'loose');
      if (loose.length) detail += '<div class="shared-screen-tray"><span>未摆位</span>' + loose.map(id => '<button class="small-button" draggable="true" data-peer="' + id + '">' + peers[id].name + '</button>').join('') + '</div>';
    }
    $('#devicesView').innerHTML = pageHeading('设备与布局', '连接你的电脑，让位置和桌面上一样。', '<button class="small-button" data-action="pair">' + icon('plus') + '添加设备</button>') +
      '<div class="devices-layout"><div class="device-list"><p class="list-label">这台电脑</p><div class="device-row local-row">' + icon('windows') + '<span><strong>我的 Windows</strong><small>192.168.1.105 · 本机</small></span></div><p class="list-label">已知设备 · 2</p>' + deviceRow('mac') + deviceRow('think') + '<button class="add-device" data-action="pair" title="连接新设备">' + icon('plus') + '连接新设备</button><div class="pair-summary">' + icon('shield') + ' ' + (paired() ? '设备身份已验证' : '尚未配对') + '<br><button class="text-button" data-action="pair">管理配对 ' + icon('chevron') + '</button></div></div><div class="device-detail">' + detail + '</div></div>';
  }

  function formatSize(bytes) {
    if (bytes < 1024) return bytes + ' B';
    if (bytes < 1048576) return Math.round(bytes / 1024) + ' KB';
    return (bytes / 1048576).toFixed(1).replace(/\.0$/, '') + ' MB';
  }
  function visibleTransfers() {
    return state.transfers.filter(item => state.filter === 'all' || (state.filter === 'active' ? ['sending', 'paused', 'failed'].includes(item.phase) : item.phase === 'done'));
  }
  function transferPageSize() { return $('#appWindow').clientHeight <= 600 ? 1 : 2; }
  function transferRow(item) {
    let status;
    let actions = '';
    if (item.phase === 'done') status = '<span class="success">' + icon('check-circle') + '已完成</span>';
    else if (item.phase === 'cancelled') status = '已取消';
    else if (item.phase === 'failed') status = '<span class="danger">连接中断</span>';
    else status = '<span>' + (item.phase === 'paused' ? '已暂停' : '正在发送') + ' · ' + item.progress + '%</span><div class="progress-track" role="progressbar" aria-label="' + escapeHtml(item.name) + ' 传输进度" aria-valuenow="' + item.progress + '" aria-valuemin="0" aria-valuemax="100"><i style="--progress:' + (item.progress / 100) + '"></i></div>';
    if (item.phase === 'sending' || item.phase === 'paused') actions += '<button class="icon-button" data-transfer="' + item.id + '" data-transfer-action="pause" aria-label="' + (item.phase === 'paused' ? '继续传输' : '暂停传输') + '">' + icon(item.phase === 'paused' ? 'play' : 'pause') + '</button><button class="icon-button" data-transfer="' + item.id + '" data-transfer-action="cancel" aria-label="取消传输">' + icon('close') + '</button>';
    if (item.phase === 'failed' || item.phase === 'cancelled') actions += '<button class="icon-button" data-transfer="' + item.id + '" data-transfer-action="retry" aria-label="重试传输">' + icon('refresh') + '</button>';
    return '<div class="transfer-row" data-transfer-row="' + item.id + '" tabindex="-1">' + icon('file') + '<div class="transfer-file"><strong title="' + escapeHtml(item.name) + '">' + escapeHtml(item.name) + '</strong><small>' + formatSize(item.size) + ' · 发往 ' + peerName(item.peer) + '</small></div><div class="transfer-status">' + status + '</div><div class="transfer-actions">' + actions + '</div></div>';
  }

  function renderTransferList() {
    const list = $('#transferRows');
    if (!list) return;
    const focused = document.activeElement;
    const focusedTransfer = focused && focused.dataset.transfer;
    const focusedAction = focused && focused.dataset.transferAction;
    const pagerAction = focused && ['previous-transfer', 'next-transfer'].includes(focused.dataset.action) ? focused.dataset.action : null;
    const items = visibleTransfers();
    const count = transferPageSize();
    const pages = Math.max(1, Math.ceil(items.length / count));
    state.transferPage = Math.min(state.transferPage, pages - 1);
    list.innerHTML = items.length ? items.slice(state.transferPage * count, (state.transferPage + 1) * count).map(transferRow).join('') : '<div class="list-empty">' + (state.filter === 'active' ? '没有正在进行的传输。拖入文件即可开始。' : '这里会显示你的传输记录。') + '</div>';
    $('#transferPagination').innerHTML = pages > 1 ? '<span>' + (state.transferPage + 1) + ' / ' + pages + '</span><button data-action="previous-transfer"' + (!state.transferPage ? ' disabled' : '') + '>上一页</button><button data-action="next-transfer"' + (state.transferPage >= pages - 1 ? ' disabled' : '') + '>下一页</button>' : '<span>' + items.length + ' 项记录</span>';
    // Progress changes must not remove the keyboard user's current pause/cancel target.
    if (focusedTransfer) {
      const replacement = $('[data-transfer="' + focusedTransfer + '"][data-transfer-action="' + focusedAction + '"]', list) || $('[data-transfer-row="' + focusedTransfer + '"]', list);
      if (replacement) replacement.focus({ preventScroll: true });
    } else if (pagerAction) {
      const replacement = $('[data-action="' + pagerAction + '"]:not(:disabled)') || $('#transferPagination button:not(:disabled)');
      if (replacement) replacement.focus({ preventScroll: true });
    }
  }

  function renderTransfers() {
    $('#transfersView').innerHTML = pageHeading('文件传输', '文件与文件夹，都能直接交给另一台。', '<button class="small-button" data-action="inbox">' + icon('folder') + '接收目录</button>') +
      '<div class="transfer-target"><label for="recipient">发送到</label><select id="recipient"><option value="mac"' + (state.recipient === 'mac' ? ' selected' : '') + '>Mac mini</option><option value="think"' + (state.recipient === 'think' ? ' selected' : '') + '>ThinkPad</option></select><span class="tag ' + (online() ? 'good' : 'warn') + '">' + icon(online() ? 'shield' : 'wifi') + (online() ? '配对设备' : '对端离线') + '</span></div>' +
      '<div class="drop-zone" id="dropZone">' + icon('upload') + '<h2>把文件拖到这里，交给 ' + peerName(state.recipient) + '</h2><p>支持多文件和整个文件夹，重名文件不会覆盖</p><div class="drop-actions"><button data-action="pick-files">选择文件</button><button data-action="pick-folder">选择文件夹</button><button class="text-button" data-action="demo-transfer" data-focus="demo-transfer">体验一次传输 ' + icon('arrow') + '</button></div></div>' +
      '<div class="transfers-list"><div class="list-heading"><span>传输记录</span><div class="filter-group" role="group" aria-label="筛选传输记录">' + [['all', '全部'], ['active', '进行中'], ['done', '已完成']].map(item => '<button data-filter="' + item[0] + '" class="' + (state.filter === item[0] ? 'selected' : '') + '" aria-pressed="' + (state.filter === item[0]) + '">' + item[1] + '</button>').join('') + '</div></div><div id="transferRows"></div><div class="list-pagination" id="transferPagination"></div></div><div class="inbox-line">' + icon('folder') + '<span>接收至 ' + escapeHtml(state.inbox) + '</span><button class="text-button" data-action="change-inbox">更改</button></div>';
    renderTransferList();
  }

  function settingRow(title, description, control) {
    return '<div class="setting-row"><div class="setting-text"><strong>' + title + '</strong><p>' + description + '</p></div>' + control + '</div>';
  }
  function choiceGroup(name, choices, selected) {
    return '<div class="segmented" role="group" aria-label="' + name + '">' + choices.map(item => '<button data-choice="' + item[0] + '" data-choice-key="' + name + '" data-focus="choice-' + name + '-' + item[0] + '" class="' + (String(item[0]) === String(selected) ? 'selected' : '') + '" aria-pressed="' + (String(item[0]) === String(selected)) + '">' + item[1] + '</button>').join('') + '</div>';
  }

  function renderSettings() {
    const tab = state.settingsTab;
    let rows;
    if (tab === 'sharing') {
      const shortcut = '<div class="shortcut-control"><button class="keybind' + (state.recording ? ' recording' : '') + '" data-action="record-hotkey" data-focus="record-hotkey">' + (state.recording ? '请按组合键…' : keys(settings.hotkey)) + '</button>' + (state.recording ? '<button class="text-button" data-action="cancel-hotkey">取消</button>' : '<button class="text-button" data-action="default-hotkey" title="恢复 Ctrl+Alt+S">重置</button>') + '</div>';
      rows = settingRow('共用键鼠', '共用一套键盘鼠标，按设备布局自动交接。', switchControl('kvm', '共用键鼠')) +
        settingRow('光标速度', '仅在键鼠交给对端时生效。当前 ×' + settings.speed.toFixed(1), choiceGroup('speed', [[0.9, '0.9'], [1, '1.0'], [1.1, '1.1'], [1.2, '1.2']], settings.speed)) +
        settingRow('同步剪贴板文本', '只同步到当前使用的设备，避免多台互相覆盖。', switchControl('clipboard', '剪贴板同步')) +
        settingRow('切屏快捷键', state.recording ? '同时按修饰键和字母；Esc 取消。' : '随时切换到另一个输入源。点击右侧修改。', shortcut);
    } else if (tab === 'startup') {
      rows = settingRow('开机自启', '登录电脑后自动运行 InputSwitch。', switchControl('autostart', '开机自启')) +
        settingRow('自动恢复共用键鼠', '对端上线后重新连接，不会开机就抢走键鼠。', switchControl('resume', '自动恢复共用键鼠')) +
        settingRow('崩溃后自动重启', '程序意外退出时自动恢复运行。', switchControl('guard', '崩溃后自动重启')) +
        settingRow('副机模式', '给没接键鼠的电脑一次配好以上三项。请先固定 IP。', '<button class="small-button" data-action="submachine">' + (settings.autostart && settings.resume && settings.guard && settings.kvm ? icon('check') + '已开启' : '一键开启') + '</button>');
    } else {
      rows = settingRow('外观', '选择更适合当前工作环境的明暗。', choiceGroup('theme', [['light', '浅色'], ['dark', '深色'], ['system', '跟随系统']], settings.theme)) +
        settingRow('减少动态效果', '保留状态反馈，减少连接动画和过渡。', switchControl('reducedMotion', '减少动态效果')) +
        settingRow('软件更新', state.updatePhase === 'done' ? '演示结果：当前已是最新版本。' : '查看新版本和更新说明。', '<button class="small-button" data-action="check-update"' + (state.updatePhase === 'checking' ? ' disabled' : '') + '>' + (state.updatePhase === 'checking' ? icon('refresh', 'busy-icon') + '检查中…' : '检查更新') + '</button>') +
        settingRow('配置与日志', '需要排查问题时，可以查看运行记录。', '<button class="small-button" data-action="logs">' + icon('folder') + '查看记录</button>');
    }
    $('#settingsView').innerHTML = pageHeading('设置', '把常用的习惯，设置成默认。', '<span class="tag">' + icon('check') + '自动保存</span>') + '<div class="settings-tabs" role="group" aria-label="设置类别">' +
      [['sharing', '共享与快捷键'], ['startup', '启动与恢复'], ['appearance', '外观与关于']].map(item => '<button data-settings-tab="' + item[0] + '" data-focus="settings-' + item[0] + '" class="' + (tab === item[0] ? 'selected' : '') + '" aria-pressed="' + (tab === item[0]) + '">' + item[1] + '</button>').join('') +
      '</div><div class="settings-rows">' + rows + '</div><p class="settings-footnote">' + icon('info') + (tab === 'sharing' ? '紧急断开：Ctrl + Alt + Esc。光标速度变化会短暂重连。' : tab === 'startup' ? '本设计稿只保存演示设置，不修改电脑的启动任务。' : 'InputSwitch 交互预览 · 此处的更新与日志均为演示。') + '</p>';
  }

  function renderCurrent() {
    const focusKey = document.activeElement && document.activeElement.dataset.focus;
    const focusId = document.activeElement && document.activeElement.closest('.page-view') ? document.activeElement.id : null;
    ({ workspace: renderWorkspace, devices: renderDevices, transfers: renderTransfers, settings: renderSettings })[state.page]();
    if (focusKey) {
      const target = $$('[data-focus]').find(element => element.dataset.focus === focusKey && element.closest('.page-view') && !element.closest('.page-view').hidden);
      if (target) target.focus({ preventScroll: true });
    } else if (focusId) { const target = document.getElementById(focusId); if (target) target.focus({ preventScroll: true }); }
  }
  function navigate(page) {
    if (!['workspace', 'devices', 'transfers', 'settings'].includes(page)) return;
    state.recording = false;
    state.page = page;
    $$('.page-view').forEach(view => { view.hidden = view.id !== page + 'View'; });
    $$('.main-nav .nav-item').forEach(button => {
      const selected = button.dataset.page === page;
      button.classList.toggle('selected', selected);
      if (selected) button.setAttribute('aria-current', 'page'); else button.removeAttribute('aria-current');
    });
    renderCurrent();
    const view = $('#' + page + 'View');
    view.classList.remove('view-enter');
    requestAnimationFrame(() => view.classList.add('view-enter'));
    $('#viewContainer').scrollTop = 0;
  }

  function requestSwitch(target) {
    if (state.switching) return toast('切换请求已发出，正在等待显示器确认。', { pending: true });
    if (state.scenario === 'unpaired') { openPair(); return toast('先连接另一台设备，再体验画面和键鼠交接。'); }
    if (state.scenario === 'offline') return toast(state.picture === 'mac' ? '对端离线，暂时无法请它把画面切回来。' : 'Mac mini 已离线，请先在对端运行 InputSwitch。', { error: true, action: 'diagnostics', label: '排查' });
    if (state.scenario === 'noinput') return toast('没有其他可用输入口。接好显示器后，点击扫描输入口。', { error: true });
    target = target || (state.picture === 'local' ? 'mac' : 'local');
    if (state.picture === target) return toast('画面已经在' + peerName(target) + '。');
    state.switching = true;
    state.pendingText = target === 'local' ? '已请对端切回本机' : '正在发送切换请求';
    const epoch = state.epoch;
    const handoffEpoch = state.handoffEpoch;
    log(state.pendingText);
    renderHeader();
    renderCurrent();
    later(() => {
      if (epoch !== state.epoch) return;
      state.pendingText = '等待显示器确认';
      if (state.page === 'workspace') renderWorkspace();
    }, 480);
    later(() => {
      if (epoch !== state.epoch) return;
      state.switching = false;
      if (state.scenario === 'failure') {
        state.pendingText = '显示器未确认';
        renderHeader();
        renderCurrent();
        return toast('切换未完成，画面仍在' + peerName(state.picture) + '。请检查 DDC/CI。', { error: true, action: 'diagnostics', label: '排查' });
      }
      state.picture = target;
      // A late display acknowledgement must not undo an emergency keyboard release.
      const follow = settings.kvm && state.positions.mac === 'follow' && handoffEpoch === state.handoffEpoch;
      if (follow) state.keyboard = target;
      state.pendingText = '';
      renderHeader();
      renderCurrent();
      toast('画面已切到' + peerName(target) + (follow ? '，键鼠已跟随。' : '。'));
    }, 1250);
  }

  function placePeer(direction) {
    const id = state.selectedPeer;
    if (!id) return toast('先选择一台设备，再点要放置的方向。');
    if (!Object.prototype.hasOwnProperty.call(directions, direction)) return;
    const occupant = Object.keys(state.positions).find(key => key !== id && state.positions[key] === direction);
    if (occupant && direction !== 'follow' && direction !== 'loose') return toast('这个位置已有 ' + peers[occupant].name + '，请先把它移开。', { error: true });
    if (state.positions[id] === direction) return toast(peers[id].name + ' 已经在这个位置。');
    const previous = state.positions[id];
    state.positions[id] = direction;
    if (state.keyboard === id) state.keyboard = 'local';
    renderCurrent();
    renderHeader();
    toast(peers[id].name + ' 已移到' + directions[direction] + '。', { undo: () => {
      const conflict = Object.keys(state.positions).some(key => key !== id && state.positions[key] === previous && !['follow', 'loose'].includes(previous));
      if (conflict) return toast('原位置已有其他设备，无法撤销。', { error: true });
      state.positions[id] = previous;
      renderCurrent();
      toast('已恢复 ' + peers[id].name + ' 的位置。');
    } });
  }

  function crossScreen() {
    if (!settings.kvm) return toast('请先打开共用键鼠。', { error: true });
    if (!online()) return toast('对端未连接，请先确认配对和网络。', { error: true });
    const id = Object.keys(state.positions).find(key => !['follow', 'loose'].includes(state.positions[key]));
    if (!id) return toast('先把一台设备放到上、下、左、右任一方向。');
    state.keyboard = state.keyboard === id ? 'local' : id;
    renderHeader();
    renderCurrent();
    toast('模拟跨屏：键鼠已交给' + peerName(state.keyboard) + '。');
  }

  function toggleSetting(key) {
    if (typeof defaults[key] !== 'boolean') return;
    settings[key] = !settings[key];
    if (key === 'kvm' && !settings.kvm) state.keyboard = 'local';
    persist();
    applyTheme();
    renderHeader();
    renderCurrent();
    const labels = { kvm: '共用键鼠', clipboard: '剪贴板同步', autostart: '开机自启', resume: '自动恢复共用键鼠', guard: '崩溃后自动重启', reducedMotion: '减少动态效果' };
    toast(labels[key] + '已' + (settings[key] ? '开启。' : '关闭。'));
  }

  function openPair() { state.devicePane = 'pair'; state.pairPhase = 'idle'; navigate('devices'); later(() => { const input = $('#pairCode'); if (input) input.focus(); }, 30); }
  function savePair(event) {
    event.preventDefault();
    if (state.pairPhase === 'waiting') return;
    state.pairCode = $('#pairCode').value.trim();
    if (!state.pairCode) { state.pairPhase = 'error'; renderDevices(); $('#pairCode').focus(); return; }
    state.pairPhase = 'waiting';
    const epoch = state.epoch;
    renderDevices();
    log('配对码已保存，等待对端验证身份。');
    later(() => {
      if (epoch !== state.epoch || state.pairPhase !== 'waiting') return;
      state.scenario = 'ready';
      $('#demoState').value = 'ready';
      state.pairPhase = 'idle';
      state.devicePane = 'layout';
      state.selectedPeer = 'mac';
      renderHeader();
      renderCurrent();
      toast('配对成功，双方身份已确认。');
    }, 1450);
  }

  function showModal(title, body, actions) {
    restoreFocus = document.activeElement;
    $('#modalContent').innerHTML = '<div class="modal-head"><h2 id="modalTitle">' + title + '</h2><button class="icon-button" data-action="close-modal" aria-label="关闭对话框">' + icon('close') + '</button></div><div class="modal-body">' + body + '</div>' + (actions ? '<div class="modal-actions">' + actions + '</div>' : '');
    if (!$('#modal').open) $('#modal').showModal();
  }
  function closeModal() { $('#modal').close(); }
  $('#modal').addEventListener('close', () => { if (restoreFocus && restoreFocus.isConnected) restoreFocus.focus({ preventScroll: true }); });
  $('#modal').addEventListener('click', event => {
    if (event.target !== $('#modal')) return;
    const box = $('#modal').getBoundingClientRect();
    if (event.clientX < box.left || event.clientX > box.right || event.clientY < box.top || event.clientY > box.bottom) closeModal();
  });

  function help() {
    showModal('让设备协同起来', '<p>单屏切换和多屏共享可以一起使用。首页的两个场景只切换视图，各台设备仍按自己的布局工作。</p><div class="help-row"><span>切换显示器输入源</span>' + keys(settings.hotkey) + '</div><div class="help-row"><span>紧急断开键鼠共享</span><kbd>Ctrl + Alt + Esc</kbd></div><div class="help-row"><span>摆放已选设备</span><span>方向键 / 拖动</span></div><div class="help-row"><span>把设备移回未摆位</span><kbd>Delete</kbd></div><div class="modal-note">这是可交互设计稿。设备、连接、进度和系统操作均为演示，不会切换真实屏幕或读取、发送文件内容。</div>', '<button data-action="close-modal">知道了</button>');
  }

  function diagnostics() {
    showModal('切换遇到问题？', '<p>画面在另一台电脑时，需要由对端发出切回指令。先确认对端在线，再检查显示器连接。</p><div class="help-row"><span>显示器菜单</span><span>开启 DDC/CI</span></div><div class="help-row"><span>显示器连线</span><span>优先 DP / HDMI 直连</span></div><div class="help-row"><span>另一台电脑</span><span>开机并运行 InputSwitch</span></div><div class="help-row"><span>双方配对码</span><span>完全一致</span></div><div class="modal-note">内置屏幕通常不支持 DDC/CI。实际检测结果应由软件后端报告。</div>', '<button data-action="close-modal">返回工作台</button>');
  }

  function maintain(kind, confirmed) {
    if (state.maintenance) return;
    if (kind === 'scan' && !confirmed) {
      showModal('扫描显示器输入口', '<p>扫描会逐个尝试输入口。真实使用时，屏幕可能短暂变黑几次，请先保存正在编辑的内容。</p><div class="modal-note">当前为设计演示，只播放扫描进度，不会操作显示器。</div>', '<button data-action="close-modal">暂不扫描</button><button class="strong-button" data-action="confirm-scan">开始扫描</button>');
      return;
    }
    state.maintenance = true;
    const epoch = state.epoch;
    const names = { detect: '正在重新检测显示器…', scan: '正在扫描输入口…', check: '正在检测切换能力…' };
    log(names[kind]);
    renderHeader();
    toast(names[kind], { pending: true });
    later(() => {
      if (epoch !== state.epoch) return;
      state.maintenance = false;
      renderHeader();
      const results = {
        detect: '重新检测完成 · P275MV PLUS 已连接。',
        scan: state.scenario === 'noinput' ? '扫描结束，仍未找到其他可用输入口。' : '扫描完成 · 2 个输入口已连接，HDMI 无信号。',
        check: state.scenario === 'failure' || state.scenario === 'noinput' ? '显示器没有确认切换能力，请检查 DDC/CI。' : '检测完成 · 当前显示器支持软件切换。'
      };
      toast(results[kind], { error: kind === 'check' && ['failure', 'noinput'].includes(state.scenario) });
    }, kind === 'scan' ? 1850 : 1050);
  }

  function startTransfers(items) {
    if (!paired()) { openPair(); return toast('请先配对设备，再发送文件。'); }
    const records = items.map(item => ({ id: ++transferId, name: item.name, size: item.size, peer: state.recipient, phase: online() ? 'sending' : 'failed', progress: 0 }));
    state.transfers.unshift(...records);
    state.filter = 'all';
    state.transferPage = 0;
    navigate('transfers');
    renderHeader();
    toast(online() ? '已加入 ' + items.length + ' 项演示传输。' : '对端已离线。记录已保留，上线后可以重试。', { error: !online() });
  }
  function transferAction(id, action) {
    const item = state.transfers.find(record => record.id === Number(id));
    if (!item) return;
    if (action === 'pause') item.phase = item.phase === 'paused' ? 'sending' : 'paused';
    else if (action === 'cancel') { item.phase = 'cancelled'; toast('已取消 ' + item.name + ' 的演示传输。'); }
    else if (action === 'retry') {
      if (!online()) return toast('对端仍然离线，连接恢复后再重试。', { error: true });
      item.phase = 'sending';
      item.progress = 0;
      toast('已重新开始演示传输。');
    }
    renderTransferList();
    renderHeader();
  }
  setInterval(() => {
    let changed = false;
    state.transfers.forEach(item => {
      if (item.phase !== 'sending') return;
      changed = true;
      if (!online()) { item.phase = 'failed'; toast('连接中断，' + item.name + ' 可在设备上线后重试。', { error: true }); return; }
      item.progress = Math.min(100, item.progress + 8);
      if (item.progress === 100) { item.phase = 'done'; toast(item.name + ' 传输完成（演示）。'); }
    });
    if (changed) {
      if (state.page === 'transfers') renderTransferList();
      renderHeader();
    }
  }, 360);

  function changeScenario(value) {
    state.epoch++;
    state.scenario = value;
    state.switching = false;
    state.maintenance = false;
    state.pendingText = '';
    state.pairPhase = 'idle';
    state.updatePhase = 'idle';
    state.keyboard = 'local';
    if (value === 'ready' || value === 'noinput' || value === 'unpaired') state.picture = 'local';
    if (value === 'offline') state.transfers.filter(item => item.phase === 'sending').forEach(item => { item.phase = 'failed'; });
    renderHeader();
    renderCurrent();
    dismissToast();
    log('已切换演示状态：' + $('#demoState').selectedOptions[0].textContent);
  }

  const actions = {
    switch: () => requestSwitch(),
    help, diagnostics,
    'escape-help': () => showModal('随时结束键鼠共享', '<p>按 <kbd>Ctrl + Alt + Esc</kbd> 可结束当前键鼠共享会话。</p><p>真实发送过程中，光标可能已在另一台设备上，因此界面不提供点不到的“收回键鼠”按钮。</p>', '<button data-action="close-modal">知道了</button>'),
    'theme-cycle': () => { settings.theme = document.documentElement.dataset.theme === 'dark' ? 'light' : 'dark'; persist(); applyTheme(); if (state.page === 'settings') renderSettings(); },
    'dismiss-toast': dismissToast,
    undo: () => { const callback = undoAction; dismissToast(); if (callback) callback(); },
    'close-modal': closeModal,
    pair: openPair,
    'pair-cancel': () => { state.pairPhase = 'idle'; state.devicePane = 'layout'; renderDevices(); },
    'random-code': () => { state.pairCode = String(crypto.getRandomValues(new Uint32Array(1))[0] % 100000000).padStart(8, '0'); $('#pairCode').value = state.pairCode; $('#pairCode').focus(); },
    'cross-screen': crossScreen,
    'confirm-scan': () => { closeModal(); maintain('scan', true); },
    shortcut: () => { state.settingsTab = 'sharing'; navigate('settings'); $('#settingsView .keybind').focus(); },
    'record-hotkey': () => { state.recording = true; renderSettings(); $('.keybind').focus(); },
    'cancel-hotkey': () => { state.recording = false; renderSettings(); },
    'default-hotkey': () => { settings.hotkey = defaults.hotkey; state.recording = false; persist(); renderHeader(); renderSettings(); toast('快捷键已恢复为 Ctrl + Alt + S。'); },
    submachine: () => { settings.autostart = settings.resume = settings.guard = settings.kvm = true; persist(); renderHeader(); renderSettings(); toast('副机模式已开启（演示）。'); },
    'check-update': () => {
      state.updatePhase = 'checking';
      const epoch = state.epoch;
      renderSettings();
      later(() => { if (epoch !== state.epoch) return; state.updatePhase = 'done'; if (state.page === 'settings') renderSettings(); toast('检查完成：当前已是最新版本（演示）。'); }, 1100);
    },
    'quick-send': () => { navigate('transfers'); $('#filePicker').click(); },
    'send-peer': () => { if (state.selectedPeer) state.recipient = state.selectedPeer; navigate('transfers'); },
    'pick-files': () => $('#filePicker').click(),
    'pick-folder': () => $('#folderPicker').click(),
    'demo-transfer': () => startTransfers([{ name: '设计交付.zip', size: 12897484 }]),
    'previous-transfer': () => { state.transferPage = Math.max(0, state.transferPage - 1); renderTransferList(); },
    'next-transfer': () => { state.transferPage++; renderTransferList(); },
    inbox: () => showModal('接收目录', '<p>收到的文件会保存在这个目录，重名文件会自动保留两个副本。</p><code class="inline-code">' + escapeHtml(state.inbox) + '</code><div class="modal-note">这是示例位置。设计稿不会打开或创建真实目录。</div>', '<button data-action="change-inbox">更改示例目录</button><button data-action="close-modal">完成</button>'),
    'change-inbox': () => showModal('更改接收目录', '<label class="field-label" for="inboxInput">示例目录</label><input id="inboxInput" style="width:100%" value="' + escapeHtml(state.inbox) + '" maxlength="200"><p class="field-help" id="inboxHelp">只更改原型中的显示路径，不创建文件夹。</p>', '<button data-action="close-modal">取消</button><button class="strong-button" data-action="save-inbox">保存</button>'),
    'save-inbox': () => {
      const value = $('#inboxInput').value.trim();
      if (!value) { $('#inboxHelp').textContent = '请填写示例目录。'; $('#inboxHelp').classList.add('error'); $('#inboxInput').focus(); return; }
      state.inbox = value;
      closeModal();
      if (state.page === 'transfers') renderTransfers();
      toast('示例接收目录已更新。');
    },
    ports: () => showModal('管理显示器输入源', '<p>给输入口起一个好认的名字，或设为热键目标。</p><div class="ports-list">' + ['local', 'mac'].map(id => '<div class="port-row"><div><input aria-label="' + (id === 'local' ? '本机输入口名称' : 'Mac 输入口名称') + '" id="port-' + id + '" value="' + escapeHtml(state.portLabels[id]) + '" maxlength="24"><small>' + (id === 'local' ? 'DisplayPort' : 'USB-C') + ' · 本机已确认</small></div><button class="small-button" data-target="' + id + '"' + (state.picture === id ? ' disabled title="这是当前输入源"' : '') + '>' + (state.target === id ? '已设为热键' : '设为热键') + '</button></div>').join('') + '<div class="port-row"><div>HDMI<small>无信号 · 当前不可切换</small></div><button class="small-button" disabled>设为热键</button></div></div>', '<button data-action="close-modal">取消</button><button class="strong-button" data-action="save-ports">保存名称</button>'),
    'save-ports': () => { ['local', 'mac'].forEach(id => { state.portLabels[id] = $('#port-' + id).value.trim() || (id === 'local' ? 'DP · 本机' : 'USB-C · Mac mini'); }); closeModal(); renderCurrent(); toast('输入源名称已保存。'); },
    logs: () => showModal('最近的操作', '<div class="recent-log">' + state.logs.map(item => '<p><time>' + item.time + '</time><span>' + escapeHtml(item.text) + '</span></p>').join('') + '</div><p class="field-help">仅记录本次原型演示中的操作。</p>', '<button data-action="close-modal">完成</button>'),
    reset: () => {
      scheduled.forEach(timer => clearTimeout(timer));
      scheduled.clear();
      dismissToast();
      if ($('#modal').open) closeModal();
      Object.assign(settings, defaults);
      state = initialState();
      $('#demoState').value = 'ready';
      persist();
      applyTheme();
      navigate('workspace');
      renderHeader();
      toast('已恢复默认演示。');
    }
  };

  document.addEventListener('click', event => {
    const button = event.target.closest('button');
    if (!button || button.disabled) return;
    const data = button.dataset;
    if (data.page) return navigate(data.page);
    if (data.scene) { state.scene = data.scene; state.selectedPeer = null; renderCurrent(); return; }
    if (data.setting) return toggleSetting(data.setting);
    if (data.peer) { state.selectedPeer = data.peer; state.devicePane = 'layout'; renderCurrent(); return; }
    if (data.slot || data.place) return placePeer(data.slot || data.place);
    if (data.switchTo) return requestSwitch(data.switchTo);
    if (data.maintain) return maintain(data.maintain);
    if (data.settingsTab) { state.recording = false; state.settingsTab = data.settingsTab; renderCurrent(); return; }
    if (data.choiceKey) {
      if (data.choiceKey === 'speed') { settings.speed = Number(data.choice); state.keyboard = 'local'; toast('光标速度已设为 ×' + settings.speed.toFixed(1) + '，键鼠连接已恢复。'); }
      else if (data.choiceKey === 'theme') settings.theme = data.choice;
      persist();
      applyTheme();
      renderHeader();
      renderCurrent();
      return;
    }
    if (data.filter) { state.filter = data.filter; state.transferPage = 0; renderTransfers(); return; }
    if (data.transferAction) return transferAction(data.transfer, data.transferAction);
    if (data.target) { state.target = data.target; $$('[data-target]').forEach(control => { control.textContent = control.dataset.target === state.target ? '已设为热键' : '设为热键'; }); toast('已把 ' + state.portLabels[data.target] + ' 设为热键目标。'); return; }
    if (data.action && actions[data.action]) return actions[data.action]();
  });

  document.addEventListener('submit', event => { if (event.target.id === 'pairForm') savePair(event); });
  document.addEventListener('input', event => { if (event.target.id === 'pairCode') state.pairCode = event.target.value; });
  $('#demoState').addEventListener('change', event => changeScenario(event.target.value));
  document.addEventListener('change', event => {
    if (event.target.id === 'recipient') { state.recipient = event.target.value; renderTransfers(); }
    if (event.target.id === 'filePicker' || event.target.id === 'folderPicker') {
      const files = Array.from(event.target.files || []);
      if (files.length) startTransfers(files.map(file => ({ name: file.webkitRelativePath || file.name, size: file.size })));
      event.target.value = '';
    }
  });

  document.addEventListener('keydown', event => {
    // Emergency release has priority over dialogs, text editing and shortcut recording.
    if (event.ctrlKey && event.altKey && event.key === 'Escape') {
      event.preventDefault();
      state.handoffEpoch++;
      state.keyboard = 'local';
      state.recording = false;
      renderHeader();
      renderCurrent();
      toast('已结束本次键鼠共享，键鼠回到本机（演示）。');
      if ($('#modal').open) {
        let notice = $('#emergencyNotice');
        if (!notice) { notice = document.createElement('p'); notice.id = 'emergencyNotice'; notice.className = 'modal-note'; notice.setAttribute('role', 'status'); $('.modal-body').appendChild(notice); }
        notice.textContent = '键鼠已回到本机（演示），可以继续当前操作。';
      }
      return;
    }
    if ($('#modal').open) return;
    const editing = event.target.matches('input, textarea, select, [contenteditable="true"]');
    if (state.recording) {
      event.preventDefault();
      if (event.key === 'Escape') { state.recording = false; renderSettings(); return; }
      if (['Control', 'Alt', 'Shift', 'Meta'].includes(event.key)) return;
      if (!(event.ctrlKey || event.altKey || event.metaKey) || !/^[a-z0-9]$/i.test(event.key)) return toast('请使用 Ctrl、Alt 或 Meta 加一个字母或数字。', { error: true });
      const parts = [];
      if (event.ctrlKey) parts.push('Ctrl');
      if (event.altKey) parts.push('Alt');
      if (event.shiftKey) parts.push('Shift');
      if (event.metaKey) parts.push('Meta');
      parts.push(event.key.toUpperCase());
      settings.hotkey = parts.join('+');
      state.recording = false;
      persist();
      renderHeader();
      renderSettings();
      toast('快捷键已保存为 ' + parts.join(' + ') + '。');
      return;
    }
    if (editing) return;
    const combo = [event.ctrlKey ? 'Ctrl' : '', event.altKey ? 'Alt' : '', event.shiftKey ? 'Shift' : '', event.metaKey ? 'Meta' : '', event.key.length === 1 ? event.key.toUpperCase() : event.key].filter(Boolean).join('+');
    if (combo === settings.hotkey) { event.preventDefault(); requestSwitch(state.picture === state.target ? undefined : state.target); return; }
    const move = { ArrowUp: 'up', ArrowLeft: 'left', ArrowRight: 'right', ArrowDown: 'down', Delete: 'loose' }[event.key];
    if (move && state.selectedPeer && (state.page === 'devices' || (state.page === 'workspace' && state.scene === 'multi'))) {
      event.preventDefault();
      placePeer(move);
    }
  });

  document.addEventListener('dragstart', event => {
    const peer = event.target.closest('[draggable="true"][data-peer]');
    if (!peer) return;
    state.selectedPeer = peer.dataset.peer;
    event.dataTransfer.setData('application/x-inputswitch-peer', peer.dataset.peer);
    event.dataTransfer.effectAllowed = 'move';
    $$('.topology-slot:not(.occupied):not(.self), .shared-screen-tray[data-slot]').forEach(slot => slot.classList.add('aim'));
  });
  document.addEventListener('dragover', event => {
    const target = event.target.closest('[data-slot], #dropZone');
    if (!target) return;
    event.preventDefault();
    target.classList.add('drag-over');
    event.dataTransfer.dropEffect = target.id === 'dropZone' ? 'copy' : 'move';
  });
  document.addEventListener('dragleave', event => { const target = event.target.closest('[data-slot], #dropZone'); if (target && !target.contains(event.relatedTarget)) target.classList.remove('drag-over'); });
  document.addEventListener('drop', event => {
    const target = event.target.closest('[data-slot], #dropZone');
    if (!target) return;
    event.preventDefault();
    $$('.drag-over').forEach(element => element.classList.remove('drag-over'));
    if (target.id === 'dropZone') {
      const files = Array.from(event.dataTransfer.files);
      if (files.length) startTransfers(files.map(file => ({ name: file.name, size: file.size })));
      return;
    }
    const id = event.dataTransfer.getData('application/x-inputswitch-peer');
    if (!peers[id]) return;
    state.selectedPeer = id;
    placePeer(target.dataset.slot);
  });
  document.addEventListener('dragend', () => $$('.drag-over').forEach(element => element.classList.remove('drag-over')));

  function updateDimensions() {
    const app = $('#appWindow');
    const compact = app.clientWidth <= 950;
    if (compact !== lastCompact) { app.classList.toggle('compact-nav', compact); lastCompact = compact; }
    $('#dimensionLabel').textContent = Math.round(app.clientWidth) + ' × ' + Math.round(app.clientHeight) + ' · 100%';
    if (state && state.page === 'transfers') renderTransferList();
  }
  $('#previewSize').addEventListener('change', event => {
    const app = $('#appWindow');
    if (event.target.value === 'auto') { app.style.width = ''; app.style.height = ''; }
    else {
      const dimensions = event.target.value.split('x').map(Number);
      app.style.width = dimensions[0] + 'px';
      app.style.height = dimensions[1] + 'px';
    }
    updateDimensions();
  });
  new ResizeObserver(() => { clearTimeout(resizeTimer); resizeTimer = setTimeout(updateDimensions, 50); }).observe($('#appWindow'));
  window.addEventListener('resize', updateDimensions);

  state = initialState();
  const params = new URLSearchParams(location.search);
  if (params.get('embed') === '1') document.body.classList.add('embed');
  if (params.get('scene') === 'multi') state.scene = 'multi';
  if (['ready', 'offline', 'unpaired', 'failure', 'noinput'].includes(params.get('state'))) state.scenario = params.get('state');
  if (['dark', 'light'].includes(params.get('theme'))) settings.theme = params.get('theme');
  $('#demoState').value = state.scenario;
  applyTheme();
  renderHeader();
  navigate(['workspace', 'devices', 'transfers', 'settings'].includes(params.get('page')) ? params.get('page') : 'workspace');
  updateDimensions();
})();

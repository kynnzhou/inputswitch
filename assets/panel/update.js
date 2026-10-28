// 自动更新的界面层：顶栏徽章、更新弹窗、重启过渡层。
// 三件都只读 snap.update（后端七态状态机的镜像），不自己推阶段含义 ——
// 和 kvmFollowing() 问后端是同一个规矩：两套判据必然打架。
// 动作只有一个出口 updateAction()，全部 POST /api/update?action=...

/// 字节数成人话。下载进度要用；后端 human_size 在 Rust 侧，前端自己来。
/// 一位小数起步（< 100 的单位），大了取整 —— "1.2 MB/s" 比 "1.234 MB/s"
/// 好看，也比 "1220.6 KB/s" 好读。
function humanBytes(n) {
  if (!n || n < 0) return '0 B';
  const units = ['B', 'KB', 'MB', 'GB'];
  let i = 0, v = n;
  while (v >= 1024 && i < units.length - 1) { v /= 1024; i++; }
  return (i === 0 ? v : v.toFixed(v >= 100 ? 0 : 1)) + ' ' + units[i];
}

/// 当前更新状态。snap 可能还没拉到（首屏），兜一个空对象。
function updateState() {
  return (snap && snap.update) || {};
}

/// 顶栏徽章：只有「有新版本且用户没忽略」才亮。
/// 别的阶段（正在下、已就绪、正在重启）交给弹窗和过渡层说话 ——
/// 徽章再亮就成了第二张嘴，用户会以为又出了一个新版本。
function renderUpdateBadge(u) {
  const dot = document.getElementById('updateDot');
  const btn = document.getElementById('updateBtn');
  const on = u.phase === 'available' && !!u.latest && u.skipped !== u.latest;
  dot.classList.toggle('on', on);
  btn.title = on
    ? `发现新版本 v${u.latest}，点开查看并安装`
    : '检查更新、下载并安装新版本';
}

/// 重启过渡层：倒计时中盖住全屏，给「取消」留一条明路。
/// 归零由后端装（装成功进程就退了，这层自然消失），取消回 ready。
function renderRestartOverlay(u) {
  const mask = document.getElementById('restartOverlay');
  if (u.phase !== 'restarting') {
    mask.style.display = 'none';
    return;
  }
  mask.style.display = '';
  document.getElementById('restartNum').textContent = u.auto_restart_in || 0;
}

/// 弹窗正文：一个 phase 一段话 + 一组按钮。
/// 弹窗开不开由 openUpdate/closeUpdate 管，这里只在其打开时刷新内容。
function renderUpdateDialog(u) {
  const title = document.getElementById('updateTitle');
  const body = document.getElementById('updateBody');
  const closeBtn = '<button class="btn-sm ghost" onclick="closeUpdate()">关闭</button>';
  const cur = u.current || '—';
  // 发布日期只取日期段：ISO 时间戳对用户没有意义，还占一行
  const pubDate = (u.pub_date || '').slice(0, 10);
  const pubLine = pubDate ? `<div class="upd-ver">发布于 ${esc(pubDate)}</div>` : '';

  if (u.phase === 'checking') {
    title.textContent = '更新';
    body.innerHTML = `<div class="upd-ver">正在检查更新…</div>
      <div class="upd-row">${closeBtn}</div>`;
    return;
  }
  if (u.phase === 'downloading') {
    title.textContent = '正在下载更新';
    const pct = u.total > 0 ? Math.floor(u.downloaded * 100 / u.total) : 0;
    const speed = u.bytes_per_sec > 0 ? ` · ${humanBytes(u.bytes_per_sec)}/s` : '';
    // 服务器没给 Content-Length 时 total 是 0：不画百分比（画了是假的），
    // 只报已下载字节数，进度条停在 0 宽
    const size = u.total > 0
      ? `${humanBytes(u.downloaded)} / ${humanBytes(u.total)}${speed}`
      : `已下载 ${humanBytes(u.downloaded)}${speed}`;
    body.innerHTML = `
      <div class="upd-ver">v${esc(cur)} → v${esc(u.latest || '…')}</div>
      <div class="progress"><i style="width:${pct}%"></i></div>
      <div class="upd-ver">${esc(size)}</div>
      <div class="upd-hint">下载完成后会自动校验 SHA-256，通过了才能装。</div>
      <div class="upd-row">${closeBtn}</div>`;
    return;
  }
  if (u.phase === 'ready') {
    title.textContent = '更新已就绪';
    body.innerHTML = `
      <div class="upd-ver">v${esc(cur)} → v${esc(u.latest || '…')}</div>
      <div class="upd-hint">安装包已下载并通过 SHA-256 校验。安装过程中本程序会退出，
        装完自动重启（大约几十秒）；不想现在装就关掉，随时可以再打开。</div>
      <div class="upd-row">
        <button class="primary" onclick="updateAction('install')">安装并重启</button>
        ${closeBtn}
      </div>`;
    return;
  }
  if (u.phase === 'restarting') {
    title.textContent = '即将重启';
    body.innerHTML = `
      <div class="upd-ver">v${esc(cur)} → v${esc(u.latest || '…')}</div>
      <div class="upd-hint">倒计时结束就安装。取消可以继续用当前版本，安装包还在。</div>
      <div class="upd-row">
        <button class="primary" onclick="updateAction('cancel')">取消重启</button>
      </div>`;
    return;
  }
  if (u.phase === 'up_to_date') {
    title.textContent = '已是最新';
    body.innerHTML = `
      <div class="upd-ver">当前版本 v${esc(cur)} 已是最新。</div>
      <div class="upd-row">
        <button class="primary" onclick="updateAction('check')">重新检查</button>
        ${closeBtn}
      </div>`;
    return;
  }
  if (u.phase === 'error') {
    title.textContent = '更新没查成';
    body.innerHTML = `
      <div class="upd-ver">v${esc(cur)}</div>
      <div class="upd-notes">${esc(u.error || '未知错误')}</div>
      ${u.code ? `<div class="upd-ver">错误分类：${esc(u.code)}</div>` : ''}
      <div class="upd-row">
        <button class="primary" onclick="updateAction('check')">重试</button>
        ${closeBtn}
      </div>`;
    return;
  }
  if (u.phase === 'available') {
    title.textContent = `发现新版本 v${u.latest}`;
    // 忽略过的版本：徽章不亮，但弹窗里仍要能装 —— 用户可能改主意了。
    // 「忽略本次」再点一次是撤销（后端同一动作toggle），按钮文案跟着变。
    const skipped = u.skipped === u.latest;
    const dismissBtn = skipped
      ? '<button class="btn-sm ghost" onclick="updateAction(\'dismiss\')">恢复提示</button>'
      : '<button class="btn-sm ghost" onclick="updateAction(\'dismiss\')">忽略本次</button>';
    body.innerHTML = `
      <div class="upd-ver">v${esc(cur)} → <b>v${esc(u.latest)}</b></div>
      ${pubLine}
      ${skipped ? '<div class="upd-hint">已忽略这个版本：顶栏不再显示徽章，但更新仍然可以装。</div>' : ''}
      ${u.notes ? `<div class="upd-notes">${esc(u.notes)}</div>` : ''}
      <div class="upd-row">
        <button class="primary" onclick="updateAction('download')">下载并安装</button>
        ${dismissBtn}
        ${closeBtn}
      </div>`;
    return;
  }
  // idle：从没查过。打开弹窗这个动作本身就是"我想知道有没有新版"
  title.textContent = '更新';
  body.innerHTML = `
    <div class="upd-ver">当前版本 v${esc(cur)}，还没检查过更新。</div>
    <div class="upd-row">
      <button class="primary" onclick="updateAction('check')">检查更新</button>
      ${closeBtn}
    </div>`;
}

/// 每轮轮询都会跑：徽章和过渡层常驻，弹窗只在打开时刷新内容
///（避免每 1.2s 重建一次 DOM 把用户滚动位置和文本选择冲掉）。
function renderUpdate() {
  const u = updateState();
  renderUpdateBadge(u);
  renderRestartOverlay(u);
  if (document.getElementById('updateModal').style.display !== 'none') {
    renderUpdateDialog(u);
  }
}

/// 打开弹窗。从没查过（或上次查失败）就顺手查一次 —— 用户点开这个
/// 对话框本身就是"我想知道有没有新版"，让他再点一次按钮是多余的。
/// 开合走 core.js 的 openModal/closeModal：Esc 关、点遮罩关、开时
/// 焦点进框、关时还给更新钮（见 core.js 弹窗那一节的注释）。
function openUpdate() {
  openModal('updateModal');
  renderUpdateDialog(updateState());
  const phase = updateState().phase;
  if (phase === 'idle' || phase === 'error') updateAction('check');
}

function closeUpdate() {
  closeModal('updateModal');
}

/// 更新动作唯一出口。check/download 是阻塞调用，后端丢后台线程跑，
/// 这里发完等下一轮轮询刷状态（下载中轮询会临时加速，见 core.js）。
/// install 同理：点了之后倒计时线程自己走，过渡层接管交互。
function updateAction(act) {
  api('/api/update?action=' + act).then(pull);
}

// 共用键鼠：权限横幅与手感档位（renderKvm）、链路行（renderKvmLinks）、
// 两套摆位图（设备页 3×3：renderKvmGrid；工作台多屏场景的十字五槽：
// renderWorkspaceTopology）、拖拽与键盘摆位（Pointer Events）、摆位落库
// （placeOn / linkApi）。两套图共用 screenCard 和 document 级事件 ——
// 格子带 data-drop，指针事件和方向键摆位不关心容器是谁。
// P4 起工作台不再有「共用键鼠」卡：电源开关搬进共享条（#kvmSwitch），
// 光标速度搬去设置页（#kvmScaleRow），链路行搬去设备页（#kvmLinks），
// 「把键鼠交给对端」由多屏场景自己画（multiSceneHtml 的 wsHandOver）。
function renderKvm() {
  const k = snap.kvm || {};
  const el = (id) => document.getElementById(id);

  // 接收手感档位：位移先按两屏分辨率比例换算（几何正确），再乘这个倍率。
  // 两个系统的指针加速曲线不同，同一串位移在两边速度必然有差 ——
  // 「回到本机变慢」就往上调一档。控件在设置页「快捷键」选项卡。
  const scale = Number(k.recv_scale || 1);
  const scaleBtns = [
    ['kvmScale09', 0.9],
    ['kvmScale10', 1],
    ['kvmScale11', 1.1],
    ['kvmScale12', 1.2],
  ];
  scaleBtns.forEach(([id, v]) => {
    const node = el(id);
    if (!node) return;
    node.classList.toggle('sel', Math.abs(v - scale) < 0.001);
  });
  if (el('kvmScaleHint')) {
    el('kvmScaleHint').textContent = '当前 ×' + scale.toFixed(1)
      + '。只影响键鼠在对端、你在本机操作时的速度。';
  }

  // 缺权限时给一条明路。注意这两个值由 GUI 进程自己上报，不看 CLI 探针
  // （探针是包里的第二个二进制，另一个 cdhash，TCC 只认主程序，它永远报 ✗）
  // 横幅在工作台置顶（permBanner 在场景面板上方）—— 缺权限时场景面板里
  // 的任何状态都是空转，它必须先于一切被看见。
  const perms = snap.perms || { capture: true, inject: true };
  const missing = [];
  if (!perms.inject) missing.push('注入（接收对端的键鼠）');
  if (!perms.capture) missing.push('抓取（把本机键鼠送出去）');
  el('permBanner').style.display = missing.length ? '' : 'none';
  if (missing.length) {
    el('permText').textContent = '缺少权限：' + missing.join('、') +
      '。在系统设置里勾选后，必须重启本程序才会生效。';
  }

  renderKvmLinks(k);
  renderKvmGrid(k);
  renderWorkspaceTopology(k);
}

// 一台对端一行：只显示"它现在是什么状态" + 一个动作按钮。
//
// **方向和模式都不再给下拉** —— 摆位图已经把它们都表达了：
// 拖进某个格子 = 多屏共享（朝那条边推出去），拖到"没有门"= 跟随画面。
// 两套控件改同一个字段，用户会不知道该信哪个；摆位图里拖一下比下拉框快，
// 而且"两台同侧"这种配重错误只有图上看得出来。
const MODE_NAME = { edge: '多屏共享', follow: '跟随画面' };

function renderKvmLinks(k) {
  const box = document.getElementById('kvmLinks');
  if (!box) return;
  const links = k.links || [];
  if (!links.length) {
    box.innerHTML = '';
    return;
  }
  box.innerHTML = links.map(l => {
    // 状态一行写完：方位 ·（多屏共享才写模式）· 为什么现在没通。
    // 跟随画面那条不重复写「跟随画面」——「共用一块屏」已经说完了。
    const parts = [l.side ? SIDE_NAME[l.side] : '共用一块屏'];
    if (l.side) parts.push(MODE_NAME[l.mode] || '多屏共享');
    const peer = (snap.peers || []).find(p => p.node === l.node);
    // 链路"活着"指的是键鼠心跳，而心跳要求**两边**都开着共用键鼠。
    // 所以没心跳要分开说：对端不在线 / 对端的没开 / 本机的没开。
    let why = '';
    let warn = false;
    if (!l.alive) {
      if (!peer) why = '不在线';
      else if (!peer.kvm_on) { why = '它的共用键鼠没开'; warn = true; }
      else if (!k.on) { why = '本机共用键鼠没开'; warn = true; }
      else why = '还没连上';
      parts.push(why);
    }
    const meta = `<span class="meta${warn ? ' warn' : ''}">${parts.join(' · ')}</span>`;
    const held = l.active ? '<span class="held">键鼠在这台</span>'
      // 已经在转发时，点它是"改指向"（不重启、不重新抓取、画面不动）；
      // 空闲时点它才是"建一场新会话"。文案分开，免得用户以为是同一件事
      : (l.can_retarget
        ? `<button class="btn-sm" onclick="handOverTo('${esc(l.node)}')">转给它</button>`
        : `<button class="btn-sm" onclick="handOverTo('${esc(l.node)}')">交给它</button>`);
    // 圆点用纯色元素，不塞 ● 文本 —— 7×7 的盒子里放 10px 字形会溢出成黑疙瘩
    return `<div class="linkrow">
      <span class="node">${icon('mouse')}<span class="dot${l.alive ? ' on' : ''}"></span><span class="nm">${esc(l.node)}</span></span>
      ${meta}
      ${held}
    </div>`;
  }).join('');
}

function handOverTo(node) {
  flash('正在把键鼠交给 ' + node, '光标推到屏幕边缘就能过去，Ctrl+Alt+Esc 收回');
  api('/api/kvm-handover?node=' + encodeURIComponent(node)).then(pull);
}
// 点名发文件给这一台。不带 node 时后端会退回"握着键鼠的那台"，
// 多台在线又没会话就直接拒绝 —— 多机时猜人必然猜错。
// 发完跳到传输页：接下来要看的是传输记录，不是还留在设备页。
function sendTo(node) {
  // 对端设了配对码而本机没设：文件会被它在握手处拒掉（transfer.rs 的
  // receive_one：key 不匹配直接返回"配对码不一致，已拒绝"）。后端拒绝只
  // 写进它自己的状态行，用户点完「发文件」跳出系统对话框、选完文件看着
  // 它静默失败 —— 提前说清，并给一条去配对的路。
  const peer = (snap.peers || []).find(p => p.node === node);
  if (peer && peer.needs_pair) {
    return flash('对端设了配对码，本机没设', '文件会被它拒绝 —— 两边填同一个码再发',
      { text: '去配对', fn: () => showPage('devices') });
  }
  api('/api/send?node=' + encodeURIComponent(node)).then(pull);
  showPage('transfers');
}

// ---- 3×3 摆位图 ----
//
// 中心是本机，四边是对端，四角是斜角（协议只支持上下左右，斜角不连）。
// 交互是拖的（deskflow 那种手感）：抓住一台对端丢到某个方向，松手即生效。
// 四角仍然丢不进去 —— 越界判定是四向掩码，斜着没有可用的"门"。
//
// 为什么要有它：方向在配置里是每台对端一个字符串，两台都填 "right" 时
// 下拉框上完全正常，而实际推过去落到谁那儿是不确定的。格子天然一人一位，
// 摆不进去就说明配重了 —— 这类错误靠读下拉框永远发现不了。
const GRID_ROWS = [
  ['corner', 'top', 'corner'],
  ['left', 'self', 'right'],
  ['corner', 'bottom', 'corner'],
];
const SIDE_NAME = { right: '右侧', left: '左侧', top: '上方', bottom: '下方' };

// 拖动中的状态。面板每 400ms 全量重绘一次（innerHTML 替换），
// 拖到一半被重绘 = 手里的卡片凭空消失，所以重绘期间必须让路。
let dragState = null;
// 点击放置的选中态（第一下点哪台选哪台，第二下点空格子落位）。
// 设计稿 state.selectedPeer 同款；两套摆位图共用一份 —— 设备页选中一台，
// 工作台多屏场景里那台也同样亮着（同一份数据的两个视角）。
let kvmSelected = null;

function screenCard(node, o) {
  const tag = o.active ? '<span class="stag">键鼠在这台</span>'
    : o.dup ? '<span class="stag warn">放重了</span>'
    : o.fresh ? '<span class="stag new">未摆位</span>' : '';
  // 「起点」= 本次会话是从这台开始的。键鼠转到别台之后，对面推回来会
  // 落到这里（不是回到 Mac）—— 画面还在起点那台上，键鼠回去才看得见。
  const rootTag = (o.root && !o.active) ? '<span class="stag root">起点</span>' : '';
  // 「共用屏」只该用在**已经配成跟随画面**的链路上。
  //
  // 没配链路的对端（刚发现、还没摆位）以前也落在这栏、也印着「共用屏」，
  // 于是用户看到「两台都在共用屏里」，以为程序认定它们共用一块屏 ——
  // 实际那只是"你还没告诉我它放哪"。
  const label = o.fresh ? '未摆位' : (o.side ? SIDE_NAME[o.side] : '共用一块屏');
  const sel = kvmSelected === node;
  return `<div class="screen${sel ? ' sel' : ''}" data-node="${esc(node)}" data-side="${esc(o.side || '')}"
      tabindex="0" role="button" aria-pressed="${sel ? 'true' : 'false'}" title="${esc(node)} · ${label}（点选后点方向放下，也可拖动）"
      aria-label="${esc(node)}，${label}">
    ${icon('monitor')}<span class="dot${o.alive ? ' on' : ''}"></span>
    <span class="sname">${esc(node)}</span>${tag}${rootTag}</div>`;
}

function renderKvmGrid(k) {
  // 拖动中不重绘：innerHTML 一换，手里那张卡片就没了
  if (dragState) return;
  const box = document.getElementById('kvmGrid');
  const looseBox = document.getElementById('kvmLooseBox');
  const hint = document.getElementById('kvmGridHint');
  if (!box || !looseBox) return;

  // 重绘会重建节点，焦点会丢。记住是哪一台，画完再还给它的新节点。
  const focused = document.activeElement && document.activeElement.classList
    && document.activeElement.classList.contains('screen')
    ? document.activeElement.dataset.node : null;

  const links = k.links || [];
  const peers = snap.peers || [];
  const bySide = {};
  const loose = [];
  const dup = [];
  const known = new Set();
  // 本次会话从哪台开始（多跳的回程落点）。只有一台能是起点。
  const rootNode = k.root || '';
  links.forEach(l => {
    known.add(l.node);
    const item = {
      node: l.node, side: l.side || '', alive: !!l.alive, active: !!l.active,
      root: rootNode === l.node,
    };
    if (!l.side) loose.push(item);
    else if (bySide[l.side]) { dup.push(l.node); loose.push(Object.assign({ dup: true }, item)); }
    else bySide[l.side] = item;
  });
  // 已发现但还没配链路的对端：拖进格子就等于给它建一条（后端会新建）
  peers.forEach(p => {
    if (!known.has(p.node)) {
      loose.push({ node: p.node, side: '', alive: true, fresh: true, root: rootNode === p.node });
    }
  });

  const cell = (slot) => {
    if (slot === 'self') {
      return `<div class="cell self" data-drop="self">本机<small>${esc(snap.node || '—')}</small></div>`;
    }
    if (slot === 'corner') {
      // 协议里只有上下左右四个门，四角永远不能放。留空比写四个「斜角不连」安静
      return '<div class="cell corner" data-drop="corner">—</div>';
    }
    const taken = bySide[slot];
    // 选中了一台对端：空格子亮起来（aim），文案从「拖到这里」换成「放到这里」
    // —— 点击放置的第二半，用户看得见该点哪（设计稿 app.js:146 同款）
    const aim = kvmSelected && !taken ? ' aim' : '';
    const inner = taken
      ? screenCard(taken.node, taken)
      : '<span class="dropzone">' + (kvmSelected ? '放到这里' : '拖到这里') + '</span>';
    return `<div class="cell${aim}" data-drop="${slot}"><span class="sideslot">${SIDE_NAME[slot]}</span>${inner}</div>`;
  };
  box.innerHTML = GRID_ROWS.map(row => row.map(cell).join('')).join('');

  looseBox.innerHTML = loose.length
    ? loose.map(item => screenCard(item.node, item)).join('')
    : '<span class="dropzone">（没有待摆位的对端）</span>';

  const fresh = loose.filter(x => x.fresh).map(x => x.node);
  // 说明压到一句：图上已经能看出摆位，多余的重复说明只会把卡片撑高。
  // 文案必须和能做的事一一对应 —— 2026-10-28 差距审查时这里写着
  // 「或点它再点格子」，而点击放置根本没实现（pointerup 的 !moved 分支
  // 是"什么都不做"），是一句兑现不了的承诺。批2 补上点击放置后才对得上。
  const parts = ['点一台对端选中它，再点方向放下；也可以直接拖动。一个格子只放一台。'];
  if (fresh.length) parts.push(`还没摆位：${fresh.join('、')}。`);
  if (dup.length) parts.push(`⚠ ${dup.join('、')} 也摆在同一侧，推过去会落到哪台不确定。`);
  hint.textContent = parts.join(' ');

  if (focused) {
    const again = box.querySelector(`.screen[data-node="${focused}"]`)
      || looseBox.querySelector(`.screen[data-node="${focused}"]`);
    if (again) again.focus();
  }
}

// ---- 工作台多屏场景的摆位图（设计稿 topology 十字五槽的生产版）----
// 和设备页 3×3 图（renderKvmGrid）共用同一套交互：screenCard 可拖、
// 格子带 data-drop、document 级 pointer 事件 + 方向键摆位。版式按设计稿：
// 上下左右四槽 + 中心本机，四角留空（协议只支持四向，没有斜角）。
// links/peers → bySide/loose 的归集逻辑和 renderKvmGrid 逐行同构 ——
// 两处必须一致，否则同一台对端在两张图上的位置会打架。
const WS_SLOTS = ['top', 'left', 'self', 'right', 'bottom'];

function renderWorkspaceTopology(k) {
  // 拖动中不重绘：innerHTML 一换，手里那张卡片就没了
  if (dragState) return;
  const box = document.getElementById('wsTopology');
  const looseBox = document.getElementById('wsLooseBox');
  if (!box || !looseBox) return;

  // 重绘会重建节点，焦点会丢。记住是哪一台，画完再还给它的新节点。
  const focused = document.activeElement && document.activeElement.classList
    && document.activeElement.classList.contains('screen')
    ? document.activeElement.dataset.node : null;

  const links = k.links || [];
  const peers = snap.peers || [];
  const bySide = {};
  const loose = [];
  const dup = [];
  const known = new Set();
  const rootNode = k.root || '';
  links.forEach(l => {
    known.add(l.node);
    const item = {
      node: l.node, side: l.side || '', alive: !!l.alive, active: !!l.active,
      root: rootNode === l.node,
    };
    if (!l.side) loose.push(item);
    else if (bySide[l.side]) { dup.push(l.node); loose.push(Object.assign({ dup: true }, item)); }
    else bySide[l.side] = item;
  });
  peers.forEach(p => {
    if (!known.has(p.node)) {
      loose.push({ node: p.node, side: '', alive: true, fresh: true, root: rootNode === p.node });
    }
  });

  const slotHtml = (slot) => {
    if (slot === 'self') {
      return '<div class="topology-slot self slot-self">' + icon('windows')
        + '<strong>本机</strong><small>' + esc(snap.node || '—') + '</small></div>';
    }
    const taken = bySide[slot];
    // 同 renderKvmGrid 的 cell()：选中一台后空槽亮 aim、文案变「点这里放下」
    const aim = kvmSelected && !taken ? ' aim' : '';
    const inner = taken
      ? screenCard(taken.node, taken)
      : '<span class="slothint">' + (kvmSelected ? '点这里放下' : SIDE_NAME[slot] + '<br>放置设备') + '</span>';
    return '<div class="topology-slot' + (taken ? ' occupied' : '') + aim + ' slot-' + slot
      + '" data-drop="' + slot + '">' + inner + '</div>';
  };
  box.innerHTML = WS_SLOTS.map(slotHtml).join('');

  looseBox.innerHTML = loose.length
    ? loose.map(item => screenCard(item.node, item)).join('')
    : '<span class="slothint">（没有待摆位的对端）</span>';

  if (focused) {
    const again = box.querySelector('.screen[data-node="' + focused + '"]')
      || looseBox.querySelector('.screen[data-node="' + focused + '"]');
    if (again) again.focus();
  }
}

// 往某个方向放一台。这格原本那台先撤下来 —— 后端是"每台一份 side"，
// 只设置新的那台会变成两台同侧，而图里根本画不出来。
//
// 先乐观改本地快照再发请求：否则松手后卡片会先弹回旧位置，等 400ms 后那次
// 轮询回来再跳到新位置，手感像"放不进去"。
//
// 放完给一个反悔窗口（设计稿 placePeer 的 undo toast，6500ms）：拖错方向是
// 高频误操作 —— 格子挨得近，而"原来在哪"用户不一定记得。撤销把自己放回
// 原位、被顶走的那台归回本格；只此一次，撤销动作本身不再挂撤销。
async function placeOn(side, node) {
  if (!snap) return;
  const k = snap.kvm || (snap.kvm = {});
  const links = k.links || (k.links = []);
  const posts = [];
  const prev = links.find(l => l.side === side && l.node !== node);
  const me = links.find(l => l.node === node);
  const back = me ? (me.side || '') : '';      // 撤销要回到的位置
  const bumped = prev ? prev.node : '';        // 被顶走的那台，撤销时归位
  if (prev) {
    prev.side = '';
    if (prev.mode === 'edge') prev.mode = 'follow';   // 后端：无门的 edge 自动降级
    posts.push(linkApi(prev.node, ''));
  }
  if (me) {
    me.side = side;
    // 有没有门 = 模式，这两个字段是绑定的（后端也会做同样的纠正）
    me.mode = side ? 'edge' : 'follow';
  } else {
    // 托盘里"未配链路"那类：先本地插一条，后端会真的建出来
    links.push({ node, side, alive: true, mode: side ? 'edge' : 'follow', active: false });
  }
  renderKvmGrid(k);
  posts.push(linkApi(node, side));
  await Promise.all(posts);
  pull();
  if (side !== back || bumped) {
    const where = side ? SIDE_NAME[side] : '共用一块屏';
    flash(node + ' 已放到' + where,
      side ? '键鼠推到那条边就会过去' : '切画面时键鼠跟着走',
      { text: '撤销', fn: () => {
        if (side !== back) placeOn(back, node);
        if (bumped) linkApi(bumped, side).then(pull);
      } });
  }
}

function linkApi(node, side) {
  return api('/api/link?node=' + encodeURIComponent(node) + '&side=' + encodeURIComponent(side));
}

// ---- 拖拽：Pointer Events，不用 HTML5 拖放 ----
// WKWebView 对 draggable 元素的 HTML5 拖放有历史怪癖（dragstart / dataTransfer
// 在 WebView 里不可靠），而且没有"幽灵图像"可用；pointer 事件两端都稳。
function dropTargetAt(x, y) {
  const el = document.elementFromPoint(x, y);
  return el && el.closest ? el.closest('[data-drop]') : null;
}

function paintTarget(el) {
  document.querySelectorAll('[data-drop].hit, [data-drop].bad')
    .forEach(n => n.classList.remove('hit', 'bad'));
  if (!el) return;
  el.classList.add(el.dataset.drop === 'corner' ? 'bad' : 'hit');
}

document.addEventListener('pointerdown', (e) => {
  const card = e.target.closest && e.target.closest('.screen');
  if (!card || e.button !== 0 || dragState) return;
  // 掐掉默认行为，否则拖到一半会顺手把界面上的说明文字整段框选上
  // （选区从 pointerdown 就开始建立，等到 pointermove 再 preventDefault 已经晚了）。
  // 代价是这次点击不会自动聚焦，所以手动补一下 —— 方向键摆位靠它。
  e.preventDefault();
  const sel = window.getSelection && window.getSelection();
  if (sel) sel.removeAllRanges();
  try { card.focus({ preventScroll: true }); } catch (_) { card.focus(); }
  document.documentElement.classList.add('dragging');
  const r = card.getBoundingClientRect();
  dragState = {
    node: card.dataset.node, el: card, id: e.pointerId,
    sx: e.clientX, sy: e.clientY,
    dx: e.clientX - r.left, dy: e.clientY - r.top,
    w: r.width, h: r.height, moved: false,
  };
  try { card.setPointerCapture(e.pointerId); } catch (_) { /* 老 WebKit 没有也照样能拖 */ }
}, true);

document.addEventListener('pointermove', (e) => {
  const d = dragState;
  if (!d || e.pointerId !== d.id) return;
  if (!d.moved) {
    // 4px 以内当点击，不算拖 —— 否则轻轻一点就把卡片甩飞
    if (Math.abs(e.clientX - d.sx) < 4 && Math.abs(e.clientY - d.sy) < 4) return;
    d.moved = true;
    // 脱离原容器：格子是 flex 居中的，留在里面会被布局拽回去
    d.el.style.width = d.w + 'px';
    d.el.style.height = d.h + 'px';
    d.el.style.left = (d.sx - d.dx) + 'px';
    d.el.style.top = (d.sy - d.dy) + 'px';
    d.el.classList.add('dragging');
    document.body.appendChild(d.el);
    // 有的引擎会在这时候才真正落下选区锚点，再清一次
    const sel = window.getSelection && window.getSelection();
    if (sel && sel.rangeCount) sel.removeAllRanges();
  }
  e.preventDefault();
  d.el.style.left = (e.clientX - d.dx) + 'px';
  d.el.style.top = (e.clientY - d.dy) + 'px';
  paintTarget(dropTargetAt(e.clientX, e.clientY));
}, true);

document.addEventListener('pointerup', (e) => {
  const d = dragState;
  if (!d || e.pointerId !== d.id) return;
  if (!d.moved) {
    // 没移动 = 点击。pointerdown 里 preventDefault 掐掉了 click 事件，卡片
    // 点击只能在这里做：点哪台选哪台，再点自己取消（设计稿 data-peer 分支
    // 同款语义）。以前这里是"什么都不做"—— 提示文案却写着「点它再点格子」，
    // 是一句兑现不了的承诺（2026-10-28 差距审查 B 层，批2 补上）。
    const node = d.node;
    if (kvmSelected === node) {
      kvmSelected = null;
      finishDrag(null);
      renderDevicePane();   // 名册高亮/三块文字跟手变，不等 1.2s 轮询
      flashGrid('已取消选中。');
      return;
    }
    kvmSelected = node;
    finishDrag(null);
    renderDevicePane();
    flashGrid(`已选中 ${node}：点一个方向放下，点别处取消。`);
    return;
  }
  const hit = dropTargetAt(e.clientX, e.clientY);
  const slot = hit ? hit.dataset.drop : null;
  if (slot && slot !== 'corner' && slot !== 'self') {
    finishDrag(() => placeOn(slot === 'loose' ? '' : slot, d.node));
  } else {
    finishDrag(null);
    if (slot === 'corner') flashGrid('斜角不能放，只支持上下左右四个方向。');
    if (slot === 'self') flashGrid('想让两台共用一块屏，请拖到下面「共用一块屏」那一栏。');
  }
}, true);

document.addEventListener('pointercancel', () => finishDrag(null), true);

// 点击放置的第二半：点空格子把选中的对端放进去。卡片自己的点击走上面
// pointerup 的 !moved 分支（pointerdown 里 preventDefault 掐掉了 click），
// 格子空白处没有卡片、click 事件正常发得出 —— 放这里。
// 语义照设计稿（app.js:575-576）：点卡片 = 选哪台；点空格 = 放置。
document.addEventListener('click', (e) => {
  const t = e.target;
  if (!t || !t.closest) return;
  if (t.closest('.screen')) return;   // 卡片点击归 pointerup 管
  const slot = t.closest('[data-drop]');
  if (!slot) {
    // 点到不相干的地方 = 取消选中：不然 stale 选择留着，回去一看格子全亮着
    if (kvmSelected) { kvmSelected = null; paintKvm(); renderDevicePane(); }
    return;
  }
  const where = slot.dataset.drop;
  if (where === 'corner') return flashGrid('斜角不能放，只支持上下左右四个方向。');
  if (where === 'self') return flashGrid('本机格不用放；共用一块屏请点托盘那一栏。');
  if (!kvmSelected) return flashGrid('先点一台对端选中它，再点方向放下。');
  const picked = kvmSelected;
  kvmSelected = null;
  placeOn(where === 'loose' ? '' : where, picked);
});

/// 两套摆位图都重画（选中态 / aim 高亮变化时用）。
function paintKvm() {
  renderKvmGrid(snap.kvm || {});
  renderWorkspaceTopology(snap.kvm || {});
}

// 名册列点选（设计稿 deviceRow data-peer 的生产版）：和摆位图卡片共用
// 同一份 kvmSelected。切完即时刷名册高亮、placement-note 和
// selected-detail，不等下一轮 1.2s 轮询 —— 选中是纯本地状态，
// 没有请求要发，让用户盯着"点了没反应"的界面等一秒毫无道理。
function selectDevice(node) {
  kvmSelected = kvmSelected === node ? null : node;
  paintKvm();
  renderDevicePane();
}

function finishDrag(then) {
  const d = dragState;
  dragState = null;
  paintTarget(null);
  document.documentElement.classList.remove('dragging');
  if (!d) return;
  d.el.classList.remove('dragging');
  d.el.removeAttribute('style');
  if (d.moved) d.el.remove();   // 已经被挪到 body 下，重绘会重建整张图
  // 两套摆位图都重画：拖拽可能发生在任一容器里（设备页 / 工作台多屏场景）
  paintKvm();
  if (then) then();
}

// 提示单独一行：renderKvmGrid 每次都会重写 #kvmGridHint，
// 放下即被覆盖，等于没提示
function flashGrid(text) {
  const el = document.getElementById('kvmFlash');
  if (!el) return;
  el.textContent = text;
  clearTimeout(flashGrid.timer);
  flashGrid.timer = setTimeout(() => { el.textContent = ''; }, 2600);
}

// 键盘兜底：触控板拖不准、远程桌面里拖不动，方向键一样能摆
document.addEventListener('keydown', (e) => {
  const card = e.target.closest && e.target.closest('.screen');
  if (!card) return;
  const dir = { ArrowLeft: 'left', ArrowRight: 'right', ArrowUp: 'top', ArrowDown: 'bottom' }[e.key];
  if (dir) { e.preventDefault(); kvmSelected = null; placeOn(dir, card.dataset.node); return; }
  if (e.key === 'Delete' || e.key === 'Backspace') {
    e.preventDefault();
    kvmSelected = null;
    placeOn('', card.dataset.node);
  }
});

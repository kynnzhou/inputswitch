// 渲染层：顶栏三事实（renderHeader）、主按钮选目标（nextTarget）、
// 工作台场景（workspaceUi / renderWorkspaceScene 一组 + 管理输入源弹窗）、
// 传输页（transferUi / renderTransfers 一组）、全页总渲染（render）。
// 只画东西，不发请求 —— 动作全在 actions.js。
/// 顶部常驻状态条：画面 / 键鼠 / 对端。
///
/// 三个都是**事实**，不是设置项：画面 = 本机这块屏当前选中的输入口；
/// 键鼠 = kvm.active（非空就是在别台手上）；对端 = 在线台数 + 会话对端活不活。
/// 以前这些信息散在三张卡里，用户要先滚到对应卡片才知道"我现在在哪台"。
function renderHeader() {
  const el = (id) => document.getElementById(id);
  const cur = (snap.inputs || []).find(i => i.current);
  // 只写用户看得懂的那个名字。`name` 是配置里的键（usbc / hdmi1），
  // 和 label 并排写出来就成了「usbc USB-C（这台 Mac）」—— 同一件事说两遍，
  // 还把一个内部标识塞到了最显眼的位置。
  el('hdrScreen').innerHTML = !snap.has_display
    ? '<span class="sub">在另一台机器上</span>'
    : cur
      ? esc(cur.label || cur.name || '输入源')
      : '<span class="sub">读不到</span>';
  const k = snap.kvm || {};
  el('hdrKeys').textContent = k.active ? k.active : '本机';
  const n = (snap.peers || []).length;
  el('hdrPeer').innerHTML =
    `<span class="dot${k.peer_alive ? ' on' : ' off'}"></span> ${n} 台在线`;
  // P2：侧栏「设备与布局」的徽标跟顶栏对端数是同一个数 —— 顶栏是事实，
  // 徽标是"那一页里有几台设备"的路标。两处都从 snap.peers 算，不会打架。
  const dc = el('deviceCount');
  if (dc) dc.textContent = String(n);
}

/// "下一个能切过去的"口：顶栏主按钮用它挑目标。
/// 优先级：有主机 > 无信号 > 待确认；「该口不存在」和当前口都不算。
/// 注意它**只决定主按钮**，不参与输入口行的样式 —— 列表保持中立，
/// 实心蓝只留给顶栏那一个主按钮。
function nextTarget(s) {
  const cand = (s.inputs || []).filter(i => !i.current && i.presence !== 'invalid');
  return cand.find(i => i.presence === 'occupied')
    || cand.find(i => i.presence === 'idle')
    || cand[0]
    || null;
}

/// 工作台场景分段（设计稿 state.scene 的生产版）：单屏切换 / 多屏共享。
/// 纯内存态，不持久化 —— 它只是"当前怎么看这块屏"，不是配置
/// （设计稿 initialState 同样不落盘）。
let workspaceUi = { scene: 'single' };

/// 开关态：.sw 的选中写在 class + aria-checked 上（读屏软件读后者）。
/// 顶层函数 —— 工作台共享条和设置页的开关都走它。
function flip(id, on) {
  const node = document.getElementById(id);
  if (!node) return;
  node.classList.toggle('on', !!on);
  node.setAttribute('aria-checked', String(!!on));
}

/// 场景警示（设计稿 sceneAlert 的生产分支）。六种状态各带一个直达
/// 动作，没有就返回空串（整块不占版面）。
/// 和已删除的配对横幅的分工：横幅原来六种状态全说，现在按设计稿收成
/// 警示条 ——「已配对·已连接」这类正常状态由页脚常驻说；rejected
/// （校验失败）并进"没验过"档，它是配对问题的症状；replayed（重复包）
/// 是正常的网络重发，不再占版面。
function renderSceneAlert() {
  const box = document.getElementById('sceneAlert');
  if (!box) return;
  const peers = snap.peers || [];
  const k = snap.kvm || {};
  let html = '';
  if (!snap.paired) {
    html = alertInline('link', '两台电脑填入相同配对码，连接后即可共享。',
      '<button onclick="showPage(\'devices\')">去配对</button>');
  } else if (snap.peer_verified === 0) {
    const hint = snap.rejected
      ? `对端发来的 ${snap.rejected} 条消息没通过校验：多半是两边码不一样，或有一边版本较旧。`
      : '配对码已保存，正在等对端确认身份。';
    html = alertInline('link', hint,
      '<button onclick="showPage(\'devices\')">去设备页</button>');
  } else if (!peers.length) {
    html = alertInline('wifi', '还没有发现其他机器。两台都运行本程序，同一网段会自动出现。',
      '<button onclick="showPage(\'devices\')">查看设备</button>');
  } else if (!k.peer_alive) {
    html = alertInline('wifi', '对端已离线，请确认另一台电脑已开机并运行 InputSwitch。',
      '<button onclick="showPage(\'devices\')">查看设备</button>');
  } else if (snap.has_display && !(snap.inputs || []).length) {
    // 注意这档显示器是**接着的**（has_display 为真才会进这里）—— 说的是
    // "一个可切换的口都没读到"，别说成"没接显示器"，用户会去查线
    html = alertInline('info', '这台显示器上没有读到可切换的输入口，扫描一次再认。',
      '<button onclick="post(\'/api/scan\')">去扫描</button>');
  } else if (snap.support && !snap.support.startsWith('✓')) {
    html = alertInline('info', esc(snap.support),
      '<button onclick="openDiagnostics()">排查</button>', true);
  }
  box.innerHTML = html;
}

/// 警示条一行（设计稿 alert-inline）：图标 + 一句话 + 一个直达动作。
/// error 档红底 + role=alert（读屏软件立即念），普通档 role=status。
function alertInline(ic, text, btn, error) {
  return '<div class="alert-inline' + (error ? ' error' : '') + '"'
    + ' role="' + (error ? 'alert' : 'status') + '">'
    + icon(ic) + '<span>' + text + '</span>' + (btn || '') + '</div>';
}

/// 单屏场景（设计稿 singleScene 的生产版）：共用显示器头 + 双机卡片 +
/// flow-connection + 输入源条。三个对接点：
/// ① 本机卡 = 本机 + 当前输入口。blind（画面在别处、本机连显示器都没
///    枚举到）时点它 = 请对端切回（askPeer(own_value)），和顶栏主按钮
///    同一个动作；
/// ② 对端卡 = 第一台在线对端 + 它的输入口。**只信本机 inputs**：对端
///    自报的 value 是它自己那块屏的编号，跨机不可信（见 render() 里
///    主按钮那段注释），所以取"非当前、有主机、口存在"的那个口；
/// ③ 输入源条同样只信本机 inputs：current 打勾禁用、invalid 禁用、
///    其余可点，全部走本口 value 的 switchTo。
function singleSceneHtml() {
  const k = snap.kvm || {};
  const inputs = snap.inputs || [];
  const blind = snap.has_display === false;
  const cur = inputs.find(i => i.current) || null;
  const peerPort = inputs.find(i => !i.current && i.presence === 'occupied') || null;
  const peer = (snap.peers || []).find(p => p.knows_port) || (snap.peers || [])[0] || null;
  const follows = !!k.follow_link;

  const tag = blind ? '<span class="tag warn">画面在别处</span>'
    : !inputs.length ? '<span class="tag warn">输入口待连接</span>'
    : k.peer_alive ? '<span class="tag green">连接正常</span>'
    : '<span class="tag plain">等待连接</span>';

  const localCard = '<div class="computer">'
    + '<button class="computer-display' + (blind ? '' : ' active') + '" onclick="wsSwitch(\'local\')"'
    + ' aria-pressed="' + (blind ? 'false' : 'true') + '" aria-label="本机，' + (blind ? '画面在另一台机器上' : '当前画面') + '"'
    + ' title="' + (blind ? '点它请对端把画面切回本机' : '画面已经在本机') + '">'
    + '<span class="screen-content">' + icon('windows') + '<span>本机</span></span></button>'
    + '<div class="computer-name">' + esc(snap.node || '本机') + (blind ? '' : ' <span class="tag blue">当前</span>') + '</div>'
    + '<div class="computer-meta"><i class="status-dot' + (blind ? ' off' : '') + '"></i>'
    + (blind ? '画面在另一台机器上' : (cur ? esc(cur.label) + ' · 当前画面' : '没有检测到输入口'))
    + '</div></div>';

  const peerCard = '<div class="computer">'
    + '<button class="computer-display' + (blind ? ' active' : '') + '" onclick="wsSwitch(\'peer\')"'
    + ' aria-pressed="' + (blind ? 'true' : 'false') + '" aria-label="对端，' + (blind ? '当前画面' : '切换画面到对端') + '"'
    + ' title="' + (blind ? '画面已经在对端' : (peerPort ? '点它把画面切到对端' : '没有检测到对端在哪个输入口')) + '"'
    + (!blind && !peerPort ? ' disabled' : '') + '>'
    + '<span class="screen-content">' + icon('monitor') + '<span>对端</span></span></button>'
    + '<div class="computer-name">' + esc(peer ? peer.node : '对端') + (blind ? ' <span class="tag blue">当前</span>' : '') + '</div>'
    + '<div class="computer-meta"><i class="status-dot' + (blind ? '' : ' off') + '"></i>'
    + (blind ? '画面在这台显示' : (peerPort ? esc(peerPort.label) + ' · 有主机' : '没有检测到对端的输入口'))
    + '</div></div>';

  const flow = '<div class="flow-connection"><span class="flow-note">'
    + (follows ? '画面到哪，键鼠就跟到哪' : '仅切换画面') + '</span>'
    + '<div class="connection-line"><i class="signal"></i></div>'
    + icon(follows ? 'keyboard' : 'swap') + '</div>';

  let strip = '<div class="source-strip"><span>输入源</span>';
  if (!inputs.length) strip += '<span class="sub">没有检测到输入源</span>';
  inputs.forEach(i => {
    const dead = i.presence === 'invalid';
    const why = dead ? '显示器固件不接受这个输入口' : (i.current ? '画面已经在这个口上' : '');
    const suffix = dead ? ' · 该口不存在'
      : (!i.current && i.presence === 'occupied') ? ' · 有主机'
      : (!i.current && i.presence === 'idle') ? ' · 无信号' : '';
    strip += '<button class="source-button' + (i.current ? ' current' : '') + '" onclick="switchTo(' + i.value + ')"'
      + (dead || i.current ? ' disabled' : '') + (why ? ' title="' + why + '"' : '') + '>'
      + (i.current ? icon('check') : '') + esc(i.label) + suffix + '</button>';
  });
  strip += '<button class="btn-sm ghost port-manage" onclick="openPortsModal()">管理</button></div>';

  return '<div class="scene-panel"><div class="scene-panel-head"><span>' + icon('monitor')
    + '共用显示器 <strong>' + esc(snap.display || '—') + '</strong></span>' + tag + '</div>'
    + '<div class="screen-flow">' + localCard + flow + peerCard + '</div>' + strip + '</div>';
}

/// 多屏场景（设计稿 multiScene 的生产版）：摆位图 + 说明 + 键鼠交接条。
/// 摆位图是 design 的十字五槽版式，交互复用 kvm.js 的 screenCard 与
/// document 级 pointer 拖拽（格子带 data-drop）—— 和设备页 3×3 图同一
/// 套代码两个容器。交接钮的藏匿规则和已删除的 kvmHandBtn 一致：
/// 共用屏拓扑（画面一走面板就看不见）和会话中（光标被钉死）都没有
/// 可点的时机。
function multiSceneHtml() {
  const k = snap.kvm || {};
  const links = k.links || [];
  const handUseless = k.follow_link || !!k.active || !links.some(l => l.alive);
  const handBtn = handUseless ? ''
    : '<button class="btn-sm ghost port-manage" onclick="wsHandOver()"'
    + ' title="光标推到屏幕边缘就过去，按 Ctrl+Alt+Esc 收回">把键鼠交给对端 ' + icon('arrow') + '</button>';
  return '<div class="scene-panel">'
    + '<div class="scene-panel-head"><span>' + icon('screens') + '多屏布局</span>'
    + '<button class="btn-sm ghost" onclick="showPage(\'devices\')">管理布局 ' + icon('chevron') + '</button></div>'
    + '<div class="multi-topology">'
    + '<div class="topology" id="wsTopology" role="group" aria-label="设备摆位图：拖动设备到某个方向，或点它后用方向键"></div>'
    + '<div class="multi-explainer"><h3>鼠标越过边缘，<br>工作自然接续。</h3>'
    + '<p>按真实位置摆放设备。<br>直接拖动，或点它后用方向键。</p></div>'
    + '</div>'
    + '<div class="shared-screen-tray" data-drop="loose"><span>共用一块屏 · 键鼠跟随</span><div class="loosebox" id="wsLooseBox"></div></div>'
    + '<div class="source-strip"><span>' + icon('keyboard') + '</span>'
    + '<span class="multi-summary">键鼠当前在 <strong>' + esc(k.active || '本机') + '</strong></span>' + handBtn + '</div>'
    + '</div>';
}

/// 工作台场景总渲染：分段器态 + 当前场景面板 + 共享条。两个场景容器
/// 常驻 DOM，只画当前场景（设计稿 renderWorkspace 同款 —— 整页重画，
/// 生产保留静态骨架，只填场景这一段）。摆位图的数据填充在 kvm.js 的
/// renderWorkspaceTopology（render() 末尾的 renderKvm 会调它）。
function renderWorkspaceScene() {
  const single = document.getElementById('sceneSingle');
  const multi = document.getElementById('sceneMulti');
  if (!single || !multi) return;
  const isMulti = workspaceUi.scene === 'multi';
  single.hidden = isMulti;
  multi.hidden = !isMulti;
  ['single', 'multi'].forEach(v => {
    const b = document.getElementById('sceneOpt-' + v);
    if (!b) return;
    b.classList.toggle('selected', v === workspaceUi.scene);
    b.setAttribute('aria-pressed', String(v === workspaceUi.scene));
  });
  if (isMulti) multi.innerHTML = multiSceneHtml();
  else single.innerHTML = singleSceneHtml();
  // 共享条：开关态 + 一句状态文案（设计稿 sharing-strip 的 copy）
  const k = snap.kvm || {};
  flip('kvmSwitch', k.on);
  const kc = document.getElementById('kvmCopy');
  if (kc) kc.textContent = k.on
    ? (isMulti ? '鼠标推到边缘即可跨屏' : '随画面切换，自动交接')
    : '已暂停，键鼠留在本机';
  flip('clipBtn', snap.clipboard);
  const cc = document.getElementById('clipCopy');
  if (cc) cc.textContent = snap.clipboard ? '文字只同步到当前使用的设备' : '已关闭，保留各自的剪贴板';
}

/// 管理输入源弹窗的行（设计稿 ports 动作）：改名输入框 + 口状态 +
/// 设为热键。snap.target 是热键目标的展示名（后端 label_for 给出），
/// 空串 = 没设过。「该口不存在」和当前口都不能设为热键 —— 热键一按
/// 就是黑屏，比点错按钮更糟（它还是全局的，换回来比点错难）。
function isTargetPort(i) {
  return !!snap.target && snap.target === i.label;
}

function renderPortsModal() {
  const box = document.getElementById('portsList');
  if (!box) return;
  const inputs = snap.inputs || [];
  if (!inputs.length) {
    box.innerHTML = '<div class="empty">没有检测到输入源。接好显示器后点顶部「重新检测」再读一次。</div>';
  } else {
    box.innerHTML = inputs.map(i => {
      const dead = i.presence === 'invalid';
      const meta = '0x' + i.value.toString(16).toUpperCase().padStart(2, '0') + ' · '
        + (dead ? '该口不存在' : i.presence === 'occupied' ? '有主机' : i.presence === 'idle' ? '无信号' : '状态未知')
        + ' · ' + (i.verified ? '本机确认' : '待确认');
      const tgt = dead || i.current
        ? '<button class="btn-sm" disabled title="' + (i.current ? '这是当前输入源' : '显示器固件不接受这个输入口') + '">设为热键</button>'
        : '<button class="btn-sm" onclick="setTarget(' + i.value + ')">' + (isTargetPort(i) ? '已设为热键' : '设为热键') + '</button>';
      return '<div class="port-row"><div>'
        + '<input type="text" maxlength="24" value="' + esc(i.label) + '" aria-label="输入口名称"'
        + ' data-port-name="' + esc(i.name) + '" onfocus="editingPorts=true" onblur="editingPorts=false">'
        + '<small>' + meta + '</small></div>' + tgt + '</div>';
    }).join('');
  }
  const note = document.getElementById('portsNote');
  if (note) note.textContent = snap.port_note ? '上次检测：' + snap.port_note : '';
}

function render() {
  if (!snap) return;
  document.getElementById('node').textContent = snap.node || '';
  // P2：侧栏底部的本机名跟顶栏「本机」是同一个值。收成图标栏时这行字
  // 会被 CSS 藏掉（.compact-nav .local-machine > div），展开时才看得见。
  document.getElementById('navNode').textContent = snap.node || '';
  renderHeader();
  renderUpdate();

  // P4：场景警示（设计稿 sceneAlert 的生产分支）取代配对横幅 ——
  // 六种"现在连不上/切不了"各带一个直达动作；正常状态页脚说。
  renderSceneAlert();

  // P2：页脚左侧的配对状态 —— 和横幅是同一份事实，但哪一页都看得见。
  // 图标跟状态走：✓ 已连接（绿）/ 等对端确认（黄）/ 未配对（黄），
  // 颜色由 .footer-connection / .off 两个类在 CSS 里分。
  const fc = document.getElementById('footerConnection');
  if (fc) {
    if (snap.paired && snap.peer_verified > 0) {
      fc.className = 'footer-connection';
      fc.innerHTML = icon('check') + '<span>已配对 · 已连接</span>';
    } else if (snap.paired) {
      fc.className = 'footer-connection off';
      fc.innerHTML = icon('clock') + '<span>配对码已填 · 等对端确认</span>';
    } else {
      fc.className = 'footer-connection off';
      fc.innerHTML = icon('shield') + '<span>未配对</span>';
    }
  }
  // 页脚中间：切屏热键。没设过就不占位（CSS :empty 会把整行收掉）。
  const fm = document.getElementById('footerMessage');
  if (fm) fm.textContent = snap.hotkey ? '切屏热键 ' + snap.hotkey : '';
  const codeInput = document.getElementById('code');
  // 只在用户没动过输入框时才用服务端值回填。
  // 原来是无条件回填，导致清空后 1.2 秒又被拉回旧码 —— 看起来就像"取消不了"。
  if (!codeDirty && document.activeElement !== codeInput) {
    codeInput.value = snap.pair_code || '';
  }
  document.getElementById('unpairBtn').style.display = snap.paired ? '' : 'none';

  // 主按钮：**永远显示**，只是不能切的时候换文案 + .is-off。
  // 只有"本机自己也确认过这个口"才谈得上切过去：对端报的是它**自己那块屏**
  // 的编号，两块屏的 0x0F/0x12 是各算各的，拿别人的编号切自己的屏 = 黑屏。
  // v0.6.71 起 `snap.inputs` 里不再有"只凭对端自报"的口。
  const main = document.getElementById('mainSwitch');
  const peer = (snap.peers || []).find(p => p.knows_port);
  const blind = snap.has_display === false;
  const step = document.getElementById('nextStep');
  const tgt = blind ? null : nextTarget(snap);

  main.className = 'primary btn-lg';
  main.disabled = false;
  main.removeAttribute('title');
  main.onclick = quickSwitch;

  if (blind) {
    // 画面在对面：按钮变成"请它切回来"。没有对端时保留文案、标成 .is-off。
    // 图标 + <span> 跟设计稿主按钮一致（app.js renderHeader：icon('swap') + span）。
    // esc() 不能省：peer.node 是后端来的字符串，进了 innerHTML 就得转义。
    main.innerHTML = icon('swap')
      + '<span>' + (peer ? `请 ${esc(peer.node)} 把画面切回本机` : '请对端把画面切回本机') + '</span>';
    if (!peer) main.classList.add('is-off');
    main.dataset.action = 'peer';
    main.dataset.value = snap.own_value || '';
    step.innerHTML = peer
      ? '<span class="arrow">→</span> 本机读不到这块屏，切回要由正在显示画面的那台来发指令。'
      : '<span class="arrow">→</span> 对端不在线，画面暂时切不回来。';
  } else if (tgt) {
    main.innerHTML = icon('swap') + '<span>切到 ' + esc(tgt.label)
      + (kvmFollowing() ? '（键鼠跟过去）' : '') + '</span>';
    main.dataset.action = 'local';
    main.dataset.value = tgt.value;
    main.dataset.name = tgt.name || '';
    step.innerHTML = peer
      ? `<span class="arrow">→</span> 切过去之后，本机读不到这块屏，要由 ${esc(peer.node)} 切回来。`
      : '<span class="arrow">→</span> 对端不在线：切过去就切不回来，先让对端开机并运行本程序。';
  } else {
    // 有显示器、但没有可切的口（全都不存在 / 只有当前口）。
    // 不 disabled：位置和形状都不变，点下去由 quickSwitch() 说清为什么。
    main.innerHTML = icon('swap') + '<span>没有其他输入口</span>';
    main.classList.add('is-off');
    main.dataset.action = 'none';
    main.dataset.value = '';
    step.innerHTML = '<span class="arrow">→</span> 扫描一次可以确认别的口上有没有主机。';
  }

  // P4：工作台场景（单屏 / 多屏）。renderKvm 在末尾会补摆位图的数据。
  renderWorkspaceScene();

  // 管理输入源弹窗：开着的时候随轮询刷新（"已设为热键"要点完就看到），
  // 但聚焦输入框时让路 —— 重建节点会把打了一半的字抹掉（同 editingPorts
  // / codeDirty 的套路）。弹窗关着就不画。
  const pm = document.getElementById('portsModal');
  if (pm && pm.style.display !== 'none' && !editingPorts) renderPortsModal();
  renderKvm();

  // 对端
  const peers = document.getElementById('peers');
  if (!(snap.peers || []).length) {
    peers.innerHTML = '<div class="empty" style="text-align:center">还没有发现其他机器</div>';
  } else {
    peers.innerHTML = snap.peers.map(p => {
      // 对端报过来的口名可能是它自己那边的匿名名（`输入源 0x12`）——
      // 那种时候只写十六进制。有名字就只写名字：两套编号各算各的
      // （见上面主按钮那段注释），拼成「DP（这台 Windows）（0x10）」
      // 既会折行，也会让人以为这个 0x10 和左边列表里的是同一个口。
      const rawLabel = (p.label || p.input || '').trim();
      const hex = `0x${(p.value || 0).toString(16).toUpperCase().padStart(2, '0')}`;
      const shown = rawLabel && !/^输入源/.test(rawLabel) ? rawLabel : hex;
      const label = p.knows_port
        ? `它那块屏现在在 ${shown}`
        : '口未知（它的画面现在不在它那边）';
      // 对面设了码而本机没设：它会把我们发过去的指令全丢掉，
      // 表现为"点了没反应"，不说清楚会一直找不到原因
      const blocked = p.needs_pair
        ? '<div class="meta warn">对端设了配对码、本机没设，发给它的指令会被丢掉。请两边填同一个码。</div>'
        : '';
      // 程序在线、但共用键鼠关着：表现和"对面关机"一模一样（心跳不来）。
      // 不在这里点破，用户会去查网线。
      const kvmOff = p.kvm_on
        ? '<span class="tag green">共用键鼠已开</span>'
        : '<span class="tag warn">共用键鼠没开</span>';
      // 地址单独一行：和中文混排时 word-break:break-all 会把 "0x10）" 拆开
      return `<div class="peerrow">
        <span class="dot ${p.knows_port ? 'on' : 'off'}"></span>
        <div>
          <div class="name">${esc(p.node)} ${p.paired ? '<span class="tag green">已配对</span>' : '<span class="tag">未配对</span>'} ${kvmOff}</div>
          <div class="meta">${label}</div>
          <div class="addr">${esc(p.addr)}</div>
          ${blocked}
        </div>
        <button class="btn-sm" onclick="sendTo('${esc(p.node)}')">${icon('upload')}发文件</button>
      </div>`;
    }).join('');
  }

  // P3：传输页（选择器 / 拖放区标题 / 徽标 / 接收目录 / 传输记录列表）
  renderTransfers();
  document.getElementById('display').textContent = snap.display || '—';
  document.getElementById('identity').textContent = snap.identity || '—';
  const supportLine = document.getElementById('supportLine');
  document.getElementById('support').textContent = snap.support || '';
  // 只在这块屏**不能**完整切换时才占一行。✓ 完整支持是默认状态，
  // 天天挂着只会让顶栏多一行没人看的字；而它和"没有其他输入口"这类
  // 情况放一起还会自相矛盾（能切，但没口可切）。
  supportLine.style.display = snap.support && !snap.support.startsWith('✓') ? '' : 'none';
  const statusLine = document.getElementById('status');
  // 后端把"型号 · 当前口"也塞在 status 里，这两项顶栏和卡片标题已经各写了一次。
  // 只有**动作回执**（"正在检测…"、"共用键鼠已开启"）才值得占这一行。
  const idleStatus = snap.status && (
    (snap.display && snap.status.startsWith(snap.display + ' · '))
    || (!snap.has_display && snap.status.startsWith('本机没有接通的显示器')));
  // flash() 的提示有自己的 3.2s 生命周期，别被 1.2s 一次的轮询擦掉
  if (!flashUntil || Date.now() > flashUntil) {
    statusLine.textContent = idleStatus ? '' : (snap.status || '');
    statusLine.style.display = statusLine.textContent ? '' : 'none';
  }
  document.getElementById('hotkey').textContent = snap.hotkey || '—';
  // 帮助弹窗里的热键位跟着同一份值走（后端改键后两边一起变，不写死）
  const helpHotkey = document.getElementById('helpHotkey');
  if (helpHotkey) helpHotkey.textContent = snap.hotkey || '—';
  document.getElementById('target').textContent = snap.target || '未设置';

  // 开关用 .sw 表达状态，不再拿「已开启 / 已关闭」两个按钮去冒充
  // （flip 是顶层函数，工作台共享条和这里共用）
  const kc = snap.kvm || {};
  flip('autoBtn', snap.autostart);
  flip('resumeBtn', kc.auto_resume);
  flip('guardBtn', kc.keep_alive);
  const subOn = !!(kc.auto_resume && kc.keep_alive && snap.autostart);
  const sb = document.getElementById('subBtn');
  sb.textContent = subOn ? '副机模式：已开启' : '副机模式：一键开启';
  sb.classList.toggle('is-on', subOn);
  const bh = document.getElementById('bootHint');
  if (bh) {
    bh.textContent = kc.auto_resume
      ? '自动恢复不会一开机就抢键鼠：等对端上线后才接，失败会自动重试。'
      : '自动恢复已关闭：每次启动都要手动点一次「开启」。没接键鼠的那台建议打开。';
  }
}

// ── P3 传输页：选择器 / 拖放区 / 传输记录列表 ──────────────────────
// 结构移植设计稿 app.js 的传输组（visibleTransfers / transferPageSize /
// transferRow / renderTransferList / renderTransfers），数据源从设计稿的
// 演示 state.transfers 换成后端任务注册表的快照 snap.transfers
// （src/transfer/tasks.rs：本机发起的发送才有条目，阶段五态
// sending / paused / done / failed / cancelled）。
// 两条和生产现状相关的注意点：
//   ① 轮询每 1.2s 全页重绘 —— 列表两次渲染结果相同就不碰 DOM
//      （renderRowsCache），进度条不会每秒重置 transition，也不会把
//      用户正要点着的按钮节点换掉；
//   ② 键盘焦点保持照抄设计稿：重绘后把焦点还给原来那枚暂停/取消钮。
/// 筛选档与页码（纯前端状态，后端只给全量列表）；recipient 记录选择器
/// 选中项，选项集合不变时不让轮询把人正开着的下拉框关掉。
let transferUi = { filter: 'all', page: 0, recipient: '' };
/// 列表/分页两次渲染的缓存。null = 还没画过。
let transferRowsCache = null;
let transferPagerCache = null;

function formatSize(bytes) {
  if (bytes < 1024) return bytes + ' B';
  if (bytes < 1048576) return Math.round(bytes / 1024) + ' KB';
  return (bytes / 1048576).toFixed(1).replace(/\.0$/, '') + ' MB';
}

/// 筛选语义照设计稿：进行中 = sending/paused/failed（断了也算"还没完"，
/// 可以重试）；已完成 只含 done；cancelled 两头都不占，只在"全部"里可见。
function visibleTransfers() {
  return (snap.transfers || []).filter(item =>
    transferUi.filter === 'all'
    || (transferUi.filter === 'active'
      ? ['sending', 'paused', 'failed'].indexOf(item.phase) >= 0
      : item.phase === 'done'));
}

/// 每页条数：窗口矮于 600px 时一页一条（设计稿同款阈值，。app 量不到
/// 高度时（老 WebView2 / 自测 DOM 桩）按 2 条）
function transferPageSize() {
  const app = document.querySelector('.app');
  return app && app.clientHeight <= 600 ? 1 : 2;
}

/// 进度百分比。连接建立前 total 还是 0（collect 在 send() 里面做），
/// 那时显示 0% 而不是 NaN。
function transferPercent(item) {
  return item.total > 0 ? Math.min(100, Math.round(item.sent * 100 / item.total)) : 0;
}

function transferRow(item) {
  const phase = item.phase;
  let status;
  let actions = '';
  if (phase === 'done') status = '<span class="success">' + icon('check-circle') + '已完成</span>';
  else if (phase === 'cancelled') status = '已取消';
  else if (phase === 'failed') status = '<span class="danger">连接中断</span>';
  else {
    const pct = transferPercent(item);
    status = '<span>' + (phase === 'paused' ? '已暂停' : '正在发送') + ' · ' + pct + '%</span>'
      + '<div class="progress-track" role="progressbar" aria-label="' + esc(item.name)
      + ' 传输进度" aria-valuenow="' + pct + '" aria-valuemin="0" aria-valuemax="100">'
      + '<i style="--progress:' + (pct / 100) + '"></i></div>';
  }
  // data-transfer / data-transfer-action 是给焦点保持用的锚点（设计稿同款），
  // 动作本身走 onclick（生产的按钮不分发，全是全局函数直挂）
  if (phase === 'sending' || phase === 'paused') {
    actions += '<button class="icon-button" data-transfer="' + item.id + '" data-transfer-action="pause"'
      + ' onclick="transferAction(' + item.id + ',\'' + (phase === 'paused' ? 'resume' : 'pause') + '\')"'
      + ' aria-label="' + (phase === 'paused' ? '继续传输' : '暂停传输') + '">'
      + icon(phase === 'paused' ? 'play' : 'pause') + '</button>'
      + '<button class="icon-button" data-transfer="' + item.id + '" data-transfer-action="cancel"'
      + ' onclick="transferAction(' + item.id + ',\'cancel\')" aria-label="取消传输">'
      + icon('close') + '</button>';
  }
  if (phase === 'failed' || phase === 'cancelled') {
    actions += '<button class="icon-button" data-transfer="' + item.id + '" data-transfer-action="retry"'
      + ' onclick="transferAction(' + item.id + ',\'retry\')" aria-label="重试传输">'
      + icon('refresh') + '</button>';
  }
  return '<div class="transfer-row" data-transfer-row="' + item.id + '" tabindex="-1">' + icon('file')
    + '<div class="transfer-file"><strong title="' + esc(item.name) + '">' + esc(item.name) + '</strong>'
    + '<small>' + formatSize(item.total) + ' · 发往 ' + esc(item.peer) + '</small></div>'
    + '<div class="transfer-status">' + status + '</div>'
    + '<div class="transfer-actions">' + actions + '</div></div>';
}

function renderTransferList() {
  const list = document.getElementById('transferRows');
  if (!list) return;
  // 重绘前记下键盘用户正盯着的按钮（设计稿同款三段：传输钮 / 行 / 分页钮）
  const focused = document.activeElement;
  const focusedTransfer = focused && focused.dataset && focused.dataset.transfer;
  const focusedAction = focused && focused.dataset && focused.dataset.transferAction;
  const pagerAction = focused && focused.dataset && focused.dataset.action
    && ['previous-transfer', 'next-transfer'].indexOf(focused.dataset.action) >= 0
    ? focused.dataset.action : null;
  const items = visibleTransfers();
  const count = transferPageSize();
  const pages = Math.max(1, Math.ceil(items.length / count));
  transferUi.page = Math.min(transferUi.page, pages - 1);
  const rowsHtml = items.length
    ? items.slice(transferUi.page * count, (transferUi.page + 1) * count).map(transferRow).join('')
    : '<div class="list-empty">' + (transferUi.filter === 'active'
      ? '没有正在进行的传输。选好文件发出去即可。' : '这里会显示你的传输记录。') + '</div>';
  const pager = document.getElementById('transferPagination');
  const pagerHtml = pages > 1
    ? '<span>' + (transferUi.page + 1) + ' / ' + pages + '</span>'
      + '<button data-action="previous-transfer" onclick="transferPage(-1)"'
      + (!transferUi.page ? ' disabled' : '') + '>上一页</button>'
      + '<button data-action="next-transfer" onclick="transferPage(1)"'
      + (transferUi.page >= pages - 1 ? ' disabled' : '') + '>下一页</button>'
    : '<span>' + items.length + ' 项记录</span>';
  // 内容没变就不碰 DOM：1.2s 轮询每次都重画，会把进度条的 CSS transition
  // 反复重置（跳变），也会换掉用户正要点着的按钮节点（mousedown 之后节点
  // 被替换 = click 丢）。设计稿靠"进度变了才重画"规避，生产用缓存字符串。
  if (rowsHtml !== transferRowsCache) {
    list.innerHTML = rowsHtml;
    transferRowsCache = rowsHtml;
    restoreTransferFocus(focusedTransfer, focusedAction, pagerAction);
  }
  if (pager && pagerHtml !== transferPagerCache) {
    pager.innerHTML = pagerHtml;
    transferPagerCache = pagerHtml;
  }
}

/// 焦点保持：重绘之后把焦点还给原来那枚钮（设计稿 app.js 同款逻辑）。
/// 桩里 document.querySelector 只认 .app，返回 null 即跳过，不报错。
function restoreTransferFocus(focusedTransfer, focusedAction, pagerAction) {
  if (focusedTransfer) {
    const replacement = document.querySelector('[data-transfer="' + focusedTransfer + '"][data-transfer-action="' + focusedAction + '"]')
      || document.querySelector('[data-transfer-row="' + focusedTransfer + '"]');
    if (replacement) replacement.focus({ preventScroll: true });
  } else if (pagerAction) {
    const replacement = document.querySelector('[data-action="' + pagerAction + '"]:not(:disabled)')
      || document.querySelector('#transferPagination button:not(:disabled)');
    if (replacement) replacement.focus({ preventScroll: true });
  }
}

function renderTransfers() {
  const peers = snap.peers || [];
  const sel = document.getElementById('recipient');
  // 选项只在集合变化时重建 —— 每轮轮询都重画会把人正开着的下拉框关掉
  const names = peers.map(p => p.node).join('\n');
  if (sel.dataset.peers !== names) {
    sel.dataset.peers = names;
    sel.innerHTML = peers.length
      ? peers.map(p => '<option value="' + esc(p.node) + '">' + esc(p.node) + '</option>').join('')
      : '<option value="">没有对端</option>';
  }
  // 选中的对端下线了（或被顶掉）就回落第一台，别留着个空选择
  if (transferUi.recipient && !peers.some(p => p.node === transferUi.recipient)) transferUi.recipient = '';
  if (!transferUi.recipient && peers.length) transferUi.recipient = peers[0].node;
  sel.value = transferUi.recipient;
  const peer = peers.find(p => p.node === transferUi.recipient) || null;
  // 拖放区标题跟着选中的对端走（设计稿：把文件拖到这里，交给 XX）
  document.getElementById('dropPeer').textContent = peer
    ? '把文件拖到这里，交给 ' + peer.node
    : '把文件拖到这里（先在设备页配对一台）';
  // 配对标：按选中对端自己的 paired 字段说话，不拿全局状态冒充
  const tag = document.getElementById('recipientTag');
  if (!peers.length) {
    tag.className = 'tag plain';
    tag.textContent = '没有对端';
  } else if (peer && peer.paired) {
    tag.className = 'tag green';
    tag.innerHTML = icon('shield') + '已配对';
  } else {
    tag.className = 'tag warn';
    tag.innerHTML = icon('wifi') + '未配对';
  }
  // 侧栏徽标 = 正在发送 + 已暂停的条数（设计稿 renderHeader 同款）。
  // 一条在进行都没有时整枚收掉（hidden），不常驻一个 0。
  const active = (snap.transfers || []).filter(t => t.phase === 'sending' || t.phase === 'paused').length;
  const badge = document.getElementById('transferCount');
  if (badge) {
    badge.textContent = String(active);
    badge.hidden = active === 0;
  }
  document.getElementById('inboxPath').textContent = '接收至 ' + (snap.inbox || '—');
  renderTransferList();
}

// 这次切屏会不会顺手把键鼠带走 —— 文案要跟实际行为一致，
// 否则用户按了按钮发现鼠标没了会以为出 bug。
// **问后端**（will_follow = 有共用屏链路 && 本机能抓取）。原来自己推
// `k.mode !== 'edge' && role === 'console'`：k.mode 只取第一条链路，
// 而且 role 在 v0.6.49 之后已经降级成提示，判据跟 do_switch 不一致。
function kvmFollowing() {
  return !!(snap && snap.kvm && snap.kvm.will_follow);
}

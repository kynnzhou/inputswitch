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
      '<button onclick="openScanModal()">去扫描</button>');
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
    + ' data-scene-focus="local"'
    + ' aria-pressed="' + (blind ? 'false' : 'true') + '" aria-label="本机，' + (blind ? '画面在另一台机器上' : '当前画面') + '"'
    + ' title="' + (blind ? '点它请对端把画面切回本机' : '画面已经在本机') + '">'
    + '<span class="screen-content">' + icon('windows') + '<span>本机</span></span></button>'
    + '<div class="computer-name">' + esc(snap.node || '本机') + (blind ? '' : ' <span class="tag blue">当前</span>') + '</div>'
    + '<div class="computer-meta"><i class="status-dot' + (blind ? ' off' : '') + '"></i>'
    + (blind ? '画面在另一台机器上' : (cur ? esc(cur.label) + ' · 当前画面' : '没有检测到输入口'))
    + '</div></div>';

  const peerCard = '<div class="computer">'
    + '<button class="computer-display' + (blind ? ' active' : '') + '" onclick="wsSwitch(\'peer\')"'
    + ' data-scene-focus="peer"'
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
      + ' data-scene-focus="src-' + i.value + '"'
      + (dead || i.current ? ' disabled' : '') + (why ? ' title="' + why + '"' : '') + '>'
      + (i.current ? icon('check') : '') + esc(i.label) + suffix + '</button>';
  });
  strip += '<button class="btn-sm ghost port-manage" data-scene-focus="ports" onclick="openPortsModal()">管理</button></div>';

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
    : '<button class="btn-sm ghost port-manage" data-scene-focus="handover" onclick="wsHandOver()"'
    + ' title="光标推到屏幕边缘就过去，按 Ctrl+Alt+Esc 收回">把键鼠交给对端 ' + icon('arrow') + '</button>';
  return '<div class="scene-panel">'
    + '<div class="scene-panel-head"><span>' + icon('screens') + '多屏布局</span>'
    + '<button class="btn-sm ghost" data-scene-focus="layout" onclick="showPage(\'devices\')">管理布局 ' + icon('chevron') + '</button></div>'
    + '<div class="multi-topology">'
    + '<div class="topology" id="wsTopology" role="group" aria-label="设备摆位图：拖动设备到某个方向，或点它后用方向键"></div>'
    + '<div class="multi-explainer"><h3>鼠标越过边缘，<br>工作自然接续。</h3>'
    + '<p>按真实位置摆放设备。<br>直接拖动，或点它后用方向键。</p></div>'
    + '</div>'
    + '<div class="shared-screen-tray' + (kvmSelected ? ' aim' : '') + '" data-drop="loose"><span>共用一块屏 · 键鼠跟随</span><div class="loosebox" id="wsLooseBox"></div></div>'
    + '<div class="source-strip"><span>' + icon('keyboard') + '</span>'
    + '<span class="multi-summary">键鼠当前在 <strong>' + esc(k.active || '本机') + '</strong></span>' + handBtn + '</div>'
    + '</div>';
}

/// 工作台场景总渲染：分段器态 + 当前场景面板 + 共享条。两个场景容器
/// 常驻 DOM，只画当前场景（设计稿 renderWorkspace 同款 —— 整页重画，
/// 生产保留静态骨架，只填场景这一段）。摆位图的数据填充在 kvm.js 的
/// renderWorkspaceTopology（render() 末尾的 renderKvm 会调它）。
/// 场景面板的两次渲染缓存 + 焦点保持：面板里全是按钮（切源 / 双机卡片 /
/// 管理），轮询 1.2 秒整段重画一次 —— 不还焦点，键盘用户点一下就被打回
/// 页首。套路和传输列表的 renderTransferList 相同（缓存 + 锚点还焦），
/// 另外缓存还顺带保住 switchTo 挂上的 .is-switching（动画放一半被重绘
/// 会闪断）。
let sceneCache = { single: null, multi: null };

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
  if (isMulti) paintScene(multi, multiSceneHtml, 'multi');
  else paintScene(single, singleSceneHtml, 'single');
  // 共享条：开关态 + 一句状态文案（设计稿 sharing-strip 的 copy）
  const k = snap.kvm || {};
  flip('kvmSwitch', k.on);
  flip('settingsKvmSwitch', k.on);
  const kc = document.getElementById('kvmCopy');
  if (kc) kc.textContent = k.on
    ? (isMulti ? '鼠标推到边缘即可跨屏' : '随画面切换，自动交接')
    : '已暂停，键鼠留在本机';
  flip('clipBtn', snap.clipboard);
  flip('settingsClipSwitch', snap.clipboard);
  const cc = document.getElementById('clipCopy');
  if (cc) cc.textContent = snap.clipboard ? '文字只同步到当前使用的设备' : '已关闭，保留各自的剪贴板';
}

/// 场景面板的"没变就不碰 DOM"绘制。data-scene-focus 是锚点：重绘前记下
/// 焦点所在那枚钮的锚，画完把焦点还回去（桩里 activeElement 为 null、
/// contains/querySelector 不存在，整段静默跳过）。
function paintScene(box, htmlFn, key) {
  const html = htmlFn();
  if (html === sceneCache[key]) return;
  const focused = document.activeElement;
  const anchor = focused && focused.closest && box.contains && box.contains(focused)
    ? focused.closest('[data-scene-focus]')
    : null;
  box.innerHTML = html;
  sceneCache[key] = html;
  if (anchor && anchor.dataset) {
    const again = box.querySelector('[data-scene-focus="' + anchor.dataset.sceneFocus + '"]');
    if (again && again.focus) again.focus({ preventScroll: true });
  }
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
  // snap 到位后补选设备页 pane（showPage 可能跑在 pull 之前，见 core.js）
  autoDevicePane();
  document.getElementById('node').textContent = snap.node || '';
  // P2：侧栏底部的本机名跟顶栏「本机」是同一个值。收成图标栏时这行字
  // 会被 CSS 藏掉（.compact-nav .local-machine > div），展开时才看得见。
  document.getElementById('navNode').textContent = snap.node || '';
  renderHeader();
  renderUpdate();
  renderWindowControls();

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
  // 切换进行态由 switchTo / askPeer 挂上（aria-busy + 场景面板 is-switching），
  // 轮询把按钮文案换回正常态时一并摘掉 —— 摘不掉的话读屏软件会永远念"忙"
  main.removeAttribute('aria-busy');
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
    main.innerHTML = icon('swap') + '<span>切换到 ' + esc(tgt.label) + '</span>';
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

  // 正常连接时共享条已解释键鼠跟随，提示保留在按钮上；异常仍常驻说明下一步。
  main.title = step.textContent;
  step.hidden = !!(snap.has_display && tgt && peer && snap.support && snap.support.startsWith('✓'));
  // P4：工作台场景（单屏 / 多屏）。renderKvm 在末尾会补摆位图的数据。
  renderWorkspaceScene();

  // 管理输入源弹窗：开着的时候随轮询刷新（"已设为热键"要点完就看到），
  // 但聚焦输入框时让路 —— 重建节点会把打了一半的字抹掉（同 editingPorts
  // / codeDirty 的套路）。弹窗关着就不画。
  const pm = document.getElementById('portsModal');
  if (pm && pm.style.display !== 'none' && !editingPorts) renderPortsModal();
  renderKvm();
  // 设备页：名册列 + 三块随选中态/配对态变的文字（设计稿 renderDevices
  // 的生产版）。旧的 #peers 富卡片块已废 —— 对端信息收成名册紧凑行，
  // 详细地址和配对警示搬进 selected-detail（选中一台才展开）。
  renderDevicePane();

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
  const commandHotkey = document.getElementById('commandHotkey');
  if (commandHotkey) commandHotkey.textContent = snap.hotkey || '—';
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
  // 维护三钮（重新检测 / 扫描输入口 / 检测能否切换）：后端引擎线程串行
  // 跑探测类动作，一个没跑完别接第二个（连点两次只是白排队，扫描还会
  // 多黑几次屏）。跟 snap.maint_busy 一起禁用，动作结束自动恢复 ——
  // 设计稿 app.js:113 的 data-maintain 同款。主切换按钮"永远显示"的
  // 铁律不适用这三个：它们是排查动作，不是主路径。
  const maintBusy = !!snap.maint_busy;
  ['maintRescan', 'maintScan', 'maintCheck'].forEach(id => {
    const b = document.getElementById(id);
    if (b) b.disabled = maintBusy;
  });
}

/// 设备页名册列 + 详情列三块随选中态/配对态变的文字（设计稿
/// renderDevices / deviceRow / selected-detail / placement-note /
/// pair-summary 的生产版）。布局 pane 的摆位图（#kvmGrid / #kvmLoose /
/// #kvmLinks）归 kvm.js 的 renderKvm 管，这里只画名册和这几块文字。
///
/// 和已废除的 #peers 富卡片的分工：snap.peers 全是在线对端（ui.rs 的
/// live_peers），名册行因此不再标注"在不在线"—— 行内状态说键鼠开没开
/// （不开时心跳不来，表现和关机一模一样，不点破用户会去查网线）；
/// 地址、口未知、配对警示这些详细信息搬进 selected-detail，选中一台
/// 才展开，名册保持 59px 一台的紧凑度。
function renderDevicePane() {
  const peers = snap.peers || [];
  const k = snap.kvm || {};

  // ---- 名册列：本机行 / 已知设备数 / 紧凑行 ----
  const localName = document.getElementById('deviceLocalName');
  if (localName) localName.textContent = snap.node || '本机';
  const known = document.getElementById('deviceListCount');
  if (known) known.textContent = String(peers.length);

  // 重绘会重建节点，焦点会丢。记住名册里 focused 的那台，画完还给它的
  // 新节点（同 renderKvmGrid 的套路）—— 键盘用户 Tab 进行列选中一台，
  // 不该被 1.2s 一轮的轮询打回 body。
  const rowsBox = document.getElementById('deviceRows');
  const focusedPeer = document.activeElement && document.activeElement.dataset
    && document.activeElement.dataset.peer ? document.activeElement.dataset.peer : null;
  if (rowsBox) {
    rowsBox.innerHTML = peers.length ? peers.map(p => {
      const link = (k.links || []).find(l => l.node === p.node);
      const side = link
        ? (link.side ? ({ right: '右侧', left: '左侧', top: '上方', bottom: '下方' })[link.side] : '共用一块屏')
        : '未摆位';
      const sel = kvmSelected === p.node;
      return `<button class="device-row${sel ? ' selected' : ''}" data-peer="${esc(p.node)}"`
        + ` aria-pressed="${sel ? 'true' : 'false'}" onclick="selectDevice('${esc(p.node)}')">`
        + icon('monitor')
        + `<span><strong>${esc(p.node)}</strong>`
        + `<small>${side}${p.kvm_on ? '' : ' · 键鼠没开'}</small></span>`
        + `<i class="status-dot${p.kvm_on ? '' : ' off'}"></i>`
        + `</button>`;
    }).join('') : '<div class="empty">还没有发现在线的对端</div>';
    if (focusedPeer) {
      const again = rowsBox.querySelector('[data-peer="' + focusedPeer + '"]');
      if (again && typeof again.focus === 'function') again.focus();
    }
  }

  // ---- 名册列底：配对摘要（设计稿 pair-summary）----
  const pst = document.getElementById('pairSummaryText');
  if (pst) {
    pst.textContent = snap.paired
      ? (snap.peer_verified > 0 ? '设备身份已验证' : '配对码已填 · 等对端确认')
      : '尚未配对';
  }

  // ---- 摆位提示（设计稿 placement-note）----
  const pn = document.getElementById('placementNote');
  if (pn) {
    pn.innerHTML = icon('info') + '<span>'
      + (kvmSelected
        ? '已选 ' + esc(kvmSelected) + ' · 点方向放置，Delete 移回未摆位'
        : '共用一块屏与多屏布局可以同时使用。')
      + '</span>';
  }

  // ---- 选中详情（设计稿 selected-detail）----
  const sd = document.getElementById('selectedDetail');
  if (sd) {
    const p = kvmSelected ? peers.find(x => x.node === kvmSelected) : null;
    if (p) {
      // 它那块屏现在在哪个口：沿用旧 #peers 卡片的口名逻辑 —— 对端报的
      // 可能是匿名名（"输入源 0x12"），那时只写十六进制；没验过口就写
      // "口未知"，别拿对端自报的编号冒充本机可见的口（见 render() 注释）。
      const rawLabel = (p.label || p.input || '').trim();
      const hex = '0x' + (p.value || 0).toString(16).toUpperCase().padStart(2, '0');
      const portLine = p.knows_port
        ? (rawLabel && !/^输入源/.test(rawLabel) ? rawLabel : hex)
        : '口未知（它的画面现在不在它那边）';
      // 对端设了码而本机没设：指令和文件都会被它在握手处拒掉。选中这台
      // 时必须说清，否则用户对着"点了没反应"查网线。
      const blocked = p.needs_pair
        ? '<div style="color:var(--danger)">对端设了配对码、本机没设，先填同一个码。</div>'
        : '';
      sd.innerHTML = '<div><strong>' + esc(p.node) + '</strong><br>'
        + esc(p.addr) + ' · ' + esc(portLine) + ' · '
        + (p.kvm_on ? '共用键鼠已开' : '共用键鼠没开') + blocked + '</div>'
        + '<button class="btn-sm" onclick="sendTo(\'' + esc(p.node) + '\')">'
        + icon('upload') + '发文件</button>';
    } else {
      sd.innerHTML = '<div><strong>选择一台设备</strong><br>查看连接信息、调整位置</div>'
        + '<span class="tag ' + (snap.paired ? 'green' : 'warn') + '">'
        + (snap.paired ? '已配对' : '未配对') + '</span>';
    }
  }

  // ---- 配对表单文案（设计稿 pairingForm 的 pairHelp）----
  // waiting：本机填了码、对端还没验过身份。输入框只读 + 主钮禁用 +
  // aria-busy —— 这段时候改码只会让两边更不一致，想改码走取消配对。
  // 空码保存的拦截提示走 flash（savePair 里那句「先填一个配对码」），
  // 不占 field-help 位。
  const waiting = !!snap.paired && snap.peer_verified === 0;
  const help = document.getElementById('pairHelp');
  if (help) {
    help.className = 'field-help';
    help.textContent = waiting
      ? '配对码已保存，正在等待对端确认身份…'
      : '最多 32 个字符。建议使用较长的配对码。';
  }
  const code = document.getElementById('code');
  if (code) {
    if (waiting) { code.setAttribute('readonly', ''); code.setAttribute('aria-busy', 'true'); }
    else { code.removeAttribute('readonly'); code.removeAttribute('aria-busy'); }
  }
  const save = document.getElementById('pairSaveBtn');
  if (save) save.disabled = waiting;
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

/// 自绘标题栏的最大化钮：图标跟着快照里的窗口态翻面。
///
/// 最大化态不是只有这个钮能改 —— Win+↑、双击标题栏、系统窗口菜单都会动它，
/// 自己记一个布尔值一定会和现实对不上，所以认后端那个字段（主线程每次
/// Resized 重新读一次窗口）。只有 frame=none 的原生窗口里这颗钮才存在，
/// 浏览器打开时取不到元素，直接跳过。
function renderWindowControls() {
  const btn = document.getElementById('winMax');
  if (!btn || typeof btn.querySelector !== 'function') return;
  const use = btn.querySelector('use');
  if (!use || typeof use.getAttribute !== 'function') return;
  const want = snap.maximized ? '#i-win-restore' : '#i-win-max';
  if (use.getAttribute('href') === want) return;
  use.setAttribute('href', want);
  const label = snap.maximized ? '还原' : '最大化';
  btn.title = label;
  btn.setAttribute('aria-label', label);
}

// 渲染层：顶栏三事实（renderHeader）、主按钮选目标（nextTarget）、
// 输入口列表（renderPorts）、全页总渲染（render）。
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

/// 输入口列表。单独一个函数，因为改名期间要跳过它 —— 轮询重建会换掉
/// contenteditable 节点，用户打到一半的字就没了。
function renderPorts() {
  const ports = document.getElementById('ports');
  const blind = snap.has_display === false;
  if (!(snap.inputs || []).length) {
    if (blind) {
      // 这不是故障：画面在另一台机器上，本机这边连显示器都没枚举到，
      // DDC 通道根本不存在，所以自己无论如何切不回来 —— 只能请对端代劳。
      ports.innerHTML = `
        <div class="empty">本机没有检测到显示器：画面正在另一台机器上。</div>
        <div class="empty" style="margin-top:6px">画面在别处时，本机读不到这块屏，也发不出切换指令。要让画面回来，得由当前显示画面的那台来切。</div>`;
    } else {
      ports.innerHTML = `
        <div class="empty">显示器已连接，但没有报告输入源。</div>
        <div class="empty" style="margin-top:6px">点顶部「重新检测」再读一次。</div>`;
    }
    return;
  }
  ports.innerHTML = (snap.inputs || []).map(i => {
    const dead = i.presence === 'invalid';
    const presence = i.presence === 'occupied' ? '<span class="tag green">有主机</span>'
      : i.presence === 'idle' ? '<span class="tag plain">无信号</span>'
      : dead ? '<span class="tag plain">该口不存在</span>' : '';
    // 「该口不存在」不能点切换：固件不接受这个值，点了就是黑屏。
    // 所有「切换」都是描边小按钮 —— 主按钮只有顶栏那一个。
    const why = dead ? '显示器固件不接受这个输入口' : (i.current ? '画面已经在这个口上' : '');
    const switchBtn = `<button class="btn-sm" onclick="switchTo(${i.value})"`
      + `${dead || i.current ? ' disabled' : ''}`
      + `${why ? ` title="${why}"` : ''}>切换</button>`;
    return `<div class="port${i.current ? ' cur' : ''}${dead ? ' dead' : ''}">
      <span class="dot ${i.current || i.presence === 'occupied' ? 'on' : 'off'}"></span>
      <div class="grow">
        <div class="name"><span class="rename" contenteditable="true" spellcheck="false"
            data-input="${esc(i.name)}" data-orig="${esc(i.label)}"
            title="点一下可以改名（回车保存，清空则恢复默认名）"
            onfocus="editingPort=true"
            onblur="editingPort=false;saveLabel(this)"
            onkeydown="if(event.key==='Enter'){event.preventDefault();this.blur();}"
          >${esc(i.label)}</span> ${i.current ? '<span class="tag blue">本机</span>' : ''}</div>
        <div class="meta">0x${i.value.toString(16).toUpperCase().padStart(2, '0')} · ${i.verified ? '本机确认' : '待确认'}</div>
      </div>
      ${presence}
      <!-- 「该口不存在」也不能设为热键：热键一按就是黑屏，比点错更糟 ——
           它还是全局的，换回本机都得靠别的手段 -->
      <button class="btn-sm" onclick="setTarget(${i.value})"
        ${i.current || dead ? 'disabled' : ''}
        ${dead ? ' title="显示器固件不接受这个输入口"' : ''}>设为热键</button>
      ${switchBtn}
    </div>`;
  }).join('');
}

function render() {
  if (!snap) return;
  document.getElementById('node').textContent = snap.node || '';
  // P2：侧栏底部的本机名跟顶栏「本机」是同一个值。收成图标栏时这行字
  // 会被 CSS 藏掉（.compact-nav .local-machine > div），展开时才看得见。
  document.getElementById('navNode').textContent = snap.node || '';
  renderHeader();
  renderUpdate();

  const banner = document.getElementById('pairBanner');
  if (snap.paired && snap.peer_verified > 0) {
    banner.className = 'banner ok';
    const rejected = snap.rejected
      ? ` <span class="sub">· 另有 ${snap.rejected} 条消息未通过校验</span>` : '';
    banner.innerHTML = `<b>✓ 已配对 · 已连接</b> <span class="sub">双方身份已互相验证</span>${rejected}`;
  } else if (snap.paired) {
    // 只填了码、没有对端验过签：不能显示"已连接"，那是骗人的
    banner.className = 'banner warn';
    const hint = snap.rejected
      ? `<br><span class="sub">对端发来的 ${snap.rejected} 条消息没通过校验：多半是两边码不一样，或有一边版本较旧</span>`
      : '';
    banner.innerHTML = `<b>配对码已填，但还没有对端确认</b>${hint}`;
  } else {
    banner.className = 'banner warn';
    // P2：横幅在工作台页，配对卡在设备页 —— 未配对时给一个「去配对」
    // 直接跳过去（showPage 在 core.js）。文案包进 span.grow，
    // 按钮才落到横幅右侧，不和文字挤成一团。
    banner.innerHTML = '<span class="grow"><b>未配对</b> <span class="sub">同一网段里的任何设备都能控制这块屏，建议先配对</span></span>'
      + '<button class="btn-sm" onclick="showPage(\'devices\')">去配对</button>';
  }
  const replayNote = snap.replayed
    ? `<br><span class="sub">另有 ${snap.replayed} 条重复消息已忽略（网络重发，属正常现象）</span>` : '';
  if (replayNote) banner.innerHTML += replayNote;

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

  // 输入源列表。改名期间不重建（否则节点被换掉，用户输入丢失）。
  if (!editingPort) renderPorts();

  // 固件声明过、本机从没验证过也没有信号的口不列：厂商常把同一块面板所有
  // SKU 的编码都写进能力表，机器上未必真有那个接口，列出来只会被读成"本机
  // 的输入源"。真要用了有两条正路：
  //   某个口接上了新设备 → 点「扫描输入口」，扫到就自动出现；
  //   整块换了显示器     → 点「重新检测」，按新显示器的能力表重新建表。
  const hint = document.getElementById('portsHint');
  hint.textContent = snap.port_note
    ? `上次检测：${snap.port_note}`
    : (snap.inputs && snap.inputs.length
      ? '列表里没有某个口：接上设备后点「扫描输入口」。扫描会逐个试每个口，期间屏幕会黑几次。'
      : '');
  document.getElementById('displayModel').textContent = snap.display ? '· ' + snap.display : '';
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

  document.getElementById('inbox').textContent = '接收目录：' + (snap.inbox || '—');
  document.getElementById('transfer').textContent = snap.transfer || '';
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
  document.getElementById('target').textContent = snap.target || '未设置';

  // 开关用 .sw 表达状态，不再拿「已开启 / 已关闭」两个按钮去冒充
  const kc = snap.kvm || {};
  const flip = (id, on) => {
    const node = document.getElementById(id);
    if (!node) return;
    node.classList.toggle('on', !!on);
    node.setAttribute('aria-checked', String(!!on));
  };
  flip('autoBtn', snap.autostart);
  flip('clipBtn', snap.clipboard);
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

// 这次切屏会不会顺手把键鼠带走 —— 文案要跟实际行为一致，
// 否则用户按了按钮发现鼠标没了会以为出 bug。
// **问后端**（will_follow = 有共用屏链路 && 本机能抓取）。原来自己推
// `k.mode !== 'edge' && role === 'console'`：k.mode 只取第一条链路，
// 而且 role 在 v0.6.49 之后已经降级成提示，判据跟 do_switch 不一致。
function kvmFollowing() {
  return !!(snap && snap.kvm && snap.kvm.will_follow);
}

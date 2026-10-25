// 基础层：共享状态（snap / busy / codeDirty / editingPort / flashUntil）、
// 取数通道（api / post / pull）和两个小工具（esc / flash）。
// 其余三个脚本都依赖这里的全局变量与函数。
let snap = null;
let busy = false;
/// 用户动过输入框就别再被后台轮询覆盖，否则清空的码会自己弹回来
let codeDirty = false;
/// 正在给某个输入口改名。轮询每 1.2 秒把 #ports 整个重建一次，
/// 重建会换掉 contenteditable 的节点 —— 用户打两个字输入就没了
/// （实测：聚焦后打字，等一轮轮询，节点被替换、文字回到旧值）。
/// 改名期间跳过那一列的重建，和 codeDirty 保护配对码输入框是同一个模式。
let editingPort = false;
/// flash() 的提示要在页面上活满 3.2 秒，而轮询每 1.2 秒会重写那一行。
/// 存截止时间，pull() 到这个点之前不动它。
let flashUntil = 0;

async function api(path, body) {
  const opt = { method: 'POST', headers: { 'Content-Type': 'application/json' } };
  if (body) opt.body = JSON.stringify(body);
  const res = await fetch(path, opt);
  return res.json().catch(() => ({ ok: true }));
}

function post(path) {
  if (busy) return;
  api(path).then(pull);
}

async function pull() {
  try {
    const res = await fetch('/api/state', { cache: 'no-store' });
    snap = await res.json();
    render();
  } catch (e) {
    document.getElementById('pairBanner').textContent = '控制台与主程序失联了';
  }
}

function esc(text) {
  return String(text == null ? '' : text).replace(/[&<>"]/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;'}[c]));
}

/// 一句轻提示（放在顶栏状态行里，3 秒后淡出）。
/// 以前这里静默 return，点了没反应 —— 用户分不清"程序坏了"和"确实切不了"。
function flash(title, sub) {
  const line = document.getElementById('status');
  line.textContent = sub ? `${title} · ${sub}` : title;
  line.style.display = '';
  // 轮询 1.2s 一次，会把这一行改回后端状态 —— 记下截止时间让 pull() 让路
  flashUntil = Date.now() + 3200;
  clearTimeout(flash.timer);
  flash.timer = setTimeout(() => {
    line.textContent = '';
    line.style.display = 'none';
    flashUntil = 0;
  }, 3200);
}

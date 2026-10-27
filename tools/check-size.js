#!/usr/bin/env node
// 体量门禁：`src/**/*.rs` 与 `assets/panel/**` 的行数棘轮。
//
// 用法：
//   node tools/check-size.js              # 检查，违规时退出码 1
//   node tools/check-size.js --update     # 把基线往下调（只降不升）
//   node tools/check-size.js --update --allow-grow <文件>   # 明确允许某个文件涨
//   node tools/check-size.js --rename <旧路径>:<新路径>     # 文件改名，基线数值原样继承
//
// 为什么要有 --rename：`git mv a.rs a/mod.rs` 之后，旧键会被当成"已不存在"
// 删掉、新键被当成"新文件"要求 <= 800。可改名一行代码都没多，按新文件卡它
// 等于逼人先把 4202 行切完才能改名——顺序正好反了。棘轮的本意是"每改一次
// 小一点"，改名不改大小，数值就该跟着走。
//
// 两条规则：
//   1. **存量只准变小不准变大**（棘轮）。超出现有基线即失败；比基线小的文件
//      会提示"可以把基线调到 N"，用 --update 收紧。
//   2. **新文件直接合规**：不在基线里的文件必须 <= NEW_FILE_LIMIT。
//
// 为什么用棘轮而不是硬线：本仓库 tray.rs 4202 行、kvm/mod.rs 3570 行是历史
// 事实，一刀切 300 行等于要求一次几千行的大重排——那会和协作者在飞的改动
// 全面冲突。棘轮让"每改一次小一点"成为唯一允许的方向，且基线就钉在脚本里，
// 不靠人记。（cc-bridge 用同一招管前端组件，见其 CLAUDE.md 规则 7。）

const fs = require('fs');
const path = require('path');

const ROOT = path.resolve(__dirname, '..');

// 新文件的上限。Rust 侧取 800：现有中位数在 250 行左右，而 win.rs(1723)、
// kvm/mac.rs(1231) 说明这个项目单文件上千行是常态，取 300 会逼着人乱切。
const NEW_FILE_LIMIT = 800;
// 面板前端是手写 JS/CSS、无构建无框架，style.css 529 行是设计令牌的自然体量。
const PANEL_NEW_FILE_LIMIT = 400;

// 基线：2026-10-25 全量实测。**只降不升**，调整必须走 --update。
const RUST_BASELINE = {
  'src/autostart.rs': 281,
  'src/caps.rs': 195,
  'src/clipboard.rs': 646,
  'src/ddc/linux.rs': 133,
  'src/ddc/mac.rs': 655,
  'src/ddc/mod.rs': 105,
  'src/ddc/win.rs': 518,
  'src/edid.rs': 185,
  'src/gui.rs': 17,
  'src/hidden.rs': 20,
  'src/kvm/mac.rs': 1231,
  'src/kvm/mod.rs': 371,
  'src/kvm/win.rs': 1723,
  'src/lib.rs': 38,
  'src/main.rs': 218,
  'src/models.rs': 252,
  'src/panel.rs': 826,
  'src/panelwin.rs': 194,
  'src/pair.rs': 201,
  'src/peer.rs': 547,
  'src/support.rs': 249,
  'src/transfer.rs': 448,
  'src/tray/mod.rs': 484,
  'src/config/mod.rs': 26,
  'src/switcher/mod.rs': 71,
  'src/kvm/event/mod.rs': 51
};

const PANEL_BASELINE = {
  'assets/panel/actions.js': 97,
  'assets/panel/core.js': 72,
  'assets/panel/index.html': 229,
  'assets/panel/kvm.js': 395,
  'assets/panel/render.js': 280,
  'assets/panel/style.css': 578
};

// exts 是数组。注意别把数组直接喂给 endsWith —— 它会被 ToString 成
// ".js,.css,.html" 从而永远不匹配（这个 bug 让面板六件全被误判成"已不存在"）。
function walk(dir, exts, out = []) {
  for (const name of fs.readdirSync(dir)) {
    const full = path.join(dir, name);
    if (fs.statSync(full).isDirectory()) walk(full, exts, out);
    else if (exts.some((e) => full.endsWith(e))) out.push(full);
  }
  return out;
}

function countLines(file) {
  const text = fs.readFileSync(file, 'utf8');
  // 与 wc -l 一致：末尾无换行时那一行也计数
  if (text.length === 0) return 0;
  return text.split('\n').length - (text.endsWith('\n') ? 1 : 0);
}

function rel(file) {
  return path.relative(ROOT, file).split(path.sep).join('/');
}

// 返回 { violations: [], canTighten: [], seen: Set }
function check(baseline, dir, exts, limit) {
  const violations = [];
  const canTighten = [];
  const seen = new Set();

  for (const file of walk(path.join(ROOT, dir), exts)) {
    const key = rel(file);
    seen.add(key);
    const n = countLines(file);
    if (!(key in baseline)) {
      if (n > limit) {
        violations.push(`新文件 ${key} ${n} 行 > 上限 ${limit}（新文件必须直接合规）`);
      }
      continue;
    }
    const base = baseline[key];
    if (n > base) {
      violations.push(`${key} ${n} 行 > 基线 ${base} 行（棘轮只准变小，涨了 ${n - base} 行）`);
    } else if (n < base) {
      canTighten.push({ key, from: base, to: n });
    }
  }

  // 基线里已不存在的文件（被删/被改名）：提示清掉条目
  for (const key of Object.keys(baseline)) {
    if (!seen.has(key)) {
      canTighten.push({ key, from: baseline[key], to: 0, gone: true });
    }
  }
  return { violations, canTighten, seen };
}

const args = process.argv.slice(2);
const update = args.includes('--update');
const allowGrow = args.filter((a, i) => args[i - 1] === '--allow-grow');

// 改名：基线数值原样搬到新键上，旧键删掉。不带 --update 时先打印再退出，
// 让人确认；带了 --update 就直接写回。
const renames = args.filter((a, i) => args[i - 1] === '--rename');
function applyRenames() {
  for (const spec of renames) {
    const [from, to] = spec.split(':');
    if (!from || !to) {
      console.error(`--rename 要写成 <旧路径>:<新路径>，收到的是 "${spec}"`);
      process.exit(2);
    }
    if (RUST_BASELINE[from]) {
      RUST_BASELINE[to] = RUST_BASELINE[from];
      delete RUST_BASELINE[from];
      console.log(`基线改名 ${from} → ${to}（${RUST_BASELINE[to]} 行原样继承）`);
    } else if (PANEL_BASELINE[from]) {
      PANEL_BASELINE[to] = PANEL_BASELINE[from];
      delete PANEL_BASELINE[from];
      console.log(`基线改名 ${from} → ${to}（${PANEL_BASELINE[to]} 行原样继承）`);
    } else {
      console.error(`基线里没有 ${from}，没法改名`);
      process.exit(2);
    }
  }
}

// 改名必须先于 check：check 拿基线判断"新文件/超基线"，用旧键算出来的
// 结论全是错的（旧键已不存在、新键不在基线里）。
if (renames.length > 0) {
  applyRenames();
  if (!update) {
    // 只改名不写回：让调用方自己决定，别偷偷改脚本
    console.log('（加 --update 才会把改名写回脚本）');
    process.exit(0);
  }
}

const rust = check(RUST_BASELINE, 'src', ['.rs'], NEW_FILE_LIMIT);
const panel = check(PANEL_BASELINE, 'assets/panel', ['.js', '.css', '.html'], PANEL_NEW_FILE_LIMIT);

const violations = [...rust.violations, ...panel.violations];
const canTighten = [...rust.canTighten, ...panel.canTighten];

if (update) {
  // 只降不升：把"变小了"的条目写回脚本，涨了的条目必须显式 --allow-grow
  let changed = 0;
  for (const { key, to, gone } of canTighten) {
    if (gone) {
      delete (RUST_BASELINE[key] || PANEL_BASELINE[key] ? RUST_BASELINE : PANEL_BASELINE)[key];
      console.log(`基线移除已不存在的 ${key}`);
      changed++;
      continue;
    }
    if (RUST_BASELINE[key]) RUST_BASELINE[key] = to;
    else if (PANEL_BASELINE[key]) PANEL_BASELINE[key] = to;
    else continue;
    console.log(`基线 ${key} → ${to}`);
    changed++;
  }
  // 显式放行：--allow-grow <文件> 把该文件的现状值设为新基线。
  // 文档里一直有这个开关，但实现没接上（allowGrow 算出来没人用），
  // 真需要时只能手改脚本——那正是棘轮想避免的"人记"。
  for (const key of allowGrow) {
    const base = RUST_BASELINE[key] ? RUST_BASELINE : PANEL_BASELINE[key] ? PANEL_BASELINE : null;
    if (!base) {
      console.error(`--allow-grow ${key}：基线里没有这个文件（新文件不用放行，直接合规就行）`);
      process.exit(2);
    }
    const onDisk = new Set([...rust.seen, ...panel.seen]).has(key);
    if (!onDisk) {
      console.error(`--allow-grow ${key}：磁盘上找不到这个文件`);
      process.exit(2);
    }
    const now = countLines(path.join(ROOT, key));
    if (now < base[key]) {
      console.error(`--allow-grow ${key}：它比基线还小（${now} < ${base[key]}），该用 --update 收紧`);
      process.exit(2);
    }
    base[key] = now;
    console.log(`基线 ${key} → ${now}（--allow-grow 显式放行）`);
    changed++;
  }
  // 改过名也算"有东西要写回"：光改名时一条基线都不用收紧，但键必须落盘，
  // 否则下次跑又变回"新文件超限"。
  if (changed || renames.length) {
    const body = `const RUST_BASELINE = ${JSON.stringify(RUST_BASELINE, null, 2).replace(/"([^"]+)":/g, "'$1':").replace(/"/g, "'")};\n\nconst PANEL_BASELINE = ${JSON.stringify(PANEL_BASELINE, null, 2).replace(/"([^"]+)":/g, "'$1':").replace(/"/g, "'")};`;
    const self = fs.readFileSync(__filename, 'utf8');
    // 正则要连尾部所有分号一起吃干净（`\};+`），替换体自己也已带一个 `;`：
    // 早先只吃到第一个 `};` 又多补一个 `;`，结果每跑一次 --update 块尾就
    // 多攒一个分号（空语句，无害，但每次 diff 都多一行垃圾）。
    const replaced = self
      .replace(/const RUST_BASELINE = \{[\s\S]*?\};+/, body.split('\n\n')[0])
      .replace(/const PANEL_BASELINE = \{[\s\S]*?\};+/, body.split('\n\n')[1]);
    fs.writeFileSync(__filename, replaced);
    console.log(`已写回基线（${changed} 条收紧 + ${renames.length} 条改名）`);
  } else {
    console.log('没有可收紧的基线');
  }
  process.exit(violations.length ? 1 : 0);
}

if (violations.length) {
  console.error('✗ 体量门禁未通过：');
  for (const v of violations) console.error('  ' + v);
  if (canTighten.length) {
    console.error('');
    console.error('另有可收紧的基线（跑 node tools/check-size.js --update）：');
    for (const { key, from, to } of canTighten.filter((c) => !c.gone)) {
      console.error(`  ${key} ${from} → ${to}`);
    }
  }
  process.exit(1);
}

if (canTighten.length) {
  console.log('✓ 无超限文件。以下基线可以收紧（--update）：');
  for (const { key, from, to, gone } of canTighten) {
    console.log(`  ${key}${gone ? '（已不存在，条目可删）' : ` ${from} → ${to}`}`);
  }
} else {
  console.log('✓ 体量门禁通过，基线已是最紧');
}

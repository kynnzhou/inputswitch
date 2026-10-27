#!/usr/bin/env node
// 生成 updater.json：自动更新（src/update/）要拉的清单。
// 扫一个产物目录，按文件名认出各平台的包，算 SHA-256，拼出 GitHub Release
// 的下载地址。字段与 src/update/manifest.rs 的 Manifest 一一对应。
//
// 用法：
//   node tools/gen-updater-json.js <产物目录>                  # 生成 <目录>/updater.json
//   node tools/gen-updater-json.js <目录> --check              # 重算比对，防手改
//   node tools/gen-updater-json.js <目录> --notes-file f --pub-date 2026-10-28T00:00:00Z
//   node tools/gen-updater-json.js <目录> --base-url https://127.0.0.1:8443   # 联调假源
//
// 约定（两处必须同步，改一边另一边会悄悄坏掉）：
//   - 平台键：src/update/manifest.rs 的 platform_key()
//   - 仓库与产物命名：src/update/source.rs 的 REPO / pack-windows.js / build.sh
// 版本号默认读 Cargo.toml —— 和 src/update/mod.rs 的 crate::VERSION 同源。
const fs = require('fs');
const path = require('path');
const crypto = require('crypto');

const ROOT = path.resolve(__dirname, '..');
const fail = (msg) => {
  console.error(`生成失败：${msg}`);
  process.exit(1);
};

// 与 src/update/source.rs 的 REPO 对齐
const DEFAULT_REPO = 'kynnzhou/inputswitch';

// 文件名 → 平台键。故意只认这三种：别的文件放进产物目录也不会被误发布。
const PLATFORMS = [
  { key: 'windows-x86_64', test: (f) => /^InputSwitch_.+_x64-setup\.exe$/.test(f) },
  { key: 'darwin-aarch64', test: (f) => f === 'InputSwitch-macOS-arm64.zip' },
  { key: 'darwin-x86_64', test: (f) => f === 'InputSwitch-macOS-x64.zip' },
];

// ---------- 参数 ----------
const args = process.argv.slice(2);
const dir = args.find((a) => !a.startsWith('--'));
const flag = (name) => args.includes(name);
const value = (name, dft) => {
  const i = args.indexOf(name);
  return i >= 0 && args[i + 1] && !args[i + 1].startsWith('--') ? args[i + 1] : dft;
};
if (!dir || flag('--help')) {
  console.log('用法：node tools/gen-updater-json.js <产物目录> [--check] [--version X.Y.Z]');
  console.log('      [--repo owner/name] [--base-url URL] [--notes 文本 | --notes-file 文件]');
  console.log('      [--pub-date ISO] [--out 路径]');
  process.exit(dir ? 0 : 1);
}
const distDir = path.resolve(dir);
if (!fs.statSync(distDir, { throwIfNoEntry: false })?.isDirectory()) {
  fail(`目录不存在：${distDir}`);
}

// ---------- 版本号：Cargo.toml 是真源 ----------
function cargoVersion() {
  const m = fs
    .readFileSync(path.join(ROOT, 'Cargo.toml'), 'utf8')
    .match(/^version = "([0-9][0-9.]*)"/m);
  if (!m) fail('Cargo.toml 里读不到 version');
  return m[1];
}
const version = value('--version', cargoVersion());
if (!/^[0-9]+(\.[0-9]+){1,3}$/.test(version)) fail(`版本号不合法：${version}`);

const repo = value('--repo', DEFAULT_REPO);
const baseUrl = value(
  '--base-url',
  `https://github.com/${repo}/releases/download/v${version}`
).replace(/\/+$/, '');
if (!baseUrl.startsWith('https://')) {
  // 清单里的 url 必须是 https：src/update/manifest.rs 的 check_url 会拒
  fail(`base-url 必须是 https（清单地址不过 https，摘要校验就失去意义）：${baseUrl}`);
}
const outPath = path.resolve(value('--out', path.join(distDir, 'updater.json')));

// ---------- notes ----------
let notes = value('--notes', '');
const notesFile = value('--notes-file', '');
if (notesFile) {
  if (!fs.existsSync(notesFile)) fail(`notes 文件不存在：${notesFile}`);
  notes = fs.readFileSync(notesFile, 'utf8').trim();
}

// ---------- pub_date ----------
// ISO 8601 秒级；manifest.rs 只用于展示，不参与比较
const pubDate = value('--pub-date', new Date().toISOString().replace(/\.\d+Z$/, 'Z'));

// ---------- 扫产物、算摘要 ----------
function sha256(file) {
  return crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
}

function scan() {
  const files = fs.readdirSync(distDir);
  const found = [];
  for (const f of files) {
    if (f === 'updater.json') continue; // 上一轮的输出，不是产物
    const p = PLATFORMS.find((x) => x.test(f));
    if (!p) continue;
    found.push({ key: p.key, file: f, checksum: sha256(path.join(distDir, f)) });
  }
  if (found.length === 0) {
    fail(
      `${distDir} 里没有可发布的产物。\n` +
        `  期望这些文件名之一：${PLATFORMS.map((p) => p.key).join(' / ')} 对应的包\n` +
        '  Windows：InputSwitch_<版本>_x64-setup.exe（tools/pack-windows.js）\n' +
        '  macOS：InputSwitch-macOS-arm64.zip（build.sh）'
    );
  }
  // 键排序，和 Rust 侧 BTreeMap 的序列化顺序一致，diff 才稳定
  found.sort((a, b) => a.key.localeCompare(b.key));
  return found;
}

// ---------- --check：重算比对 ----------
if (flag('--check')) {
  if (!fs.existsSync(outPath)) fail(`没有 ${outPath}，先生成`);
  const old = JSON.parse(fs.readFileSync(outPath, 'utf8'));
  let bad = 0;
  const say = (ok, msg) => {
    console.log(`${ok ? '  ✓' : '  ✗'} ${msg}`);
    if (!ok) bad++;
  };
  say(old.version === version, `version：${old.version}${old.version === version ? '' : ` ≠ ${version}`}`);
  say(typeof old.notes === 'string', 'notes 是字符串');
  say(!!old.pub_date, `pub_date：${old.pub_date || '（缺）'}`);
  for (const f of scan()) {
    const entry = old.platforms?.[f.key];
    if (!entry) {
      say(false, `${f.key}：清单里没有这一平台`);
      continue;
    }
    const expectUrl = `${baseUrl}/${f.file}`;
    say(entry.url === expectUrl, `${f.key} url：${entry.url || '（缺）'}`);
    say(
      entry.checksum === f.checksum,
      `${f.key} 摘要：${entry.checksum === f.checksum ? '与磁盘文件一致' : `清单 ${entry.checksum} ≠ 磁盘 ${f.checksum}`}`
    );
  }
  process.exit(bad === 0 ? 0 : 1);
}

// ---------- 生成 ----------
const found = scan();
const manifest = {
  version,
  notes,
  pub_date: pubDate,
  platforms: {},
};
for (const f of found) {
  manifest.platforms[f.key] = { url: `${baseUrl}/${f.file}`, checksum: f.checksum };
}
fs.writeFileSync(outPath, JSON.stringify(manifest, null, 2) + '\n');

console.log(`已生成 ${path.relative(ROOT, outPath)}`);
for (const f of found) {
  console.log(`  ${f.key}: ${f.file}`);
  console.log(`    url: ${manifest.platforms[f.key].url}`);
  console.log(`    sha256: ${f.checksum}`);
}
if (!notes) {
  console.log('  提示：没给 --notes/--notes-file，更新说明是空的（弹窗里只会显示版本号）');
}

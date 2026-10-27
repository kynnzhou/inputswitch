#!/usr/bin/env node
// 把 Windows 产物打成 NSIS 安装器：dist/windows/InputSwitch_<version>_x64-setup.exe
//
// 编译不归这里管（先跑 .\build.ps1 或 cargo build），这个脚本只做三件事：
//   1. stage：把 exe / dll 拷进 .build/stage（**无空格路径** —— makensis 的
//      /D 传值遇空格会被截断，仓库路径无空格，临时文件一律落 .build）
//   2. makensis：调 packaging/windows/InputSwitch.nsi，产物落到 .build/ 中转
//      （OutFile 路径同样不能有空格）
//   3. rename：搬到 dist/windows/InputSwitch_<version>_x64-setup.exe
//      （这个命名是 updater.json 的约定，src/update/source.rs 的测试在用）
//
// 用法：
//   node tools/pack-windows.mjs                 # 全流程
//   node tools/pack-windows.mjs --skip-makensis # 只 stage（本机没装 NSIS 时验证前半段）
//   node tools/pack-windows.mjs --version 9.9.9 # 覆盖版本号（默认读 Cargo.toml）
//
// 本机没装 NSIS 时报中文错误并给出安装办法；CI（release.yml）用 choco 装。
const fs = require('fs');
const path = require('path');
const crypto = require('crypto');
const { spawnSync } = require('child_process');

const ROOT = path.resolve(__dirname, '..');
const fail = (msg) => {
  console.error(`打包失败：${msg}`);
  process.exit(1);
};

// ---------- 参数 ----------
const args = process.argv.slice(2);
const flag = (name) => args.includes(name);
const value = (name, dft) => {
  const i = args.indexOf(name);
  return i >= 0 && args[i + 1] && !args[i + 1].startsWith('--') ? args[i + 1] : dft;
};

// ---------- 版本号：唯一来源是 Cargo.toml ----------
function cargoVersion() {
  const text = fs.readFileSync(path.join(ROOT, 'Cargo.toml'), 'utf8');
  const m = text.match(/^version = "([0-9][0-9.]*)"/m);
  if (!m) fail('Cargo.toml 里读不到 version（应该有 version = "X.Y.Z"）');
  return m[1];
}
const version = value('--version', cargoVersion());
if (!/^[0-9]+(\.[0-9]+){1,3}$/.test(version)) fail(`版本号不合法：${version}`);

// ---------- 输入 ----------
const gui = path.join(ROOT, 'target/release/InputSwitch.exe');
const cli = path.join(ROOT, 'target/release/inputswitch-cli.exe');
const dll = path.join(ROOT, 'tools/webview2/x64/WebView2Loader.dll');
if (!fs.existsSync(gui)) {
  fail(`找不到 ${gui}，先编译：cargo build --release --bin InputSwitch`);
}

// ---------- 1. stage ----------
const stage = path.join(ROOT, '.build/stage');
fs.rmSync(stage, { recursive: true, force: true });
fs.mkdirSync(stage, { recursive: true });
const copy = (from, to) => {
  fs.copyFileSync(from, path.join(stage, to));
  console.log(`  stage: ${to}（${fs.statSync(from).size} 字节）`);
};
copy(gui, 'InputSwitch.exe');
if (fs.existsSync(cli)) copy(cli, 'inputswitch-cli.exe');
else console.log('  提示：没有 inputswitch-cli.exe（未构建 CLI？），安装器里不带它');
if (fs.existsSync(dll)) copy(dll, 'WebView2Loader.dll');
// MSVC 构建静态链入 loader，没有这个 dll 是正常的，不提示

// ---------- 2. makensis ----------
if (flag('--skip-makensis')) {
  console.log(`\n--skip-makensis：staging 完成（${stage}），跳过安装器打包`);
  process.exit(0);
}

const nsi = path.join('packaging/windows/InputSwitch.nsi');
if (!fs.existsSync(path.join(ROOT, nsi))) fail(`找不到 ${nsi}`);

// 先探一下 makensis 在不在 PATH 里：给中文错误，别让 spawn 的 ENOENT 裸奔
const probe = spawnSync('makensis', ['/VERSION'], { encoding: 'utf8' });
if (probe.error || probe.status !== 0) {
  fail(
    '系统里没有 makensis（NSIS）。\n' +
      '  本机装一个：https://nsis.sourceforge.io/Download ，或 choco install nsis -y\n' +
      '  只想验证 staging 前半段：node tools/pack-windows.mjs --skip-makensis'
  );
}

const out = path.join('.build', 'InputSwitch-setup.exe');
const r = spawnSync(
  'makensis',
  [`/DVERSION=${version}`, `/DSTAGING=${stage}`, `/DOUT=${out}`, nsi],
  { cwd: ROOT, encoding: 'utf8' }
);
if (r.stdout) process.stdout.write(r.stdout);
if (r.status !== 0) {
  if (r.stderr) process.stderr.write(r.stderr);
  fail(`makensis 退出码 ${r.status}`);
}

// ---------- 3. 搬到 dist/windows ----------
const built = path.join(ROOT, out);
if (!fs.existsSync(built)) fail(`makensis 报成功但找不到 ${out}`);
const finalName = `InputSwitch_${version}_x64-setup.exe`;
const dist = path.join(ROOT, 'dist/windows');
fs.mkdirSync(dist, { recursive: true });
const finalPath = path.join(dist, finalName);
fs.copyFileSync(built, finalPath);
fs.rmSync(built);
fs.rmSync(stage, { recursive: true, force: true });

// 顺口报告摘要，发布时核对用
const buf = fs.readFileSync(finalPath);
const sum = crypto.createHash('sha256').update(buf).digest('hex');
console.log(`\n安装器: ${path.relative(ROOT, finalPath)}`);
console.log(`  大小: ${(buf.length / 1024 / 1024).toFixed(2)} MB`);
console.log(`  SHA-256: ${sum}`);
console.log(`\n静默安装（自动更新走的就是这条）：${finalName} /S`);
console.log('生成 updater.json：node tools/gen-updater-json.mjs dist/windows');

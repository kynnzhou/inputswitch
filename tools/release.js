#!/usr/bin/env node
// 发版编排器：把「提交 → tag → GitHub Release」收成一个命令。
// 构建不归这里管 —— Windows 走 tools/pack-windows.js、macOS 走 ./build.sh，
// 产物落 dist/ 之后这个脚本负责剩下的。
//
// 正常发版路径其实是 CI：git push tag 后 .github/workflows/release.yml 自动
// 构建双端 + 生成 updater.json + 建 Release。这个脚本是**本地手工路径**
// （workflow 挂了要补包、或就是想本机发），以及发版前的自检。
//
// 用法：
//   node tools/release.js check              # 预检：能发吗（不碰任何东西）
//   node tools/release.js notes [--out f]    # 从 git log 攒更新说明
//   node tools/release.js tag                # 打 v<version> 并推送（--yes 才真干）
//   node tools/release.js publish <目录>     # gh release create + 上传（--yes 才真干）
//   node tools/release.js all <目录>         # check → notes → tag → publish 一条龙
//
// 默认全是 dry-run：只打印将要执行的命令，加 --yes 才真干。发版是不可逆动作，
// 「看清楚再按」比「少打一个字」重要。
const fs = require('fs');
const path = require('path');
const { spawnSync } = require('child_process');

const ROOT = path.resolve(__dirname, '..');
const fail = (msg) => {
  console.error(`发版中止：${msg}`);
  process.exit(1);
};
const info = (msg) => console.log(`  ${msg}`);

// ---------- 版本号：Cargo.toml 是真源（与 crate::VERSION 同源） ----------
function cargoVersion() {
  const m = fs
    .readFileSync(path.join(ROOT, 'Cargo.toml'), 'utf8')
    .match(/^version = "([0-9][0-9.]*)"/m);
  if (!m) fail('Cargo.toml 里读不到 version');
  return m[1];
}

function git(...a) {
  const r = spawnSync('git', a, { cwd: ROOT, encoding: 'utf8' });
  return { ok: r.status === 0, out: (r.stdout || '').trim(), err: (r.stderr || '').trim() };
}

// 推送链路探测：直连和代理 7897 在这台机器上轮流挂，推前两条都探，谁通走谁
function probePush() {
  const probes = [
    { how: 'direct', args: [] },
    { how: 'proxy', args: ['-x', 'http://127.0.0.1:7897'] },
  ];
  for (const p of probes) {
    const r = spawnSync(
      'curl',
      ['-m5', '-o', '/dev/null', '-w', '%{http_code}', ...p.args, 'https://github.com'],
      { encoding: 'utf8' }
    );
    if (r.status === 0 && (r.stdout || '').trim().startsWith('2')) {
      return p.how === 'proxy' ? ['-c', 'https.proxy=http://127.0.0.1:7897'] : ['-c', 'http.proxy=', '-c', 'https.proxy='];
    }
  }
  return null;
}

// ---------- check ----------
function cmdCheck(version) {
  console.log(`预检 v${version}：`);
  const dirty = git('status', '--porcelain');
  if (dirty.out) fail(`工作区有未提交改动，先提交（git status 有 ${dirty.out.split('\n').length} 行）`);
  info('✓ 工作区干净');

  const tagExists = git('rev-parse', '--verify', `v${version}`);
  if (tagExists.ok) fail(`tag v${version} 已存在（${tagExists.out}）。要重发先删旧 tag 和 release`);
  info(`✓ tag v${version} 未占用`);

  const gh = spawnSync('gh', ['--version'], { encoding: 'utf8' });
  if (gh.error) fail('没有 gh CLI（https://cli.github.com/）。CI 路径不受影响，本地发版需要它');
  const auth = spawnSync('gh', ['auth', 'status'], { encoding: 'utf8' });
  if (auth.status !== 0) {
    console.error((auth.stderr || auth.stdout || '').trim());
    fail('gh 未登录：gh auth login');
  }
  info('✓ gh 已安装且已登录');

  // 展示副本要跟真源同步（CLAUDE.md §2）：徽章是静态 SVG，漏改 = 徽章版本和真实版本不符
  const badge = fs.readFileSync(path.join(ROOT, 'assets/badge-release.svg'), 'utf8');
  const m = badge.match(/v\d+\.\d+\.\d+/);
  if (!m) fail('assets/badge-release.svg 里找不到版本号');
  if (m[0] !== `v${version}`) {
    fail(`assets/badge-release.svg 是 ${m[0]}，Cargo.toml 是 v${version} —— 两处都要改`);
  }
  info(`✓ 徽章版本 ${m[0]} 与 Cargo.toml 一致`);

  // tag 会从当前 HEAD 打。main 有 protect-main，确保要发布的东西已经在 main 上
  const head = git('rev-parse', '--short', 'HEAD');
  const branch = git('branch', '--show-current');
  info(`当前分支 ${branch.out} @ ${head.out} —— tag 会打在这里`);
  if (branch.out !== 'main') {
    console.log('  ⚠ 不在 main 上。确认这个提交已经合进 main（protect-main：推送走 PR）');
  }

  const repo = spawnSync('gh', ['repo', 'view', '--json', 'nameWithOwner'], { encoding: 'utf8' });
  if (repo.status === 0) info(`✓ 仓库可访问：${(repo.stdout || '').match(/"nameWithOwner":\s*"([^"]+)"/)?.[1] || '?'}`);
  else console.log('  ⚠ gh repo view 失败（网络？）。能登录通常就够，继续');

  console.log(`\n可以发。构建产物后：node tools/release.js publish <目录>`);
}

// ---------- notes ----------
function cmdNotes(version, outFile) {
  const latest = git('describe', '--tags', '--abbrev=0');
  let text;
  if (latest.ok) {
    const log = git('log', '--pretty=format:- %s', `${latest.out}..HEAD`);
    text = `自 ${latest.out} 以来的改动：\n${log.out || '（没有新提交）'}`;
  } else {
    text = git('log', '--pretty=format:- %s').out;
  }
  if (outFile) {
    fs.writeFileSync(outFile, text + '\n');
    console.log(`已写入 ${outFile}`);
  } else {
    console.log(text);
  }
}

// ---------- tag ----------
function cmdTag(version, yes) {
  const tag = `v${version}`;
  console.log(`将执行：git tag -a ${tag} -m "${tag}" && git push origin ${tag}`);
  if (!yes) {
    console.log('（dry-run：加 --yes 真干。tag 推上去会触发 release.yml 自动构建发布）');
    return;
  }
  const t = git('tag', '-a', tag, '-m', tag);
  if (!t.ok) fail(`打 tag 失败：${t.err}`);
  // 链路探测：直连和代理 7897 轮流挂，谁通走谁。-c 只对这一条命令生效，
  // 不写进仓库配置
  const extra = probePush();
  if (!extra) {
    fail(
      `直连和代理 7897 都连不上 github.com。tag 已在本地打好，网络恢复后手动推：git push origin ${tag}`
    );
  }
  const p = spawnSync('git', [...extra, 'push', 'origin', tag], { cwd: ROOT, encoding: 'utf8' });
  if (p.status !== 0) fail(`tag 推送失败：${(p.stderr || '').trim()}`);
  console.log(`已推送 ${tag} —— release.yml 应该开始跑了`);
}

// ---------- publish ----------
function cmdPublish(version, dir, yes, notesFile) {
  const dist = path.resolve(dir);
  if (!fs.statSync(dist, { throwIfNoEntry: false })?.isDirectory()) fail(`目录不存在：${dist}`);
  const files = fs
    .readdirSync(dist)
    .filter((f) => /\.(exe|zip)$/.test(f) || f === 'updater.json')
    .sort();
  if (files.length === 0) fail(`${dist} 里没有产物（exe / zip / updater.json）`);
  if (!files.includes('updater.json')) {
    fail(`没有 updater.json —— 没有它自动更新永远查不到新版。先跑：node tools/gen-updater-json.js ${dir}`);
  }

  const tag = `v${version}`;
  const view = spawnSync('gh', ['release', 'view', tag], { cwd: ROOT, encoding: 'utf8' });
  const exists = view.status === 0;
  const cmd = exists
    ? ['release', 'upload', tag, ...files.map((f) => path.join(dist, f)), '--clobber']
    : ['release', 'create', tag, ...files.map((f) => path.join(dist, f)), '--title', tag];
  if (!exists && notesFile) cmd.push('--notes-file', notesFile);
  else if (!exists) cmd.push('--notes', `InputSwitch ${tag}`);
  console.log(`将执行：gh ${cmd.join(' ')}`);
  if (!yes) {
    console.log('（dry-run：加 --yes 真干）');
    return;
  }
  const r = spawnSync('gh', cmd, { cwd: ROOT, encoding: 'utf8' });
  if (r.stdout) process.stdout.write(r.stdout);
  if (r.status !== 0) {
    if (r.stderr) process.stderr.write(r.stderr);
    fail(`gh 退出码 ${r.status}`);
  }
  console.log(`\n完成：https://github.com/kynnzhou/inputswitch/releases/tag/${tag}`);
  console.log('提醒：装了这个版本的机器要等下一版才会收到自动更新（清单里 latest 是现在这一版）');
}

// ---------- 入口 ----------
const args = process.argv.slice(2);
const yes = args.includes('--yes');
const rest = args.filter((a) => !a.startsWith('--'));
const sub = rest[0];
const version = cargoVersion();
const value = (name, dft) => {
  const i = args.indexOf(name);
  return i >= 0 && args[i + 1] ? args[i + 1] : dft;
};

switch (sub) {
  case 'check':
    cmdCheck(version);
    break;
  case 'notes': {
    const out = value('--out', '');
    cmdNotes(version, out);
    break;
  }
  case 'tag':
    cmdTag(version, yes);
    break;
  case 'publish':
    cmdPublish(version, rest[1], yes, value('--notes-file', ''));
    break;
  case 'all': {
    cmdCheck(version);
    cmdNotes(version, path.join(ROOT, '.build/release-notes.md'));
    cmdTag(version, yes);
    cmdPublish(version, rest[1], yes, path.join(ROOT, '.build/release-notes.md'));
    break;
  }
  default:
    console.log('用法：node tools/release.js check | notes | tag | publish <目录> | all <目录> [--yes]');
    process.exit(sub ? 1 : 0);
}

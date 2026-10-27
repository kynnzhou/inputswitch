#!/usr/bin/env node
// 假更新源：本机 HTTPS 服务 + 自签证书，联调 inputswitch-cli update 的整条链路。
//
// 为什么必须 HTTPS：src/update/fetch.rs 的 ensure_https 只放行 https，里面那条
// `http://127.0.0.1:` 的例外是 #[cfg(test)] 专属 —— 正式二进制里没有这条分支，
// 联调不值得为它开口子（见 fetch.rs 的注释）。所以这里自己签一张证书，并尝试
// 装进系统信任库，让系统 curl（Schannel / Secure Transport）认它。
//
// 用法：
//   node tools/fake-update-server.js <目录> [--port 8443] [--keep-cert] [--verify]
// 目录里放 updater.json + 假安装包（用 tools/gen-updater-json.js --base-url 生成）。
//
// 联调全流程：
//   # 终端 1
//   node tools/fake-update-server.js .build/fake-dist
//   # 终端 2（PowerShell）
//   $env:INPUTSWITCH_UPDATE_URL = "https://127.0.0.1:8443/updater.json"
//   target/debug/inputswitch-cli update            # 应看到 available
//   target/debug/inputswitch-cli update --download # 应下完并通过 SHA-256 校验
//   # 不要跑 --install：假 exe 会被真的执行
//
// 证书落在 .build/fake-ca/（gitignore 管着），默认退出时删；--keep-cert 保留。
const fs = require('fs');
const https = require('https');
const path = require('path');
const { spawnSync, execSync } = require('child_process');

const ROOT = path.resolve(__dirname, '..');
const fail = (msg) => {
  console.error(`假源启动失败：${msg}`);
  process.exit(1);
};

const args = process.argv.slice(2);
const dir = args.find((a) => !a.startsWith('--'));
const value = (name, dft) => {
  const i = args.indexOf(name);
  return i >= 0 && args[i + 1] ? args[i + 1] : dft;
};
if (!dir) {
  console.log('用法：node tools/fake-update-server.js <目录> [--port 8443] [--keep-cert] [--verify]');
  process.exit(1);
}
const root = path.resolve(dir);
if (!fs.statSync(root, { throwIfNoEntry: false })?.isDirectory()) fail(`目录不存在：${root}`);
const port = Number(value('--port', '8443'));
const keepCert = args.includes('--keep-cert');
const verify = args.includes('--verify');

// ---------- 自签证书 ----------
// 用 extfile 而不是 -addext：老版本 openssl（1.1.0 及一些 LibreSSL）不认
// -addext，extfile 从 1.1.1 之前就支持。CA:true 让它能装进 Root 存储当根证书。
const certDir = path.join(ROOT, '.build/fake-ca');
const keyPath = path.join(certDir, 'key.pem');
const certPath = path.join(certDir, 'cert.pem');
fs.rmSync(certDir, { recursive: true, force: true });
fs.mkdirSync(certDir, { recursive: true });
const extPath = path.join(certDir, 'openssl.cnf');
fs.writeFileSync(
  extPath,
  [
    '[req]',
    'distinguished_name = dn',
    // prompt = no：不让 openssl 交互式追问 DN 各字段（CI 和后台进程里会直接挂）
    'prompt = no',
    'x509_extensions = v3',
    '[dn]',
    'CN = localhost',
    '[v3]',
    'subjectAltName = IP:127.0.0.1,DNS:localhost',
    'basicConstraints = critical,CA:true',
    'keyUsage = critical,keyCertSign,digitalSignature',
    'extendedKeyUsage = serverAuth',
    '',
  ].join('\n')
);
const openssl = spawnSync(
  'openssl',
  [
    'req', '-x509', '-newkey', 'rsa:2048', '-nodes',
    '-keyout', keyPath, '-out', certPath,
    '-days', '2', '-config', extPath, '-extensions', 'v3',
    // -subj 双保险：老版本 openssl 不认 config 里的 prompt = no
    '-subj', '/CN=localhost',
  ],
  { encoding: 'utf8' }
);
if (openssl.status !== 0) fail(`openssl 生成证书失败：${openssl.stderr || openssl.stdout}`);

// ---------- 装进信任库（各平台命令不同；失败不拦事，手动命令照打） ----------
function trustCert() {
  const out = [];
  if (process.platform === 'win32') {
    const cer = path.join(certDir, 'cert.cer');
    fs.copyFileSync(certPath, cer);
    // -user 写当前用户的 Root 存储，不需要管理员；Schannel 链构建会查它。
    // timeout 是必须的：某些环境（CI、沙箱）里 certutil 会挂住不返回，
    // 没有超时整个脚本就卡死在这儿。挂住/失败都落进「手动命令」分支，不拦事。
    const r = spawnSync('certutil', ['-addstore', '-user', 'Root', cer], {
      encoding: 'utf8',
      timeout: 10000,
      killSignal: 'SIGTERM',
    });
    if (r.status !== 0) {
      out.push('  certutil -addstore Root ' + cer + '   # 需要管理员');
    }
  } else if (process.platform === 'darwin') {
    const r = spawnSync(
      'security',
      ['add-trusted-cert', '-d', '-r', 'trustRoot', '-k', '/Library/Keychains/System.keychain', certPath],
      { encoding: 'utf8', timeout: 10000, killSignal: 'SIGTERM' }
    );
    if (r.status !== 0) {
      out.push(`  sudo security add-trusted-cert -d -r trustRoot -k /Library/Keychains/System.keychain ${certPath}`);
    }
  } else {
    const r = spawnSync('sh', ['-c', `cp '${certPath}' /usr/local/share/ca-certificates/fake-update.crt && update-ca-certificates`], { encoding: 'utf8', timeout: 30000, killSignal: 'SIGTERM' });
    if (r.status !== 0) {
      out.push(`  sudo cp ${certPath} /usr/local/share/ca-certificates/fake-update.crt && sudo update-ca-certificates`);
    }
  }
  return out;
}
const manual = trustCert();

// ---------- 静态服务 ----------
// 路径穿越清洗：拼完之后必须还在 root 里。假源只服务本机，但目录路径不可信
// 的规矩不分线上线下。
function resolveSafe(urlPath) {
  const decoded = decodeURIComponent(urlPath.split('?')[0]);
  const p = path.normalize(path.join(root, decoded));
  return p.startsWith(root + path.sep) || p === root ? p : null;
}
const MIME = { '.json': 'application/json', '.exe': 'application/octet-stream', '.zip': 'application/zip' };
const server = https.createServer(
  { key: fs.readFileSync(keyPath), cert: fs.readFileSync(certPath) },
  (req, res) => {
    const file = resolveSafe(req.url);
    if (!file || !fs.existsSync(file) || !fs.statSync(file).isFile()) {
      res.writeHead(404).end('not found');
      return;
    }
    const body = fs.readFileSync(file);
    res.writeHead(200, {
      'Content-Type': MIME[path.extname(file)] || 'application/octet-stream',
      'Content-Length': body.length,
    });
    res.end(body);
  }
);
server.on('error', (e) => {
  if (e.code === 'EADDRINUSE') fail(`端口 ${port} 被占用了，换一个：--port`);
  fail(e.message);
});
server.listen(port, '127.0.0.1', () => {
  console.log(`假更新源已启动：https://127.0.0.1:${port}/（服务目录 ${root}）`);
  console.log(`证书：${certDir}${keepCert ? '（--keep-cert，退出时保留）' : '（退出时删除）'}`);
  if (manual.length) {
    console.log('\n自动装入信任库没成功（缺权限？），手动执行其一：');
    for (const m of manual) console.log(m);
  } else {
    console.log('证书已装入系统信任库');
  }
  console.log('\n联调（PowerShell）：');
  console.log(`  $env:INPUTSWITCH_UPDATE_URL = "https://127.0.0.1:${port}/updater.json"`);
  console.log('  target/debug/inputswitch-cli update');
  console.log('  target/debug/inputswitch-cli update --download');
  console.log('  （不要 --install：假 exe 会被真的执行）');
  if (verify) selfCheck();
});

function selfCheck() {
  const req = https.request(
    { host: '127.0.0.1', port, path: '/updater.json', rejectUnauthorized: false },
    (res) => {
      let body = '';
      res.on('data', (c) => (body += c));
      res.on('end', () => {
        console.log(`\n自检 GET /updater.json → ${res.statusCode}`);
        try {
          const m = JSON.parse(body);
          console.log(`  清单 version=${m.version}，平台：${Object.keys(m.platforms || {}).join(', ')}`);
        } catch {
          console.log('  响应不是合法 JSON（updater.json 还没生成？）');
        }
      });
    }
  );
  req.on('error', (e) => console.log(`\n自检失败：${e.message}`));
  req.end();
}

process.on('SIGINT', () => {
  server.close();
  if (!keepCert) fs.rmSync(certDir, { recursive: true, force: true });
  console.log('\n已停止');
  process.exit(0);
});

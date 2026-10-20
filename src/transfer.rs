//! 局域网文件传输（P0）。
//!
//! 一条 TCP 连接传一批文件，复用发现层已经拿到的对端 IP，不需要再配一次地址。
//! 帧格式统一为 `[type: u8][len: u32 大端][payload]`：
//! - type=1 JSON 控制帧（HELLO / DONE）
//! - type=2 裸数据块
//!
//! 完整性靠 SHA-256：发端先算一遍写进 HELLO，收端边收边算，对不上就删掉不落地。
//! 目录会递归展开并保留相对路径 —— 但相对路径必须过 `safe_rel`，
//! 否则对端可以塞一个 `../../.ssh/authorized_keys` 进来。

use std::io::{Read, Write};
use std::net::{IpAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::Digest;

use crate::pair;

pub const CHUNK: usize = 256 * 1024;
const FRAME_JSON: u8 = 1;
const FRAME_DATA: u8 = 2;
const MAX_JSON: u32 = 8 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileMeta {
    /// 相对路径，用 `/` 分隔
    pub rel: String,
    pub size: u64,
    pub hash: String,
}

#[derive(Serialize, Deserialize)]
struct Hello {
    v: u32,
    node: String,
    ts: u64,
    nonce: u64,
    mac: String,
    files: Vec<FileMeta>,
    /// 这一批是"剪贴板按需传输"（用户在对面按了 Ctrl+V），不是面板手动发文件。
    /// 收端据此决定要不要把落盘路径写进剪贴板。
    /// **不进 canonical**：老端不认识这个字段，签名照样对得上。
    #[serde(default)]
    clip: bool,
}

impl Hello {
    fn canonical(&self) -> String {
        let files = serde_json::to_string(&self.files).unwrap_or_default();
        format!("hello|{}|{}|{}|{}", self.node, self.ts, self.nonce, files)
    }
}

#[derive(Serialize, Deserialize)]
struct Done {
    ok: bool,
    message: String,
    saved: Vec<String>,
}

/// 把用户选的路径展开成待传文件清单。目录会递归，保留相对路径。
pub fn collect(paths: &[PathBuf]) -> Result<(Vec<PathBuf>, Vec<FileMeta>), String> {
    let mut local = Vec::new();
    let mut metas = Vec::new();
    for path in paths {
        let path = path.canonicalize().unwrap_or_else(|_| path.clone());
        if path.is_dir() {
            let root_name = path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "folder".to_string());
            let mut stack = vec![(path.clone(), root_name)];
            while let Some((dir, prefix)) = stack.pop() {
                let entries = std::fs::read_dir(&dir)
                    .map_err(|e| format!("读取目录失败 {}：{}", dir.display(), e))?;
                for entry in entries.flatten() {
                    let p = entry.path();
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name.starts_with('.') {
                        continue; // 跳过 .DS_Store 之类的垃圾
                    }
                    let rel = format!("{prefix}/{name}");
                    if p.is_dir() {
                        stack.push((p, rel));
                    } else if p.is_file() {
                        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                        let hash = pair::sha256_file(&p).map_err(|e| e.to_string())?;
                        metas.push(FileMeta { rel, size, hash });
                        local.push(p);
                    }
                }
            }
        } else if path.is_file() {
            let name = path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "file".to_string());
            let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            let hash = pair::sha256_file(&path).map_err(|e| e.to_string())?;
            metas.push(FileMeta {
                rel: name,
                size,
                hash,
            });
            local.push(path);
        }
    }
    if metas.is_empty() {
        return Err("没有可发送的文件".to_string());
    }
    Ok((local, metas))
}

fn write_frame(stream: &mut TcpStream, kind: u8, payload: &[u8]) -> std::io::Result<()> {
    stream.write_all(&[kind])?;
    stream.write_all(&(payload.len() as u32).to_be_bytes())?;
    stream.write_all(payload)?;
    stream.flush()
}

fn read_frame(stream: &mut TcpStream) -> std::io::Result<(u8, Vec<u8>)> {
    let mut head = [0u8; 5];
    stream.read_exact(&mut head)?;
    let len = u32::from_be_bytes([head[1], head[2], head[3], head[4]]);
    if len > MAX_JSON.max(CHUNK as u32 * 4) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "帧过大",
        ));
    }
    let mut payload = vec![0u8; len as usize];
    stream.read_exact(&mut payload)?;
    Ok((head[0], payload))
}

/// 发给某台对端。`progress(sent, total)` 在传输过程中被反复调用。
///
/// `clip = true` 表示这是剪贴板按需传输：收端收完会把落盘路径写进剪贴板。
pub fn send(
    ip: IpAddr,
    port: u16,
    node: &str,
    paths: &[PathBuf],
    key: Option<[u8; 32]>,
    clip: bool,
    mut progress: impl FnMut(u64, u64),
) -> Result<String, String> {
    let (local, metas) = collect(paths)?;
    let total: u64 = metas.iter().map(|m| m.size).sum();

    let addr = std::net::SocketAddr::new(ip, port);
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5))
        .map_err(|e| format!("连不上对端 {}：{}", addr, e))?;
    let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));

    let mut hello = Hello {
        v: 1,
        node: node.to_string(),
        ts: crate::peer::now_secs(),
        nonce: crate::peer::now_secs() ^ (total.wrapping_mul(2654435761)),
        mac: String::new(),
        files: metas.clone(),
        clip,
    };
    if let Some(k) = key {
        hello.mac = pair::sign(&k, &hello.canonical());
    }
    let body = serde_json::to_vec(&hello).map_err(|e| e.to_string())?;
    write_frame(&mut stream, FRAME_JSON, &body).map_err(|e| format!("握手失败：{}", e))?;

    let mut sent = 0u64;
    let mut buf = vec![0u8; CHUNK];
    for path in local.iter() {
        let mut file = std::fs::File::open(path).map_err(|e| format!("打开失败：{}", e))?;
        loop {
            let n = file.read(&mut buf).map_err(|e| format!("读取失败：{}", e))?;
            if n == 0 {
                break;
            }
            write_frame(&mut stream, FRAME_DATA, &buf[..n])
                .map_err(|e| format!("发送中断：{}", e))?;
            sent += n as u64;
            progress(sent, total);
        }
    }

    // 等对端的验收结论
    let (kind, payload) = read_frame(&mut stream).map_err(|e| format!("等回执失败：{}", e))?;
    if kind != FRAME_JSON {
        return Err("对端返回了无法识别的回执".to_string());
    }
    let done: Done = serde_json::from_slice(&payload).map_err(|e| e.to_string())?;
    if done.ok {
        Ok(format!("已发送 {} 个文件（{}）", done.saved.len(), human_size(total)))
    } else {
        Err(done.message)
    }
}

/// 后台接收服务。每来一个连接就处理完再接下一条。
pub fn serve<L, S>(
    inbox: Arc<Mutex<PathBuf>>,
    port: u16,
    key: Option<[u8; 32]>,
    on_log: L,
    on_saved: S,
) -> std::io::Result<()>
where
    L: Fn(String) + Send + Sync + 'static,
    S: Fn(Vec<PathBuf>, bool) + Send + Sync + 'static,
{
    let listener = TcpListener::bind(("0.0.0.0", port))?;
    let on_log = Arc::new(on_log);
    let on_saved = Arc::new(on_saved);
    std::thread::spawn(move || {
        for incoming in listener.incoming() {
            let Ok(mut stream) = incoming else { continue };
            let inbox = inbox.clone();
            let log = on_log.clone();
            let saved_cb = on_saved.clone();
            std::thread::spawn(move || {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(60)));
                // 目录可以在运行时改，每次取最新的
                let dir = inbox.lock().unwrap().clone();
                match receive_one(&mut stream, &dir, key, log.as_ref()) {
                    Ok((saved, clip)) => {
                        let text = format!("接收了 {} 个文件", saved.len());
                        log(text);
                        saved_cb(saved.clone(), clip);
                        let done = Done {
                            ok: true,
                            message: String::new(),
                            saved: saved
                                .iter()
                                .map(|p| p.display().to_string())
                                .collect(),
                        };
                        if let Ok(body) = serde_json::to_vec(&done) {
                            let _ = write_frame(&mut stream, FRAME_JSON, &body);
                        }
                    }
                    Err(err) => {
                        log(format!("接收失败：{}", err));
                        let done = Done {
                            ok: false,
                            message: err,
                            saved: Vec::new(),
                        };
                        if let Ok(body) = serde_json::to_vec(&done) {
                            let _ = write_frame(&mut stream, FRAME_JSON, &body);
                        }
                    }
                }
            });
        }
    });
    Ok(())
}

fn receive_one<L>(
    stream: &mut TcpStream,
    inbox: &Path,
    key: Option<[u8; 32]>,
    log: &L,
) -> Result<(Vec<PathBuf>, bool), String>
where
    L: Fn(String) + ?Sized,
{
    let (kind, payload) = read_frame(stream).map_err(|e| format!("读握手失败：{}", e))?;
    if kind != FRAME_JSON {
        return Err("对端没说清楚要发什么".to_string());
    }
    let hello: Hello = serde_json::from_slice(&payload).map_err(|e| e.to_string())?;
    if let Some(k) = key {
        if !pair::verify(&k, &hello.canonical(), &hello.mac) {
            return Err("配对码不一致，已拒绝".to_string());
        }
    }
    if hello.files.is_empty() {
        return Err("对方没有文件要发".to_string());
    }
    log(format!(
        "{} 要发 {} 个文件（{}）",
        hello.node,
        hello.files.len(),
        human_size(hello.files.iter().map(|f| f.size).sum())
    ));

    std::fs::create_dir_all(inbox).map_err(|e| format!("建接收目录失败：{}", e))?;

    let mut saved = Vec::new();
    for meta in &hello.files {
        let rel = safe_rel(&meta.rel)?;
        let target = unique_path(&inbox.join(&rel));
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        // 先写 .part，校验通过再改名：中途断线不会留下一个看起来正常的坏文件
        let tmp = target.with_extension("part");
        let mut file = std::fs::File::create(&tmp).map_err(|e| format!("建文件失败：{}", e))?;
        let mut left = meta.size;
        let mut hasher = sha2::Sha256::new();
        {
            while left > 0 {
                let (kind, chunk) = read_frame(stream).map_err(|e| format!("接收中断：{}", e))?;
                if kind != FRAME_DATA {
                    return Err("对端发来了意料之外的控制帧".to_string());
                }
                let take = (chunk.len() as u64).min(left);
                file.write_all(&chunk[..take as usize])
                    .map_err(|e| format!("写盘失败：{}", e))?;
                hasher.update(&chunk[..take as usize]);
                left -= take;
            }
            let actual = hasher.finalize();
            let actual_hex: String = actual.iter().map(|b| format!("{b:02x}")).collect();
            if !meta.hash.is_empty() && actual_hex != meta.hash {
                drop(file);
                let _ = std::fs::remove_file(&tmp);
                return Err(format!("{} 校验不通过，已丢弃", meta.rel));
            }
        }
        drop(file);
        std::fs::rename(&tmp, &target).map_err(|e| format!("收尾失败：{}", e))?;
        saved.push(target);
    }
    Ok((saved, hello.clip))
}

/// 拒绝绝对路径和 `..`。
fn safe_rel(rel: &str) -> Result<PathBuf, String> {
    let cleaned = rel.replace('\\', "/");
    if cleaned.starts_with('/') || cleaned.contains(':') {
        return Err("对端给了绝对路径，已拒绝".to_string());
    }
    let mut out = PathBuf::new();
    for part in cleaned.split('/') {
        match part {
            "" | "." => continue,
            ".." => return Err("对端试图写到接收目录之外，已拒绝".to_string()),
            other => out.push(other),
        }
    }
    if out.as_os_str().is_empty() {
        return Err("空文件名".to_string());
    }
    Ok(out)
}

/// 重名就加 (1)(2)，别默默覆盖用户已有的文件。
fn unique_path(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());
    let ext = path
        .extension()
        .map(|s| format!(".{}", s.to_string_lossy()))
        .unwrap_or_default();
    let parent = path.parent().unwrap_or(Path::new("."));
    for i in 1..10_000 {
        let candidate = parent.join(format!("{stem} ({i}){ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    path.to_path_buf()
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[0])
    } else {
        format!("{:.1} {}", value, UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_traversal() {
        assert!(safe_rel("../../etc/passwd").is_err());
        assert!(safe_rel("/etc/passwd").is_err());
        assert!(safe_rel("C:/Windows/win.ini").is_err());
        assert_eq!(safe_rel("a/b.txt").unwrap(), PathBuf::from("a/b.txt"));
        assert_eq!(safe_rel("./a//b.txt").unwrap(), PathBuf::from("a/b.txt"));
    }

    #[test]
    fn roundtrip_over_loopback() {
        let dir = std::env::temp_dir().join(format!("isw-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("hello.txt");
        std::fs::write(&src, b"inputswitch transfer test").unwrap();

        let inbox = dir.join("inbox");
        let port = 49123;
        serve(
            Arc::new(Mutex::new(inbox.clone())),
            port,
            None,
            |_| {},
            |_, _| {},
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(150));

        let node = "test-node";
        let result = send(
            IpAddr::from([127, 0, 0, 1]),
            port,
            node,
            &[src.clone()],
            None,
            false,
            |_, _| {},
        );
        assert!(result.is_ok(), "{:?}", result);
        let saved = inbox.join("hello.txt");
        assert!(saved.exists());
        assert_eq!(std::fs::read(&saved).unwrap(), b"inputswitch transfer test");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

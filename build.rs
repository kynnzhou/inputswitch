//! 两件事：
//! ① Windows 的 InputSwitch.exe 内嵌图标资源。
//!
//! 之前这个 exe 根本没有图标：资源管理器、任务栏、Alt+Tab 里都是通用的空白页。
//! 补图标时发现本机没有 rc.exe / windres / llvm-rc（交叉编译走 Zig，
//! 而 Zig 自带的 `zig rc` 在这台机器上链接失败），所以这里直接写 COFF
//! 资源对象，不依赖任何外部资源编译器 —— 换一台没装 Windows SDK 的机器
//! 也能编出带图标的 exe。
//!
//! 只有托盘那个 bin 需要资源：`cargo:rustc-link-arg-bin` 只对指定 bin 生效，
//! 诊断 CLI 不会为了一个图标多背几十 KB。
//!
//! ② 生成前端产物清单（panel_dist.rs，全平台）：扫描 assets/panel-dist/
//! （tools/copy-panel-dist.mjs 从 desktop/dist 拷来的 Vite 构建件），把每个
//! 文件变成 `include_bytes!` 表项，panel.rs 据此把 React 应用同源托管在 /app/。
//! 目录为空/不存在时生成空表 —— 没跑 npm 的协作者 `cargo build` 照样过，
//! 只是 /app/ 返回一句「先构建前端」的提示页（旧面板 / 不受影响）。

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=assets/AppIcon.ico");
    println!("cargo:rerun-if-changed=build.rs");
    emit_panel_dist();

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let (machine, reloc) = match arch.as_str() {
        "x86_64" => (0x8664u16, IMAGE_REL_AMD64_ADDR32NB),
        "x86" => (0x014cu16, IMAGE_REL_I386_DIR32NB),
        "aarch64" => (0xaa64u16, IMAGE_REL_ARM64_ADDR32NB),
        other => panic!("不认识的 Windows 架构：{other}"),
    };

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let obj = out_dir.join("inputswitch-res.o");

    let ico = std::fs::read("assets/AppIcon.ico").expect("读不到 assets/AppIcon.ico");
    let bytes = coff_with_icon(&ico, machine, reloc).expect("生成资源对象失败");
    std::fs::write(&obj, bytes).expect("写不了资源对象");

    // 裸目标文件，用 link-arg 传；link-lib 只认库。
    println!("cargo:rustc-link-arg-bin=InputSwitch={}", obj.display());
}

const IMAGE_REL_AMD64_ADDR32NB: u16 = 0x0003;
const IMAGE_REL_I386_DIR32NB: u16 = 0x0006;
const IMAGE_REL_ARM64_ADDR32NB: u16 = 0x000b;

const IMAGE_SYM_CLASS_STATIC: u8 = 3;
const SECTION_ALIGN_4BYTES: u8 = 2;
const RT_ICON: u32 = 3;
const RT_GROUP_ICON: u32 = 14;
const LANG_NEUTRAL: u32 = 0;

/// 一张图标图：目录里的四个描述字节 + 图像数据。
struct Image<'a> {
    w: u8,
    h: u8,
    colors: u8,
    reserved: u8,
    data: &'a [u8],
}

fn parse_ico(ico: &[u8]) -> Result<Vec<Image<'_>>, String> {
    if ico.len() < 6 {
        return Err("ico 太短".into());
    }
    let count = u16::from_le_bytes([ico[4], ico[5]]) as usize;
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let off = 6 + 16 * i;
        if off + 16 > ico.len() {
            return Err("ico 目录越界".into());
        }
        let e = &ico[off..off + 16];
        let size = u32::from_le_bytes([e[8], e[9], e[10], e[11]]) as usize;
        let at = u32::from_le_bytes([e[12], e[13], e[14], e[15]]) as usize;
        if at + size > ico.len() {
            return Err("ico 图像越界".into());
        }
        out.push(Image {
            w: e[0],
            h: e[1],
            colors: e[2],
            reserved: e[3],
            data: &ico[at..at + size],
        });
    }
    if out.is_empty() {
        return Err("ico 里没有图".into());
    }
    Ok(out)
}

fn put16(s: &mut [u8], at: usize, v: u16) {
    s[at..at + 2].copy_from_slice(&v.to_le_bytes());
}

fn put32(s: &mut [u8], at: usize, v: u32) {
    s[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

/// 把 .ico 包成一个只有 `.rsrc` 段的 COFF 目标文件。
///
/// 资源目录里的数据指针最终必须是映像里的 RVA，所以每个数据项都要一条
/// `*_ADDR32NB` 重定位指向 `.rsrc` 段符号，由链接器填 —— 和 cvtres 的做法一样。
/// 漏了重定位，链接不会报错，但图标指针会停在文件头，资源管理器直接不显示图标。
fn coff_with_icon(ico: &[u8], machine: u16, reloc_type: u16) -> Result<Vec<u8>, String> {
    let images = parse_ico(ico)?;

    // RT_GROUP_ICON 的内容：目录项 + 各图编号
    let mut group = Vec::new();
    group.extend_from_slice(&0u16.to_le_bytes()); // reserved
    group.extend_from_slice(&1u16.to_le_bytes()); // type = icon
    group.extend_from_slice(&(images.len() as u16).to_le_bytes());
    for (i, im) in images.iter().enumerate() {
        group.push(im.w);
        group.push(im.h);
        group.push(im.colors);
        group.push(im.reserved);
        group.extend_from_slice(&1u16.to_le_bytes()); // planes
        group.extend_from_slice(&32u16.to_le_bytes()); // bit count
        group.extend_from_slice(&(im.data.len() as u32).to_le_bytes());
        group.extend_from_slice(&((i + 1) as u16).to_le_bytes());
    }

    // 叶子：(类型, 名字, 数据)。图标组固定编 1 号 ——
    // 资源管理器、任务栏都按最小的编号取主图标。
    let mut leaves: Vec<(u32, u32, Vec<u8>)> = Vec::with_capacity(images.len() + 1);
    for (i, im) in images.iter().enumerate() {
        leaves.push((RT_ICON, (i + 1) as u32, im.data.to_vec()));
    }
    leaves.push((RT_GROUP_ICON, 1, group));

    let mut types: Vec<u32> = leaves.iter().map(|l| l.0).collect();
    types.sort_unstable();
    types.dedup();

    let count_of = |t: u32| leaves.iter().filter(|l| l.0 == t).count();

    // ── 目录树 ───────────────────────────────────────────────────
    // root → 类型目录 → 名字目录 → 语言项 → 数据项，最后是数据本体。
    // 这里每个名字只挂一个语言（中性语言），所以名字目录固定 16+8 字节。
    let root_at = 0usize;
    let mut cur = 16 + 8 * types.len();
    let mut type_at = Vec::with_capacity(types.len());
    for t in &types {
        type_at.push(cur);
        cur += 16 + 8 * count_of(*t);
    }
    // 名字目录按"类型顺序、组内原顺序"排，记下每个叶子对应哪个目录
    let mut ordered: Vec<usize> = Vec::with_capacity(leaves.len());
    for t in &types {
        for (i, l) in leaves.iter().enumerate() {
            if l.0 == *t {
                ordered.push(i);
            }
        }
    }
    let mut name_at = Vec::with_capacity(leaves.len());
    for _ in &ordered {
        name_at.push(cur);
        cur += 16 + 8;
    }
    let data_entry_at = cur;
    cur += 16 * leaves.len();

    // 数据本体，4 字节对齐
    let mut blob_at = vec![0usize; leaves.len()];
    for i in &ordered {
        blob_at[*i] = cur;
        cur += leaves[*i].2.len();
        while !cur.is_multiple_of(4) {
            cur += 1;
        }
    }
    let section_size = cur;
    let mut s = vec![0u8; section_size];

    // root
    put16(&mut s, root_at + 12, 0);
    put16(&mut s, root_at + 14, types.len() as u16);
    for (ti, t) in types.iter().enumerate() {
        put32(&mut s, root_at + 16 + 8 * ti, *t);
        put32(&mut s, root_at + 16 + 8 * ti + 4, 0x8000_0000 | type_at[ti] as u32);
    }

    // 类型目录 → 名字目录
    for (ti, t) in types.iter().enumerate() {
        let a = type_at[ti];
        put16(&mut s, a + 12, 0);
        put16(&mut s, a + 14, count_of(*t) as u16);
        let mut slot = 0usize;
        for (pos, leaf) in ordered.iter().enumerate() {
            let l = &leaves[*leaf];
            if l.0 != *t {
                continue;
            }
            put32(&mut s, a + 16 + 8 * slot, l.1);
            put32(&mut s, a + 16 + 8 * slot + 4, 0x8000_0000 | name_at[pos] as u32);
            slot += 1;
        }
    }

    // 名字目录 → 语言项 → 数据项
    let mut relocs = Vec::with_capacity(leaves.len());
    for (pos, leaf) in ordered.iter().enumerate() {
        let at = name_at[pos];
        let de = data_entry_at + 16 * *leaf;
        put16(&mut s, at + 12, 0);
        put16(&mut s, at + 14, 1);
        put32(&mut s, at + 16, LANG_NEUTRAL);
        // 语言这一层直接指向数据项：高位的 1 表示"还是目录"，这里不能置位，
        // 否则系统把数据项当成又一层目录，整个图标树都读不出来。
        put32(&mut s, at + 20, de as u32);
        put32(&mut s, de, blob_at[*leaf] as u32); // 待重定位
        put32(&mut s, de + 4, leaves[*leaf].2.len() as u32);
        put32(&mut s, de + 8, 0); // codepage
        put32(&mut s, de + 12, 0);
        relocs.push(de);
    }

    // 数据本体
    for (i, l) in leaves.iter().enumerate() {
        let at = blob_at[i];
        s[at..at + l.2.len()].copy_from_slice(&l.2);
    }

    // ── COFF 外壳 ────────────────────────────────────────────────
    // 文件头(20) → 段头(40) → 数据 → 重定位 → 符号表 → 字符串表
    let header_end = 20 + 40;
    let raw_len = {
        let mut n = section_size;
        while !(header_end + n).is_multiple_of(4) {
            n += 1;
        }
        n
    };
    let reloc_at = header_end + raw_len;
    let sym_at = {
        let mut n = reloc_at + 10 * relocs.len();
        while n % 4 != 0 {
            n += 1;
        }
        n
    };

    let mut out = vec![0u8; header_end];
    put16(&mut out, 0, machine);
    put16(&mut out, 2, 1); // NumberOfSections
    put32(&mut out, 4, 0); // TimeDateStamp：固定 0，可复现构建
    put32(&mut out, 8, sym_at as u32);
    put32(&mut out, 12, 2); // NumberOfSymbols：段符号 + 一条辅助记录
    put16(&mut out, 16, 0); // SizeOfOptionalHeader
    put16(&mut out, 18, 0); // Characteristics

    out[20..25].copy_from_slice(b".rsrc");
    put32(&mut out, 28, section_size as u32); // VirtualSize
    put32(&mut out, 32, 0); // VirtualAddress
    put32(&mut out, 36, raw_len as u32); // SizeOfRawData
    put32(&mut out, 40, header_end as u32); // PointerToRawData
    put32(&mut out, 44, reloc_at as u32); // PointerToRelocations
    put32(&mut out, 48, 0); // PointerToLinenumbers
    put16(&mut out, 52, relocs.len() as u16);
    put16(&mut out, 54, 0); // NumberOfLinenumbers
    // IMAGE_SCN_CNT_INITIALIZED_DATA | IMAGE_SCN_MEM_READ
    put32(&mut out, 56, 0x4000_0040);

    out.extend_from_slice(&s);
    out.resize(reloc_at, 0);

    for de in &relocs {
        out.extend_from_slice(&(*de as u32).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // 符号表索引 0 = .rsrc 段符号
        out.extend_from_slice(&reloc_type.to_le_bytes());
    }
    out.resize(sym_at, 0);

    // 段符号 + 辅助记录
    out.extend_from_slice(b".rsrc\0\0\0");
    out.extend_from_slice(&0u32.to_le_bytes()); // Value
    out.extend_from_slice(&1i16.to_le_bytes()); // SectionNumber
    out.extend_from_slice(&0u16.to_le_bytes()); // Type
    out.push(IMAGE_SYM_CLASS_STATIC);
    out.push(1); // NumberOfAuxSymbols

    out.extend_from_slice(&(section_size as u32).to_le_bytes()); // Length
    out.extend_from_slice(&(relocs.len() as u16).to_le_bytes()); // NumberOfRelocations
    out.extend_from_slice(&0u16.to_le_bytes()); // NumberOfLinenumbers
    out.extend_from_slice(&0u32.to_le_bytes()); // CheckSum
    out.extend_from_slice(&0i16.to_le_bytes()); // Number（关联段号）
    out.push(SECTION_ALIGN_4BYTES);
    out.extend_from_slice(&[0u8; 3]);

    // 字符串表：空表也有一个长度字
    out.extend_from_slice(&4u32.to_le_bytes());

    Ok(out)
}

/// 生成 `$OUT_DIR/panel_dist.rs`：`(路径, MIME, 字节)` 静态表。
/// 路径是 URL 形（`index.html`、`assets/index-Abc.js`），panel.rs 按 `/app/<路径>` 查。
/// `include_bytes!` 只认字面量路径，所以这里把绝对路径直接写进生成代码。
fn emit_panel_dist() {
    let dist = PathBuf::from("assets/panel-dist");
    println!("cargo:rerun-if-changed=assets/panel-dist");

    let mut entries: Vec<(String, PathBuf)> = Vec::new();
    if dist.is_dir() {
        // Vite 产物只有两层：根的 index.html + assets/ 下的哈希文件。
        // 真出现更深嵌套时直接递归，别静默漏文件。
        fn walk(dir: &std::path::Path, prefix: &str, out: &mut Vec<(String, PathBuf)>) {
            let Ok(iter) = std::fs::read_dir(dir) else { return };
            for e in iter.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if e.path().is_dir() {
                    walk(&e.path(), &format!("{prefix}{name}/"), out);
                } else {
                    out.push((format!("{prefix}{name}"), e.path()));
                }
            }
        }
        walk(&dist, "", &mut entries);
    }
    entries.sort();

    let mut code = String::from(
        "// 由 build.rs 生成，别手改。\n\
         // panel_dist：前端构建产物的内嵌表（URL 路径, MIME, 字节）。\n\
         pub static PANEL_DIST: &[(&str, &str, &[u8])] = &[\n",
    );
    for (url, path) in &entries {
        let abs = std::fs::canonicalize(path)
            .expect("canonicalize panel-dist 文件")
            .display()
            .to_string();
        code.push_str(&format!(
            "    ({url:?}, {:?}, include_bytes!({abs:?})),\n",
            mime_of(url)
        ));
    }
    code.push_str("];\n");

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    std::fs::write(out_dir.join("panel_dist.rs"), code).expect("写 panel_dist.rs");
}

/// 内嵌表用的极简 MIME：Vite 产物就这几种；不认识的按二进制给，
/// 浏览器对未知类型会下载而不是执行 —— 比误判成可执行的更安全。
fn mime_of(url: &str) -> &'static str {
    match url.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "json" => "application/json; charset=utf-8",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

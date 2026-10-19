#!/usr/bin/env bash
# 在 macOS 上直接交叉编译出 Windows 版（不需要 Windows 机器）
set -euo pipefail
cd "$(dirname "$0")"

TC="$PWD/.toolchain"
ZIG="$TC/zig-aarch64-macos-0.15.2/zig"
DIST="dist/windows"

if [ ! -x "$ZIG" ]; then
  echo "先运行 tools/setup-zig.sh 下载 Zig" >&2
  exit 1
fi

# dlltool / ar 的替身：rustc 会按 <target>-dlltool 的名字去 PATH 里找
mkdir -p "$TC/bin"
printf '#!/bin/sh\nexec "%s" dlltool "$@"\n' "$ZIG" > "$TC/bin/x86_64-w64-mingw32-dlltool"
printf '#!/bin/sh\nexec "%s" ar "$@"\n' "$ZIG" > "$TC/bin/x86_64-w64-mingw32-ar"
chmod +x "$TC/bin/"*
chmod +x tools/zigcc

echo "==> 交叉编译 x86_64-pc-windows-gnu"

cargo_win() {
  PATH="$TC/bin:$PATH" \
  CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER="$PWD/tools/zigcc" \
  CARGO_TARGET_X86_64_PC_WINDOWS_GNU_AR="$TC/bin/x86_64-w64-mingw32-ar" \
  cargo "$@"
}

# 1) 诊断用 CLI：不带 tray 特性 → 不链接 wry / WebView2，产物零外部依赖，
#    单个 exe 拷到任何 Windows 机器上都能直接跑。
cargo_win build --release --target x86_64-pc-windows-gnu \
  --bin inputswitch-cli --no-default-features

# 2) 托盘本体：带 tray 特性 → 控制面板是 WebView2 窗口
cargo_win build --release --target x86_64-pc-windows-gnu --bin InputSwitch

OUT="target/x86_64-pc-windows-gnu/release"
mkdir -p "$DIST"
cp "$OUT/InputSwitch.exe" "$DIST/"
cp "$OUT/inputswitch-cli.exe" "$DIST/"

# 关键：GNU 目标下 wry 链接的是 WebView2Loader 的导入库（webview2-com-sys 里
# 静态库只在 target_env = "msvc" 时才用），所以这个 DLL 必须在 exe 旁边，
# 否则双击直接弹「找不到 WebView2Loader.dll，无法继续执行代码」。
WEBVIEW2_DLL="tools/webview2/x64/WebView2Loader.dll"
if [ -f "$WEBVIEW2_DLL" ]; then
  cp "$WEBVIEW2_DLL" "$DIST/"
else
  echo "警告：找不到 $WEBVIEW2_DLL，打出来的 exe 在 Windows 上会启动失败" >&2
fi

echo
echo "完成: $(pwd)/$DIST"
ls -la "$DIST"

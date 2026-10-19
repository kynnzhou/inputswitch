#!/usr/bin/env bash
# 下载 Zig（自带 mingw-w64 工具链），用于在 macOS 上交叉编译 Windows 版。
# 只在想从 mac 出 exe 时需要；在 Windows 上直接 .\build.ps1 就行。
set -euo pipefail
cd "$(dirname "$0")/.."

VER="${ZIG_VER:-0.15.2}"
ARCH="$(uname -m)"
case "$ARCH" in
  arm64) ZARCH="aarch64" ;;
  x86_64) ZARCH="x86_64" ;;
  *) echo "不支持的架构: $ARCH" >&2; exit 1 ;;
esac

DIR=".toolchain/zig-${ZARCH}-macos-${VER}"
if [ -x "$DIR/zig" ]; then
  echo "已存在: $DIR/zig ($("$DIR/zig" version))"
  exit 0
fi

mkdir -p .toolchain
URL="https://ziglang.org/download/${VER}/zig-${ZARCH}-macos-${VER}.tar.xz"
echo "下载 $URL"
curl -# -L -o .toolchain/zig.tar.xz "$URL"

echo "解压"
tar -C .toolchain -xf .toolchain/zig.tar.xz
rm -f .toolchain/zig.tar.xz

if [ ! -x "$DIR/zig" ]; then
  echo "解压后找不到 $DIR/zig" >&2
  ls .toolchain >&2
  exit 1
fi
echo "完成: $("$DIR/zig" version)"

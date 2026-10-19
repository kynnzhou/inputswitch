#!/usr/bin/env bash
# 构建 macOS .app（在本机执行）
set -euo pipefail
cd "$(dirname "$0")"

APP_NAME="InputSwitch"
BUNDLE="dist/${APP_NAME}.app"
GUI_BIN="target/release/${APP_NAME}"

echo "==> 编译（release）"
cargo build --release --bins

if [ ! -f "$GUI_BIN" ]; then
  echo "找不到 $GUI_BIN，编译可能失败了" >&2
  exit 1
fi

echo "==> 组装 ${BUNDLE}"
rm -rf "$BUNDLE"
mkdir -p "${BUNDLE}/Contents/MacOS" "${BUNDLE}/Contents/Resources"

cp "$GUI_BIN" "${BUNDLE}/Contents/MacOS/${APP_NAME}"
chmod +x "${BUNDLE}/Contents/MacOS/${APP_NAME}"
cp assets/AppIcon.icns "${BUNDLE}/Contents/Resources/AppIcon.icns"

# 命令行工具一并放进去，方便在终端里跑 probe / selftest
# 注意名字不能和 InputSwitch 只差大小写：macOS 文件系统不区分大小写，会互相覆盖
cp target/release/inputswitch-cli "${BUNDLE}/Contents/MacOS/inputswitch-cli"

cat > "${BUNDLE}/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>InputSwitch</string>
    <key>CFBundleDisplayName</key><string>InputSwitch</string>
    <key>CFBundleExecutable</key><string>InputSwitch</string>
    <key>CFBundleIdentifier</key><string>cn.zhouke.inputswitch</string>
    <key>CFBundleVersion</key><string>0.1.0</string>
    <key>CFBundleShortVersionString</key><string>0.1.0</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleIconFile</key><string>AppIcon</string>
    <key>LSUIElement</key><true/>
    <key>LSMinimumSystemVersion</key><string>11.0</string>
    <key>NSHighResolutionCapable</key><true/>
    <!-- 托盘应用没有可见窗口，几分钟无交互就会被 macOS App Nap 挂起节电：
         进程被挂起时事件钩子停摆、系统按超时把钩子禁用 → 键鼠漏到本机
        （体感：一切正常一两分钟后开始漏）。彻底退出 App Nap。 -->
    <key>NSAppSleepDisabled</key><true/>
</dict>
</plist>
PLIST

echo "==> 临时签名（ad-hoc）"
codesign --force --deep --sign - "$BUNDLE" 2>/dev/null || echo "   跳过签名（未安装 Xcode 命令行工具时正常）"

echo
echo "完成: $(pwd)/${BUNDLE}"
echo "双击运行，或：open ${BUNDLE}"
echo "命令行诊断： ${BUNDLE}/Contents/MacOS/inputswitch-cli probe"

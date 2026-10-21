#!/usr/bin/env bash
# 构建 macOS .app（在本机执行）
set -euo pipefail
cd "$(dirname "$0")"

APP_NAME="InputSwitch"
BUNDLE="dist/${APP_NAME}.app"
GUI_BIN="target/release/${APP_NAME}"
INSTALL_DIR="/Applications"

# 版本号只有一个来源：托盘启动日志里那句「构建=vX.Y.Z」。
# 以前 Info.plist 固定写 0.1.0，用户查版本只能看到 0.1.0，误以为装的还是老版本。
VERSION="$(sed -n 's/.*构建=v\([0-9][0-9.]*\).*/\1/p' src/tray.rs | head -1)"
if [ -z "$VERSION" ]; then
  echo "读不到版本号（src/tray.rs 里应该有一句 构建=vX.Y.Z）" >&2
  exit 1
fi

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

echo "==> 版本 ${VERSION}"
/usr/libexec/PlistBuddy \
  -c "Set :CFBundleShortVersionString ${VERSION}" \
  -c "Set :CFBundleVersion ${VERSION}" \
  "${BUNDLE}/Contents/Info.plist"

# 签名：默认 ad-hoc。
#
# 试过用固定本地自签名证书（2026-10-20）：DR 确实变成
# `identifier + 证书根哈希`、不再随编译变化，但**授权勾不上去** ——
# 在「设备控制和数据访问」里勾了之后，系统 TCC 库里根本不写这一行
# （/Library/Application Support/com.apple.TCC/TCC.db 里查不到
# cn.zhouke.inputswitch），于是点「开启」永远弹回设置页。
# find-identity 显示有效、codesign --verify 也过，但 tccd 就是不认。
# 结论：macOS 27 的辅助功能授权只接受 ad-hoc 或 Apple 签发的身份。
#
# 想再试别的证书时用环境变量：ISW_SIGN_ID="证书名" ./build.sh
SIGN_ID="${ISW_SIGN_ID:-}"

echo "==> 签名"
if [ -n "$SIGN_ID" ] && security find-identity -v -p codesigning 2>/dev/null | grep -qF "$SIGN_ID"; then
  # 嵌套二进制要逐个签，最后签外层。用 --deep 一次性签会留下
  # .cstemp 残影，`codesign --verify` 报 "a sealed resource is missing or invalid"。
  codesign --force --sign "$SIGN_ID" "${BUNDLE}/Contents/MacOS/inputswitch-cli"
  codesign --force --sign "$SIGN_ID" "${BUNDLE}/Contents/MacOS/${APP_NAME}"
  codesign --force --sign "$SIGN_ID" "$BUNDLE"
  codesign --verify --deep --strict "$BUNDLE" || { echo "签名校验失败" >&2; exit 1; }
  echo "   ${SIGN_ID}"
else
  codesign --force --sign - "${BUNDLE}/Contents/MacOS/inputswitch-cli" 2>/dev/null \
    && codesign --force --sign - "${BUNDLE}/Contents/MacOS/${APP_NAME}" 2>/dev/null \
    && codesign --force --sign - "$BUNDLE" 2>/dev/null \
    || echo "   跳过签名（未安装 Xcode 命令行工具时正常）"
  echo "   ad-hoc"
fi

# 装进 /Applications，并保证系统里只注册这一份。
# 不这么做的话 dist/ 里那份、以前留在废纸篓里的那份都会以同名同包名出现在
# 「设置 → 隐私与安全性」的添加列表里，用户很可能勾到废弃副本，
# 于是勾了没反应、点开启又弹回来（2026-10-20 实际踩过）。
LSR=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister
if [ -d "${INSTALL_DIR}/${APP_NAME}.app" ]; then
  # 覆盖前必须让旧进程退出：运行中替换二进制 → 下次启动 SIGKILL(Code Signature Invalid)
  pkill -f "${INSTALL_DIR}/${APP_NAME}.app/Contents/MacOS/${APP_NAME}" 2>/dev/null || true
  sleep 1
fi
rm -rf "${INSTALL_DIR}/${APP_NAME}.app"
cp -R "$BUNDLE" "${INSTALL_DIR}/${APP_NAME}.app"

"$LSR" -u "$(pwd)/${BUNDLE}" >/dev/null 2>&1 || true
"$LSR" -f "${INSTALL_DIR}/${APP_NAME}.app" >/dev/null 2>&1 || true
# 最后再注销一次 dist 里那份：copy 动作本身会把它重新注册回来。
# 废纸篓里那份也要注销 —— 文件还在，系统重新扫描就会再注册回来，
# 于是设置页的添加列表里又冒出第二个同名 InputSwitch（不删文件，只注销注册）。
"$LSR" -u "$(pwd)/${BUNDLE}" >/dev/null 2>&1 || true
"$LSR" -u "$HOME/.Trash/${APP_NAME}.app" >/dev/null 2>&1 || true

# 打 macOS 分发 zip —— **从 /Applications 那份打**，不要从 dist 打。
#
# 2026-10-21 实测：手工 `ditto` 去读 dist/InputSwitch.app 打 zip，这个"读"
# 动作本身会把它重新注册给 LaunchServices，Launchpad 里于是出现 4 个同名
# InputSwitch（磁盘上只有 3 份）。打完就把 dist 那份删掉，从根上断掉这个副本。
REPO=$(pwd)
mkdir -p "${REPO}/dist"
rm -f "${REPO}/dist/InputSwitch-macOS-arm64.zip"
(
  cd "${INSTALL_DIR}" && ditto -c -k --sequesterRsrc --keepParent \
    "${APP_NAME}.app" "${REPO}/dist/InputSwitch-macOS-arm64.zip"
) && echo "分发包: ${REPO}/dist/InputSwitch-macOS-arm64.zip"
rm -rf "$BUNDLE"
"$LSR" -u "${REPO}/${BUNDLE}" >/dev/null 2>&1 || true

echo
echo "完成: ${INSTALL_DIR}/${APP_NAME}.app  (v${VERSION})"
echo "已在 /Applications，双击或：open -a ${APP_NAME}"
echo "命令行诊断： ${INSTALL_DIR}/${APP_NAME}.app/Contents/MacOS/inputswitch-cli probe"
echo
echo "权限提醒（顺序很重要）："
echo "  1) ad-hoc 签名每次重编译 cdhash 都变 → 装完要去「设置 → 隐私与安全性 →"
echo "     设备控制和数据访问」把 ${APP_NAME} 取消勾选再重新勾上"
echo "  2) **先勾再启动**。已经跑起来的进程会缓存旧的 TCC 结论，勾了也照样报"
echo "     「没权限」，点开启只会再弹一次设置页 —— 看起来像勾选无效。"
echo "     已经在跑的话：托盘菜单「⚠ 系统授权没生效 · 点这里重启应用」，"
echo "     或 quit 掉再 open。"
echo "  添加列表里只有 /Applications 这一份。"

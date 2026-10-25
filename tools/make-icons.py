#!/usr/bin/env python3
"""从 design/brand/*.svg 生成各平台图标。

    python3 tools/make-icons.py

产出（都提交进仓库，构建时不需要跑这个脚本）：
    assets/AppIcon-1024.png        应用图标
    assets/AppIcon.icns            macOS 装包
    assets/AppIcon.ico             16–256 七个尺寸，Windows exe 内嵌
    assets/menubar.png             22×22 菜单栏模板图（macOS）
    assets/menubar@2x.png          44×44 同上，Retina
    assets/tray-win-32.png         Windows 通知区域彩色小图

为什么要用 Chrome 渲染：本机没有 rsvg / inkscape / cairosvg，Pillow 也不能读 SVG。
每个尺寸都按原生像素渲染（不是把 1024 缩下来），16px 才不至于糊成一团。
"""
import os
import shutil
import struct
import subprocess
import sys
import tempfile

CHROME = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BRAND = os.path.join(ROOT, "design", "brand")
ASSETS = os.path.join(ROOT, "assets")


def render(svg_path, png_path, width, height, scale=1, post=""):
    """用 headless Chrome 把一张 SVG 渲染成 PNG，背景透明。

    post 是对 SVG 源码的再加工（比如丢阴影、改底板占比），用来出小尺寸变体 ——
    见 render_app。
    """
    with open(svg_path, encoding="utf-8") as f:
        svg = f.read()
    svg = svg.replace('width="1024"', f'width="{width}"', 1)
    svg = svg.replace('height="1024"', f'height="{height}"', 1)
    svg = svg.replace('width="44"', f'width="{width}"', 1)
    svg = svg.replace('height="44"', f'height="{height}"', 1)
    if post:
        svg = post(svg)
    html = (
        "<!DOCTYPE html><html><head><meta charset=utf-8><style>"
        "html,body{margin:0;padding:0;background:transparent}svg{display:block}"
        "</style></head><body>" + svg + "</body></html>"
    )
    tmp_dir = tempfile.mkdtemp(prefix="isw-icon-")
    tmp = os.path.join(tmp_dir, "page.html")
    with open(tmp, "w", encoding="utf-8") as f:
        f.write(html)
    try:
        subprocess.run(
            [
                CHROME,
                "--headless=new",
                "--disable-gpu",
                "--hide-scrollbars",
                "--default-background-color=00000000",
                f"--force-device-scale-factor={scale}",
                f"--window-size={width},{height}",
                f"--screenshot={png_path}",
                "file://" + tmp,
            ],
            check=True,
            capture_output=True,
        )
    finally:
        shutil.rmtree(tmp_dir, ignore_errors=True)
    if not os.path.exists(png_path):
        sys.exit(f"渲染失败：{png_path}")


def write_ico(pngs, out):
    """PNG 压缩的 ICO。Windows Vista 起支持，256 那张也只能是 PNG。"""
    blobs = [open(p, "rb").read() for p in pngs]
    header = struct.pack("<HHH", 0, 1, len(blobs))
    offset = 6 + 16 * len(blobs)
    entries = b""
    data = b""
    for p, blob in zip(pngs, blobs):
        # 边长从文件名里取，256 在目录里记作 0
        size = int(os.path.basename(p).split("-")[1].split(".")[0])
        entries += struct.pack(
            "<BBBBHHII", size % 256, size % 256, 0, 0, 1, 32, len(blob), offset
        )
        data += blob
        offset += len(blob)
    with open(out, "wb") as f:
        f.write(header + entries + data)


def write_icns(iconset_dir, out):
    subprocess.run(["iconutil", "-c", "icns", iconset_dir, "-o", out], check=True)


# 底板在画布里的占比。1024 / 256 这些大尺寸按 0.8047 出（等于设计稿里内缩
# 9.77%），和系统图标一致；16–64 放大到 0.875。
#
# 为什么要放大：16px 只有 16 个像素，0.8047 的底板落不到整数边界上，
# 四周一圈全被抗锯齿摊成半透明 —— 图标看上去蒙了一层灰。放大到 0.875
# 之后边缘正好压在像素上，每格都是实心的。30–64 之间按比例插值，
# 避免 32 和 64 之间出现跳变。
def plate_scale(size):
    if size >= 128:
        return 1.0
    if size <= 16:
        return 0.875 / 0.8047
    t = (size - 16) / (128 - 16)
    target = 0.875 + (0.8047 - 0.875) * t
    return target / 0.8047


def small_size_svg(svg, size):
    """小尺寸变体：去掉投影、底板放大到 plate_scale 的比例。

    系统图标在 64px 以下就不画投影了 —— 投影只有一两个像素高，烤进
    图像里只是一圈发灰的边。
    """
    if size >= 128:
        return svg
    svg = svg.replace('<g filter="url(#sh)">', "<g>")
    k = plate_scale(size)
    if abs(k - 1.0) < 1e-6:
        return svg
    t = (1 - k) / 2 * 1024
    svg = svg.replace(
        '<path d="M',
        f'<g transform="translate({t:.3f} {t:.3f}) scale({k:.5f})"><path d="M',
        1,
    )
    return svg.replace("</svg>", "</g></svg>")


def render_app(src, png, size):
    """应用图标：大尺寸按设计稿原样，小尺寸走 small_size_svg。"""
    render(src, png, size, size, post=lambda s: small_size_svg(s, size))


def main():
    if not os.path.exists(CHROME):
        sys.exit(f"找不到 Chrome：{CHROME}\n换机器时先装 Chrome，或者手工把 PNG 拷过来。")

    work = tempfile.mkdtemp(prefix="isw-icons-")
    try:
        app = os.path.join(BRAND, "icon.svg")
        bar = os.path.join(BRAND, "menubar.svg")
        tray = os.path.join(BRAND, "tray-win.svg")

        # ── 应用图标 ──────────────────────────────────────────────
        # 每个尺寸都按原生像素渲染，不是把 1024 缩下来。
        app_sizes = [16, 24, 32, 48, 64, 128, 256, 512, 1024]
        app_pngs = {}
        for s in app_sizes:
            p = os.path.join(work, f"app-{s}.png")
            render_app(app, p, s)
            app_pngs[s] = p

        # Windows 缩放时会按 20/40/80/96 这些中间尺寸去挑，没有就退到最近的再拉伸。
        # 直接按原生像素补上，Explorer 和任务栏才都不糊。
        win_pngs = []
        for s in (16, 20, 24, 32, 40, 48, 64, 80, 96, 128, 256):
            if s in app_pngs:
                win_pngs.append(app_pngs[s])
                continue
            p = os.path.join(work, f"app-{s}.png")
            render_app(app, p, s)
            win_pngs.append(p)

        shutil.copy(app_pngs[1024], os.path.join(ASSETS, "AppIcon-1024.png"))

        # .ico：Windows 在各种缩放比下会挑的尺寸
        write_ico(win_pngs, os.path.join(ASSETS, "AppIcon.ico"))

        # .icns：iconutil 要求固定的文件名
        iconset = os.path.join(work, "AppIcon.iconset")
        os.makedirs(iconset)
        for s in (16, 32, 128, 256, 512):
            shutil.copy(app_pngs[s], os.path.join(iconset, f"icon_{s}x{s}.png"))
            shutil.copy(
                app_pngs[s * 2], os.path.join(iconset, f"icon_{s}x{s}@2x.png")
            )
        write_icns(iconset, os.path.join(ASSETS, "AppIcon.icns"))

        # ── 菜单栏（模板图：整张只有 alpha 有意义，系统按明暗反色）──
        # 画布是 22pt；@2x 靠 device-scale-factor 出 44 像素，而不是把 22 拉大。
        for name, scale in (("menubar.png", 1), ("menubar@2x.png", 2)):
            p = os.path.join(work, f"bar-{scale}.png")
            render(bar, p, 22, 22, scale=scale)
            shutil.copy(p, os.path.join(ASSETS, name))

        # ── Windows 通知区域（彩色，不带底板）────────────────────
        p = os.path.join(work, "tray-32.png")
        render(tray, p, 32, 32)
        shutil.copy(p, os.path.join(ASSETS, "tray-win-32.png"))
    finally:
        shutil.rmtree(work, ignore_errors=True)

    for name in (
        "AppIcon-1024.png",
        "AppIcon.icns",
        "AppIcon.ico",
        "menubar.png",
        "menubar@2x.png",
        "tray-win-32.png",
    ):
        path = os.path.join(ASSETS, name)
        print(f"{name:20} {os.path.getsize(path):>7} bytes")


if __name__ == "__main__":
    main()

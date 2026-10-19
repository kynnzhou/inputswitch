# 在 Windows 上构建 InputSwitch.exe
# 用法（PowerShell，在工程目录下）：
#   .\build.ps1
$ErrorActionPreference = "Stop"

Write-Host "==> 编译（release）" -ForegroundColor Cyan

# 诊断 CLI 不带 tray 特性：不链接 WebView2，单文件即可分发
cargo build --release --bin inputswitch-cli --no-default-features
if ($LASTEXITCODE -ne 0) { throw "编译失败" }

# 托盘本体带 tray 特性
cargo build --release --bin InputSwitch
if ($LASTEXITCODE -ne 0) { throw "编译失败" }

$dist = "dist\windows"
New-Item -ItemType Directory -Force -Path $dist | Out-Null

$gui = "target\release\InputSwitch.exe"
$cli = "target\release\inputswitch-cli.exe"
if (-not (Test-Path $gui)) { throw "找不到 $gui" }

Copy-Item $gui $dist -Force
if (Test-Path $cli) { Copy-Item $cli $dist -Force }

# WebView2 载入器：MSVC 工具链是静态链进 exe 的（用不着这个 dll），
# GNU 工具链是动态链的（必须有）。无脑拷过去，两种情况都不出错。
$dll = "tools\webview2\x64\WebView2Loader.dll"
if (Test-Path $dll) {
    Copy-Item $dll $dist -Force
} else {
    Write-Host "提示：没找到 $dll（MSVC 构建可忽略；GNU 构建会启动失败）" -ForegroundColor Yellow
}

Write-Host ""
Write-Host "完成: $dist" -ForegroundColor Green
Write-Host "  InputSwitch.exe  双击运行，出现在系统托盘"
Write-Host "  inputswitch-cli.exe  命令行诊断：probe / selftest / status"
Write-Host ""
Write-Host "开机自启：把 InputSwitch.exe 的快捷方式放进"
Write-Host "  %APPDATA%\Microsoft\Windows\Start Menu\Programs\Startup"

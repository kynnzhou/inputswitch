# InputSwitch 的 Windows 安装器（NSIS，除 nsExec（随 NSIS 分发）外零插件）。
#
# 由 tools/pack-windows.mjs 调用，不要手敲 makensis：
#   makensis /DVERSION=0.6.90 /DSTAGING=.build/stage /DOUT=.build/InputSwitch-setup.exe \
#     packaging/windows/InputSwitch.nsi
# 版本号只从命令行来：Cargo.toml 的 version 是真源，pack 脚本负责读它并传进来。
#
# 几个决定的理由：
# - **装到 $LOCALAPPDATA\Programs\InputSwitch，RequestExecutionLevel user**：
#   免管理员。自动更新（src/update/install.rs）是「下载 → 等本进程退出 → 静默
#   安装器覆盖」的链路，装在用户目录下这条路永远有写权限；装 Program Files 的话
#   静默 /S 也会弹 UAC，倒计时归零后没有人点「是」。
# - **不写开机自启**：自启开关是程序内功能（src/autostart.rs），安装器替它写
#   注册表的话，两处状态必然不一致。
# - **安装/卸载前 taskkill**：运行中的 exe 不能被覆盖（能改名、不能写）。update
#   模块的 helper 已经保证「等本进程退出再调安装器」，这里是给「用户手动双击
#   新版安装器」兜底。taskkill 退出码不拦安装：0 = 杀到了、128 = 本来没在跑。
# - **只英文界面**：安装器只是分发通道，文案越少越不容易和行为脱节；程序内的
#   中文说明在面板和手册里。
# - **卸载不删配置**：配置在 %APPDATA%（src/config 的 paths），重装时配对、热键
#   都还在。真要清干净用面板里的重置。

!ifndef VERSION
  !error "必须传 /DVERSION=x.y.z（tools/pack-windows.mjs 从 Cargo.toml 读入后传入）"
!endif
!ifndef STAGING
  !error "必须传 /DSTAGING=<无空格目录>（tools/pack-windows.mjs 准备的暂存目录；makensis 的 /D 值遇空格会被截断）"
!endif
!ifndef OUT
  !define OUT "InputSwitch-setup.exe"
!endif

!define APP_NAME "InputSwitch"
!define PUBLISHER "kynnzhou"
# 安装目录与卸载条目都写 HKCU，和免管理员安装对应
!define REG_APP "Software\InputSwitch"
!define REG_UNINSTALL "Software\Microsoft\Windows\CurrentVersion\Uninstall\InputSwitch"

Name "InputSwitch"
Caption "InputSwitch 安装器"
BrandingText "kynnzhou/inputswitch"
OutFile "${OUT}"
InstallDir "$LOCALAPPDATA\Programs\${APP_NAME}"
InstallDirRegKey HKCU "${REG_APP}" "InstallDir"
RequestExecutionLevel user
SetCompressor /SOLID lzma

# 资源管理器「属性 → 详细信息」里看到的版本。四段式，缺的一段补 .0
VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "${APP_NAME}"
VIAddVersionKey "CompanyName" "${PUBLISHER}"
VIAddVersionKey "FileDescription" "${APP_NAME} 安装器"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "LegalCopyright" "MIT"

# 无 MUI：一个安装进度页就够（静默 /S 下页面本来就全跳过）
Page instfiles
# 卸载器：确认页 + 进度页
UninstPage uninstConfirm
UninstPage instfiles

!macro KillRunning
  nsExec::ExecToLog 'taskkill /IM InputSwitch.exe /F'
  Pop $0
  Sleep 300
!macroend

Section "Install"
  !insertmacro KillRunning
  SetOutPath "$INSTDIR"
  # 托盘本体 + 诊断 CLI。inputswitch-cli 按 --no-default-features 单独构建，
  # 不链接 wry，放进来给终端诊断用（probe / selftest / update）
  File "${STAGING}\InputSwitch.exe"
  File "${STAGING}\inputswitch-cli.exe"
  # GNU 工具链构建时 WebView2 的载入器是动态链的，这个 dll 必须陪在 exe 旁边
  # （见 manual.md 的 Windows 分发注意）；MSVC 构建是静态链入，带上也无害。
  # staging 里没有（MSVC 构建没拷）就跳过。
  !if /FileExists "${STAGING}\WebView2Loader.dll"
    File "${STAGING}\WebView2Loader.dll"
  !endif

  # 开始菜单：程序 + 卸载。不建桌面快捷方式 —— 托盘应用常驻托盘，桌面图标是噪音
  CreateShortcut "$SMPROGRAMS\InputSwitch.lnk" "$INSTDIR\InputSwitch.exe"
  CreateShortcut "$SMPROGRAMS\卸载 InputSwitch.lnk" "$INSTDIR\uninstall.exe"

  WriteUninstaller "$INSTDIR\uninstall.exe"
  WriteRegStr HKCU "${REG_APP}" "InstallDir" "$INSTDIR"
  # 「应用 → 安装的应用」条目
  WriteRegStr HKCU "${REG_UNINSTALL}" "DisplayName" "InputSwitch"
  WriteRegStr HKCU "${REG_UNINSTALL}" "DisplayIcon" "$INSTDIR\InputSwitch.exe"
  WriteRegStr HKCU "${REG_UNINSTALL}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${REG_UNINSTALL}" "Publisher" "${PUBLISHER}"
  WriteRegStr HKCU "${REG_UNINSTALL}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegDWORD HKCU "${REG_UNINSTALL}" "NoModify" 1
  WriteRegDWORD HKCU "${REG_UNINSTALL}" "NoRepair" 1
SectionEnd

Section "Uninstall"
  !insertmacro KillRunning
  Delete "$INSTDIR\InputSwitch.exe"
  Delete "$INSTDIR\inputswitch-cli.exe"
  Delete "$INSTDIR\WebView2Loader.dll"
  Delete "$INSTDIR\uninstall.exe"
  # 只删空目录：用户在安装目录里放了自己的东西时不误伤
  RMDir "$INSTDIR"
  Delete "$SMPROGRAMS\InputSwitch.lnk"
  Delete "$SMPROGRAMS\卸载 InputSwitch.lnk"
  DeleteRegKey HKCU "${REG_APP}"
  DeleteRegKey HKCU "${REG_UNINSTALL}"
SectionEnd

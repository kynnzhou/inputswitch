import { useCallback, useEffect, useState } from "react";
import { IconSprite, Icon } from "./components/IconSprite";
import { Sidebar } from "./components/Sidebar";
import { Topbar } from "./components/Topbar";
import { Workspace } from "./components/Workspace";
import { Devices } from "./components/Devices";
import { Transfers } from "./components/Transfers";
import { Settings } from "./components/Settings";
import { useSnapshot } from "./hooks/useSnapshot";
import { HelpModal, DiagnosticsModal, ScanModal, PortsModal, UpdateModal } from "./components/Modals";
import { useTheme } from "./hooks/useTheme";
import { kvmTakeback, updateAction } from "./lib/api";
import type { PageId, Toast } from "./lib/ui";
import type { Snapshot } from "./lib/types";
import brand from "./assets/brand.svg";
import "./styles/panel.css";

// 面板壳 = design/interactive 的 .app-window：
// 49px 标题栏横贯 / 178px 侧栏 + 主区 / 32px 页脚。窗口本身就是这个网格，
// 不要再包一层（旧版 .app 那层是「壳外还有壳」，是之前排版错位的根因）。
export function App() {
  const { snap, offline, refresh } = useSnapshot();
  const { theme, setTheme, reduceMotion, toggleReduceMotion } = useTheme();
  const [page, setPage] = useState<PageId>(() => {
    // 截图/排查用：?page=devices 直接落到某一页（不影响正常使用）
    const p = new URLSearchParams(window.location.search).get("page");
    return p === "devices" || p === "transfers" || p === "settings" ? p : "workspace";
  });
  const [toast, setToast] = useState<Toast>({ text: "", until: 0 });
  // 弹窗（一次只开一个；帮助 → 排查的转场用 push 替换）
  const [modal, setModal] = useState<null | "help" | "diag" | "scan" | "ports" | "update">(null);
  // 摆位图选中态：设备页与工作台多屏共用一份（同一份数据的两个视角）
  const [selectedNode, setSelectedNode] = useState<string | null>(null);
  // 摆位乐观层：放下立即生效，后端快照回来后被覆盖
  const [optimistic, setOptimistic] = useState<{ node: string; side: string }[]>([]);

  const flash = useCallback((text: string) => {
    setToast({ text, until: Date.now() + 3200 });
  }, []);

  // toast 到期自动清（对照旧面板 flash 的 3.2s setTimeout）
  useEffect(() => {
    if (!toast.text) return;
    const t = setTimeout(() => setToast({ text: "", until: 0 }), 3200);
    return () => clearTimeout(t);
  }, [toast]);

  // 「图标窄栏」不是纯 CSS 断点：@container app (max-width:950px) 只负责把文字藏掉，
  // 真正把栅格第一列从 178 收成 65 的是 .compact-nav 这个类，设计稿由 JS 挂上去。
  // 之前一直没挂 —— 820 窗口下侧栏就是一根 178 宽的空柱子，图标浮在中间还白吃 113px。
  // 用 ResizeObserver 而不是 window.resize：窗口尺寸由 webview 决定，且要量的是 .app-window 本身。
  useEffect(() => {
    const el = document.getElementById("appWindow");
    if (!el) return;
    const sync = () => el.classList.toggle("compact-nav", el.clientWidth <= 950);
    sync();
    // jsdom / 老 webview 里没有 ResizeObserver，退化成只在挂载时判一次
    if (typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(sync);
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  // 切页重放一次入场动画（设计稿的 .view-enter，180ms 位移 4px）
  useEffect(() => {
    const el = document.getElementById(`${page}View`);
    if (!el) return;
    el.classList.remove("view-enter");
    const raf = requestAnimationFrame(() => el.classList.add("view-enter"));
    return () => cancelAnimationFrame(raf);
  }, [page]);

  const hasUpdate =
    !!snap?.update?.latest &&
    snap.update.latest !== snap.update.current &&
    snap.update.skipped !== snap.update.latest;

  return (
    <>
      <IconSprite />
      <section className="app-window" id="appWindow" aria-label="InputSwitch 控制面板">
        <header className="app-titlebar">
          <div className="brand">
            <img src={brand} alt="" width={25} height={25} />
            <strong>InputSwitch</strong>
          </div>
          <div className="titlebar-actions">
            <button
              id="updateBtn"
              className="icon-button update-entry"
              onClick={() => setModal("update")}
              title={hasUpdate ? `有新版本 ${snap?.update.latest}` : "软件更新"}
              aria-label="软件更新"
            >
              <Icon name="download" />
              {hasUpdate && <i className="update-dot" id="updateDot" />}
            </button>
            <button className="icon-button" onClick={() => setModal("help")} title="使用帮助" aria-label="使用帮助">
              <Icon name="help" />
            </button>
          </div>
        </header>

        <Sidebar snap={snap} page={page} onNavigate={setPage} onHelp={() => setModal("help")} />

        <div className="app-main">
          <Topbar
            snap={snap}
            flash={flash}
            refresh={refresh}
            openScan={() => setModal("scan")}
            openShortcut={() => setPage("settings")}
          />
          <div className="view-container" id="viewContainer">
            {offline && (
              <div className="alert-inline workspace-alert error" role="alert">
                <Icon name="info" />
                <span>控制台与主程序失联了 —— 显示的是最后一次拿到的状态。主程序退出或重启中会这样。</span>
              </div>
            )}
            <section id="workspaceView" className="page-view" aria-label="工作台" hidden={page !== "workspace"}>
              <Workspace
                snap={snap}
                flash={flash}
                onNavigate={setPage}
                selectedNode={selectedNode}
                onSelectNode={setSelectedNode}
                optimistic={optimistic}
                setOptimistic={setOptimistic}
                refresh={refresh}
                openPorts={() => setModal("ports")}
              />
            </section>
            <section id="devicesView" className="page-view" aria-label="设备与布局" hidden={page !== "devices"}>
              <Devices
                snap={snap}
                flash={flash}
                refresh={refresh}
                selectedNode={selectedNode}
                onSelectNode={setSelectedNode}
                optimistic={optimistic}
                setOptimistic={setOptimistic}
              />
            </section>
            <section id="transfersView" className="page-view" aria-label="文件传输" hidden={page !== "transfers"}>
              <Transfers snap={snap} flash={flash} />
            </section>
            <section id="settingsView" className="page-view" aria-label="设置" hidden={page !== "settings"}>
              <Settings
                snap={snap}
                theme={theme}
                setTheme={setTheme}
                reduceMotion={reduceMotion}
                toggleReduceMotion={toggleReduceMotion}
                openUpdate={() => setModal("update")}
              />
            </section>
          </div>
        </div>

        <footer className="app-footer">
          <FooterConnection snap={snap} />
          <span className="footer-message" role="status" aria-live="polite">
            {offline ? "主程序失联 · 这是最后一次拿到的状态" : snap ? "一切都好" : "连接中…"}
          </span>
          <button
            className="text-button escape-hint"
            onClick={() => {
              // 键鼠本来就在本机时没什么可断的，给一句话比静默更好
              if (!snap?.kvm?.active) return flash("键鼠现在就在本机");
              void kvmTakeback().then(() => flash("已把键鼠收回本机"));
            }}
            title="把键鼠收回本机（也可以随时按 Ctrl + Alt + Esc）"
          >
            <kbd>Ctrl + Alt + Esc</kbd>
            <span>紧急断开</span>
          </button>
        </footer>

        <div className="toast-stack" id="toastStack" aria-live="polite" aria-atomic="true">
          {toast.text && (
            <div className="toast">
              <Icon name="info" />
              <span>{toast.text}</span>
            </div>
          )}
        </div>
      </section>

      {snap && modal === "help" && (
        <HelpModal snap={snap} onClose={() => setModal(null)} onDiagnostics={() => setModal("diag")} />
      )}
      {modal === "diag" && <DiagnosticsModal onClose={() => setModal(null)} />}
      {modal === "scan" && <ScanModal onClose={() => setModal(null)} refresh={refresh} flash={flash} />}
      {snap && modal === "ports" && <PortsModal snap={snap} onClose={() => setModal(null)} refresh={refresh} flash={flash} />}
      {snap && modal === "update" && <UpdateModal snap={snap} onClose={() => setModal(null)} refresh={refresh} flash={flash} />}
      <RestartOverlay snap={snap} />
    </>
  );
}

/// 更新安装的重启倒计时全屏层（对照旧面板 restartOverlay）：
/// phase=restarting 时盖住全屏，显示剩余秒数 + 取消。装的过程本程序会退出、
/// 画面会闪，必须明说"正在发生什么"并给显眼的取消。轮询在 restarting
/// 阶段自动提到 250ms（useSnapshot），倒计时是流畅的。
function RestartOverlay({ snap }: { snap: Snapshot | null }) {
  const u = snap?.update;
  if (!u || u.phase !== "restarting") return null;
  return (
    <div className="restart-mask">
      <div className="restart-box">
        <div className="restart-num">{u.restart_in || 0}</div>
        <div className="restart-tip">秒后重启并安装更新</div>
        <div className="restart-sub">
          安装过程中本程序会退出，装完自动拉起新版本。取消可以继续用当前版本。
        </div>
        <button className="primary" onClick={() => void updateAction("cancel")}>
          取消重启
        </button>
      </div>
    </div>
  );
}

function FooterConnection({ snap }: { snap: Snapshot | null }) {
  if (!snap) return <span className="footer-connection">…</span>;
  if (snap.paired && snap.peer_verified > 0) {
    return (
      <span className="footer-connection">
        <Icon name="shield" />
        <span>已配对 · 连接已加密</span>
      </span>
    );
  }
  if (snap.paired) {
    return (
      <span className="footer-connection">
        <Icon name="clock" />
        <span>配对码已填 · 等待对端确认</span>
      </span>
    );
  }
  return (
    <span className="footer-connection">
      <Icon name="link" />
      <span>尚未配对 · 请先连接设备</span>
    </span>
  );
}

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
import type { PageId, Toast } from "./lib/ui";
import type { Snapshot } from "./lib/types";
import "./styles/panel.css";

export function App() {
  const { snap, offline, refresh } = useSnapshot();
  const { theme, setTheme, reduceMotion, toggleReduceMotion } = useTheme();
  const [page, setPage] = useState<PageId>("workspace");
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

  return (
    <>
      <IconSprite />
      {offline && (
        <div className="banner warn" role="alert" style={{ margin: 12 }}>
          <span className="grow">
            <b>控制台与主程序失联了</b>
            <span className="sub"> —— 显示的是最后一次拿到的状态。主程序退出或重启中会这样。</span>
          </span>
        </div>
      )}
      <div className="app">
        <Sidebar snap={snap} page={page} onNavigate={setPage} />
        <div className="app-main">
          <Topbar
            snap={snap}
            toast={toast}
            flash={flash}
            refresh={refresh}
            openHelp={() => setModal("help")}
            openScan={() => setModal("scan")}
            openUpdate={() => setModal("update")}
          />
          <div className="view-container" id="viewContainer">
            {page === "workspace" && (
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
            )}
            {page === "devices" && (
              <Devices
                snap={snap}
                flash={flash}
                refresh={refresh}
                selectedNode={selectedNode}
                onSelectNode={setSelectedNode}
                optimistic={optimistic}
                setOptimistic={setOptimistic}
              />
            )}
            {page === "transfers" && <Transfers snap={snap} flash={flash} />}
            {page === "settings" && (
              <Settings
                snap={snap}
                theme={theme}
                setTheme={setTheme}
                reduceMotion={reduceMotion}
                toggleReduceMotion={toggleReduceMotion}
              />
            )}
          </div>
        </div>
        <footer className="app-footer">
          <FooterConnection snap={snap} />
          <span className="footer-message">{snap?.hotkey ? `切屏热键 ${snap.hotkey}` : ""}</span>
          <span className="escape-hint">
            <kbd>Ctrl</kbd>
            <kbd>Alt</kbd>
            <kbd>Esc</kbd>
            <span>紧急断开键鼠</span>
          </span>
        </footer>
      </div>
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
      <div className="restart-box glass">
        <div className="restart-num">{u.restart_in || 0}</div>
        <div className="restart-tip">秒后重启并安装更新</div>
        <div className="restart-sub">
          安装过程中本程序会退出，装完自动拉起新版本。取消可以继续用当前版本。
        </div>
        <button className="btn-lg" onClick={() => void import("./lib/api").then((m) => m.updateAction("cancel"))}>
          取消重启
        </button>
      </div>
    </div>
  );
}

function FooterConnection({ snap }: { snap: Snapshot | null }) {
  if (!snap) return <span className="footer-connection off">…</span>;
  if (snap.paired && snap.peer_verified > 0) {
    return (
      <span className="footer-connection">
        <Icon name="check" />
        <span>已配对 · 已连接</span>
      </span>
    );
  }
  if (snap.paired) {
    return (
      <span className="footer-connection off">
        <Icon name="clock" />
        <span>配对码已填 · 等对端确认</span>
      </span>
    );
  }
  return (
    <span className="footer-connection off">
      <Icon name="shield" />
      <span>未配对</span>
    </span>
  );
}

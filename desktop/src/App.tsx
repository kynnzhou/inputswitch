import { useCallback, useEffect, useState } from "react";
import { IconSprite, Icon } from "./components/IconSprite";
import { Sidebar } from "./components/Sidebar";
import { Topbar } from "./components/Topbar";
import { Workspace } from "./components/Workspace";
import { Devices } from "./components/Devices";
import { Transfers } from "./components/Transfers";
import { Settings } from "./components/Settings";
import { useSnapshot } from "./hooks/useSnapshot";
import { useTheme } from "./hooks/useTheme";
import type { PageId, Toast } from "./lib/ui";
import type { Snapshot } from "./lib/types";
import "./styles/panel.css";

export function App() {
  const { snap, offline, refresh } = useSnapshot();
  const { theme, setTheme, reduceMotion, toggleReduceMotion } = useTheme();
  const [page, setPage] = useState<PageId>("workspace");
  const [toast, setToast] = useState<Toast>({ text: "", until: 0 });

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
          <Topbar snap={snap} onNavigate={setPage} toast={toast} flash={flash} refresh={refresh} />
          <div className="view-container" id="viewContainer">
            {page === "workspace" && <Workspace snap={snap} flash={flash} onNavigate={setPage} />}
            {page === "devices" && <Devices snap={snap} flash={flash} refresh={refresh} />}
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
    </>
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

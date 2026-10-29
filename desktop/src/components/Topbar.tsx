import type { Snapshot } from "../lib/types";
import type { Toast } from "../lib/ui";
import { Icon } from "./IconSprite";
import { askPeer, checkSupport, rescan, switchTo } from "../lib/api";
import { nextTarget, kvmFollowing } from "../lib/pure";

export function Topbar({
  snap,
  toast,
  flash,
  refresh,
  openHelp,
  openScan,
  openUpdate,
}: {
  snap: Snapshot | null;
  toast: Toast;
  flash: (text: string) => void;
  refresh: () => void;
  openHelp: () => void;
  openScan: () => void;
  openUpdate: () => void;
}) {
  if (!snap) {
    return (
      <header className="topbar glass">
        <div className="statusline">
          <div className="stat node">
            <Icon name="windows" />
            <span className="k">本机</span>
            <span className="v">…</span>
          </div>
        </div>
      </header>
    );
  }

  const cur = snap.inputs.find((i) => i.current);
  const k = snap.kvm;
  const blind = snap.has_display === false;
  const peer = snap.peers.find((p) => p.knows_port);
  const tgt = blind ? null : nextTarget(snap.inputs);
  const maintBusy = snap.maint_busy;

  // 主按钮三态（对照 render.js 335-382）：blind → 请对端切回；
  // 有目标 → 切到 X；否则 →「没有其他输入口」+ .is-off。永远显示、永不 disabled。
  const mainOff = (blind && !peer) || !tgt;
  const mainText = blind
    ? peer
      ? `请 ${peer.node} 把画面切回本机`
      : "请对端把画面切回本机"
    : tgt
      ? `切到 ${tgt.label}${kvmFollowing(snap) ? "（键鼠跟过去）" : ""}`
      : "没有其他输入口";
  const nextStep = blind
    ? peer
      ? "→ 本机读不到这块屏，切回要由正在显示画面的那台来发指令。"
      : "→ 对端不在线，画面暂时切不回来。"
    : tgt
      ? peer
        ? `→ 切过去之后，本机读不到这块屏，要由 ${peer.node} 切回来。`
        : "→ 对端不在线：切过去就切不回来，先让对端开机并运行本程序。"
      : "→ 扫描一次可以确认别的口上有没有主机。";

  const quickSwitch = () => {
    if (blind) {
      if (!peer) return flash("暂时切不回来 · 对端不在线，得由正在显示画面的那台发指令");
      void askPeer(snap.own_value).then(refresh);
      return;
    }
    if (!tgt) return flash("这台屏没有可切的口 · 先点「扫描输入口」确认别的口上有没有主机");
    void switchTo(tgt.value).then(refresh);
  };

  // status 行：只显示动作回执（"型号 · 当前口"这类重复信息不占行，对照 render.js）
  const idleStatus =
    !!snap.status &&
    ((snap.display && snap.status.startsWith(snap.display + " · ")) ||
      (!snap.has_display && snap.status.startsWith("本机没有接通的显示器")));
  const statusText = Date.now() < toast.until ? toast.text : idleStatus ? "" : snap.status;

  return (
    <header className="topbar glass">
      <div className="statusline">
        <div className="stat node">
          <Icon name="windows" />
          <span className="k">本机</span>
          <span className="v">{snap.node}</span>
        </div>
        <div className="stat">
          <Icon name="monitor" />
          <span className="k">画面</span>
          <span className="v">
            {blind ? (
              <span className="sub">在另一台机器上</span>
            ) : cur ? (
              cur.label || cur.name || "输入源"
            ) : (
              <span className="sub">读不到</span>
            )}
          </span>
        </div>
        <div className="stat">
          <Icon name="mouse" />
          <span className="k">键鼠</span>
          <span className="v">{k.active || "本机"}</span>
        </div>
        <div className="stat">
          <span className="k">对端</span>
          <span className="v">
            <span className={`dot${k.peer_alive ? " on" : " off"}`}></span> {snap.peers.length} 台在线
          </span>
        </div>
      </div>
      <div className="actions">
        {/* 主按钮：永远显示，不能切时换文案 + .is-off（铁律，vitest 钉住） */}
        <button
          id="mainSwitch"
          className={`primary btn-lg${mainOff ? " is-off" : ""}`}
          onClick={quickSwitch}
        >
          <Icon name="swap" />
          <span>{mainText}</span>
        </button>
        <button id="updateBtn" className="upd-btn" onClick={openUpdate}>
          <Icon name="download" />
          更新
          <i className={`upd-dot${snap.update.latest && snap.update.latest !== snap.update.current && snap.update.skipped !== snap.update.latest ? " on" : ""}`} id="updateDot" />
        </button>
        <div className="spacer"></div>
        <button className="icon-help" onClick={openHelp} title="使用帮助" aria-label="使用帮助">
          <Icon name="help" />
        </button>
        <div className="flat-group">
          <button id="maintRescan" disabled={maintBusy} onClick={() => void rescan().then(refresh)}>
            <Icon name="refresh" />
            重新检测
          </button>
          <button id="maintScan" disabled={maintBusy} onClick={openScan}>
            <Icon name="scan" />
            扫描输入口
          </button>
          <button id="maintCheck" disabled={maintBusy} onClick={() => void checkSupport().then(refresh)}>
            <Icon name="check-circle" />
            检测能否切换
          </button>
        </div>
      </div>
      <div className="nextstep">{nextStep}</div>
      {snap.support && !snap.support.startsWith("✓") && (
        <div className="hint">能否用软件切换：{snap.support}</div>
      )}
      {statusText && <div className="toast">{statusText}</div>}
    </header>
  );
}

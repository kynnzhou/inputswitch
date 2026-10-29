import type { Snapshot } from "../lib/types";
import { Icon } from "./IconSprite";
import { askPeer, checkSupport, rescan, switchTo } from "../lib/api";
import { nextTarget, kvmFollowing } from "../lib/pure";

// 命令区 = 设计稿 .command-bar：上面一行「画面 / 键鼠 / 在线」+ 唯一实心蓝主按钮，
// 下面一行三个维护动作 + 快捷键提示。主按钮铁律不变：永远显示、永不 disabled，
// 不能切时换成灰蓝（.unavailable + .is-off，后者是 vitest 钉住的类名）。
export function Topbar({
  snap,
  flash,
  refresh,
  openScan,
  openShortcut,
}: {
  snap: Snapshot | null;
  flash: (text: string) => void;
  refresh: () => void;
  openScan: () => void;
  /** 点快捷键提示 → 去设置页改键（设计稿 data-action="shortcut" 的行为） */
  openShortcut: () => void;
}) {
  if (!snap) {
    return (
      <header className="command-bar">
        <div className="command-top">
          <div className="owner-facts">
            <div className="owner-fact">
              <Icon name="monitor" />
              <span className="fact-label">画面</span>
              <strong>…</strong>
            </div>
          </div>
        </div>
      </header>
    );
  }

  const k = snap.kvm;
  const blind = snap.has_display === false;
  const peer = snap.peers.find((p) => p.knows_port);
  const tgt = blind ? null : nextTarget(snap.inputs);
  const maintBusy = snap.maint_busy;

  // 主按钮三态（对照 render.js 335-382）：blind → 请对端切回；
  // 有目标 → 切到 X；否则 →「没有其他输入口」+ 灰蓝。永远显示、永不 disabled。
  const mainOff = (blind && !peer) || !tgt;
  const mainText = blind
    ? peer
      ? `请 ${peer.node} 把画面切回本机`
      : "请对端把画面切回本机"
    : tgt
      ? `切到 ${tgt.label}${kvmFollowing(snap) ? "（键鼠跟过去）" : ""}`
      : "没有其他输入口";
  const why = blind
    ? peer
      ? "本机读不到这块屏，切回要由正在显示画面的那台来发指令。"
      : "对端不在线，画面暂时切不回来。"
    : tgt
      ? peer
        ? `切过去之后本机读不到这块屏，要由 ${peer.node} 切回来。`
        : "对端不在线：切过去就切不回来，先让对端开机并运行本程序。"
      : "扫描一次可以确认别的口上有没有主机。";

  const quickSwitch = () => {
    if (blind) {
      if (!peer) return flash("暂时切不回来 · 对端不在线，得由正在显示画面的那台发指令");
      void askPeer(snap.own_value).then(refresh);
      return;
    }
    if (!tgt) return flash("这台屏没有可切的口 · 先点「扫描输入口」确认别的口上有没有主机");
    void switchTo(tgt.value).then(refresh);
  };

  const picture = blind ? peer?.node || "对端" : "本机";
  const keyboard = k.on ? k.active || "本机" : "本机";
  const online = snap.peers.length;

  return (
    <header className="command-bar">
      <div className="command-top">
        <div className="owner-facts">
          <div className="owner-fact">
            <Icon name="monitor" />
            <span className="fact-label">画面</span>
            <strong>{picture}</strong>
          </div>
          <span className="fact-divider"></span>
          <div className="owner-fact">
            <Icon name="mouse" />
            <span className="fact-label">键鼠</span>
            <strong>{keyboard}</strong>
          </div>
          <span className="connection-fact">
            <i className={`status-dot${k.peer_alive ? "" : " off"}`}></i>
            {online ? `${online} 台在线` : "暂无对端"}
          </span>
          {snap.support && !snap.support.startsWith("✓") && (
            <span className="tag warn" title={snap.support}>
              软件切换受限
            </span>
          )}
        </div>
        <button
          id="mainSwitch"
          className={`primary${mainOff ? " is-off unavailable" : ""}`}
          onClick={quickSwitch}
          title={why}
        >
          <Icon name="swap" />
          <span>{mainText}</span>
        </button>
      </div>
      <div className="maintenance-row">
        <div className="maintenance-actions">
          <button id="maintRescan" className="quiet-button" disabled={maintBusy} onClick={() => void rescan().then(refresh)}>
            <Icon name="refresh" />
            重新检测
          </button>
          <button id="maintScan" className="quiet-button" disabled={maintBusy} onClick={openScan}>
            <Icon name="scan" />
            扫描输入口
          </button>
          <button id="maintCheck" className="quiet-button" disabled={maintBusy} onClick={() => void checkSupport().then(refresh)}>
            <Icon name="check-circle" />
            检测能否切换
          </button>
        </div>
        <button
          className="hotkey-hint text-button"
          onClick={openShortcut}
          title={`切屏快捷键 ${snap.hotkey || "—"} · 点击修改`}
        >
          {(snap.hotkey || "Ctrl+Alt+S").split("+").map((key) => (
            <kbd key={key}>{key.trim()}</kbd>
          ))}
        </button>
      </div>
    </header>
  );
}

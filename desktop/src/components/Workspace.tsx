import { useState } from "react";
import type { Snapshot } from "../lib/types";
import type { PageId } from "../lib/ui";
import { Icon } from "./IconSprite";
import { askPeer, kvmPower, sendFiles, switchTo, toggleClipboard } from "../lib/api";

/** 一行警示（对照 alert-inline）：图标 + 一句话 + 一个直达动作 */
function AlertInline({
  icon,
  text,
  action,
  error,
}: {
  icon: string;
  text: string;
  action?: { label: string; onClick: () => void };
  error?: boolean;
}) {
  return (
    <div className={`alert-inline${error ? " error" : ""}`} role={error ? "alert" : "status"}>
      <Icon name={icon} />
      <span>{text}</span>
      {action && <button onClick={action.onClick}>{action.label}</button>}
    </div>
  );
}

/** 场景警示六分支（对照 render.js renderSceneAlert），没有就整块消失 */
function SceneAlert({ snap, onNavigate }: { snap: Snapshot; onNavigate: (p: PageId) => void }) {
  const peers = snap.peers;
  const k = snap.kvm;
  if (!snap.paired) {
    return (
      <AlertInline
        icon="link"
        text="两台电脑填入相同配对码，连接后即可共享。"
        action={{ label: "去配对", onClick: () => onNavigate("devices") }}
      />
    );
  }
  if (snap.peer_verified === 0) {
    const hint = snap.rejected
      ? `对端发来的 ${snap.rejected} 条消息没通过校验：多半是两边码不一样，或有一边版本较旧。`
      : "配对码已保存，正在等对端确认身份。";
    return (
      <AlertInline
        icon="link"
        text={hint}
        action={{ label: "去设备页", onClick: () => onNavigate("devices") }}
      />
    );
  }
  if (!peers.length) {
    return (
      <AlertInline
        icon="wifi"
        text="还没有发现其他机器。两台都运行本程序，同一网段会自动出现。"
        action={{ label: "查看设备", onClick: () => onNavigate("devices") }}
      />
    );
  }
  if (!k.peer_alive) {
    return (
      <AlertInline
        icon="wifi"
        text="对端已离线，请确认另一台电脑已开机并运行 InputSwitch。"
        action={{ label: "查看设备", onClick: () => onNavigate("devices") }}
      />
    );
  }
  if (snap.has_display && !snap.inputs.length) {
    return (
      <AlertInline
        icon="info"
        text="这台显示器上没有读到可切换的输入口，扫描一次再认。"
        action={{ label: "去扫描", onClick: () => document.getElementById("maintScan")?.click() }}
      />
    );
  }
  if (snap.support && !snap.support.startsWith("✓")) {
    return <AlertInline icon="info" text={snap.support} error />;
  }
  return null;
}

/** 单屏场景：共用显示器头 + 双机卡片 + flow + 输入源条（对照 singleSceneHtml） */
function SingleScene({ snap, refresh }: { snap: Snapshot; refresh: (t: string) => void }) {
  const inputs = snap.inputs;
  const blind = snap.has_display === false;
  const cur = inputs.find((i) => i.current) || null;
  const peerPort = inputs.find((i) => !i.current && i.presence === "occupied") || null;
  const peer = snap.peers.find((p) => p.knows_port) || snap.peers[0] || null;
  const follows = !!snap.kvm.follow_link;

  const tag = blind ? (
    <span className="tag warn">画面在别处</span>
  ) : !inputs.length ? (
    <span className="tag warn">输入口待连接</span>
  ) : snap.kvm.peer_alive ? (
    <span className="tag green">连接正常</span>
  ) : (
    <span className="tag plain">等待连接</span>
  );

  const wsSwitch = (who: "local" | "peer") => {
    if (who === "local" && blind) {
      void askPeer(snap.own_value).then(() => refresh("已请对端切换…"));
    } else if (who === "peer" && !blind && peerPort) {
      void switchTo(peerPort.value).then(() => refresh("切换中…"));
    }
  };

  return (
    <div className="scene-panel">
      <div className="scene-panel-head">
        <span>
          <Icon name="monitor" />
          共用显示器 <strong>{snap.display || "—"}</strong>
        </span>
        {tag}
      </div>
      <div className="screen-flow">
        <div className="computer">
          <button
            className={`computer-display${blind ? "" : " active"}`}
            onClick={() => wsSwitch("local")}
            aria-pressed={blind ? "false" : "true"}
            title={blind ? "点它请对端把画面切回本机" : "画面已经在本机"}
          >
            <span className="screen-content">
              <Icon name="windows" />
              <span>本机</span>
            </span>
          </button>
          <div className="computer-name">
            {snap.node || "本机"}
            {!blind && <span className="tag blue">当前</span>}
          </div>
          <div className="computer-meta">
            <i className={`status-dot${blind ? " off" : ""}`} />
            {blind
              ? "画面在另一台机器上"
              : cur
                ? `${cur.label} · 当前画面`
                : "没有检测到输入口"}
          </div>
        </div>
        <div className="flow-connection">
          <span className="flow-note">{follows ? "画面到哪，键鼠就跟到哪" : "仅切换画面"}</span>
          <div className="connection-line">
            <i className="signal"></i>
          </div>
          <Icon name={follows ? "keyboard" : "swap"} />
        </div>
        <div className="computer">
          <button
            className={`computer-display${blind ? " active" : ""}`}
            onClick={() => wsSwitch("peer")}
            aria-pressed={blind ? "true" : "false"}
            title={blind ? "画面已经在对端" : peerPort ? "点它把画面切到对端" : "没有检测到对端在哪个输入口"}
            disabled={!blind && !peerPort}
          >
            <span className="screen-content">
              <Icon name="monitor" />
              <span>对端</span>
            </span>
          </button>
          <div className="computer-name">
            {peer ? peer.node : "对端"}
            {blind && <span className="tag blue">当前</span>}
          </div>
          <div className="computer-meta">
            <i className={`status-dot${blind ? "" : " off"}`} />
            {blind ? "画面在这台显示" : peerPort ? `${peerPort.label} · 有主机` : "没有检测到对端的输入口"}
          </div>
        </div>
      </div>
      <div className="source-strip">
        <span>输入源</span>
        {!inputs.length && <span className="sub">没有检测到输入源</span>}
        {inputs.map((i) => {
          const dead = i.presence === "invalid";
          const why = dead
            ? "显示器固件不接受这个输入口"
            : i.current
              ? "画面已经在这个口上"
              : "";
          const suffix = dead
            ? " · 该口不存在"
            : !i.current && i.presence === "occupied"
              ? " · 有主机"
              : !i.current && i.presence === "idle"
                ? " · 无信号"
                : "";
          return (
            <button
              key={i.value}
              className={`source-button${i.current ? " current" : ""}`}
              disabled={dead || i.current}
              title={why || undefined}
              onClick={() => void switchTo(i.value).then(() => refresh("切换中…"))}
            >
              {i.current && <Icon name="check" />}
              {i.label}
              {suffix}
            </button>
          );
        })}
      </div>
    </div>
  );
}

export function Workspace({
  snap,
  flash,
  onNavigate,
}: {
  snap: Snapshot | null;
  flash: (t: string) => void;
  onNavigate: (p: PageId) => void;
}) {
  const [scene, setScene] = useState<"single" | "multi">("single");

  if (!snap) return <section className="page-view" aria-label="工作台" />;
  const k = snap.kvm;
  const subOn = !!k.auto_resume && !!k.keep_alive && snap.autostart;

  return (
    <section className="page-view" aria-label="工作台">
      {(!snap.perms.capture || !snap.perms.inject) && (
        <div className="banner warn">
          <span className="grow">
            <b>系统授权没生效</b>
            <br />
            <span className="sub">
              缺少权限：{!snap.perms.inject && "注入（接收对端的键鼠）"}
              {!snap.perms.inject && !snap.perms.capture && "、"}
              {!snap.perms.capture && "抓取（把本机键鼠送出去）"}。在系统设置里勾选后，必须重启本程序才会生效。
            </span>
          </span>
        </div>
      )}

      <div className="view-heading">
        <div>
          <h1>工作台</h1>
          <p>在设备之间，轻松接续你的工作。</p>
        </div>
        <div className="segmented" role="group" aria-label="工作场景">
          <button
            className={scene === "single" ? "selected" : ""}
            aria-pressed={scene === "single"}
            onClick={() => setScene("single")}
          >
            <Icon name="monitor" />
            单屏切换
          </button>
          <button
            className={scene === "multi" ? "selected" : ""}
            aria-pressed={scene === "multi"}
            onClick={() => setScene("multi")}
          >
            <Icon name="screens" />
            多屏共享
          </button>
        </div>
      </div>

      <SceneAlert snap={snap} onNavigate={onNavigate} />

      {scene === "single" ? (
        <SingleScene snap={snap} refresh={flash} />
      ) : (
        <div className="scene-panel">
          <div className="scene-panel-head">
            <span>
              <Icon name="screens" />
              多屏布局
            </span>
            <button className="btn-sm ghost" onClick={() => onNavigate("devices")}>
              管理布局 <Icon name="chevron" />
            </button>
          </div>
          <div className="source-strip">
            <span>
              <Icon name="keyboard" />
            </span>
            <span className="multi-summary">
              键鼠当前在 <strong>{k.active || "本机"}</strong>
            </span>
            {k.links.length === 0 && (
              <span className="sub">还没有摆位 —— 去设备页把对端放进格子</span>
            )}
          </div>
        </div>
      )}

      <div className="sharing-strip">
        <div className="sharing-item">
          <Icon name="mouse" />
          <div className="sharing-copy">
            <strong>共用键鼠</strong>
            <p>
              {k.on
                ? scene === "multi"
                  ? "鼠标推到边缘即可跨屏"
                  : "随画面切换，自动交接"
                : "已暂停，键鼠留在本机"}
            </p>
          </div>
          <button
            id="kvmSwitch"
            className={`sw${k.on ? " on" : ""}`}
            role="switch"
            aria-checked={k.on}
            aria-label="共用键鼠"
            onClick={() => void kvmPower(!k.on).then(() => flash(k.on ? "正在关闭共用键鼠…" : "正在开启共用键鼠…"))}
          />
        </div>
        <div className="sharing-item">
          <Icon name="clipboard" />
          <div className="sharing-copy">
            <strong>剪贴板同步</strong>
            <p>{snap.clipboard ? "文字只同步到当前使用的设备" : "已关闭，保留各自的剪贴板"}</p>
          </div>
          <button
            id="clipBtn"
            className={`sw${snap.clipboard ? " on" : ""}`}
            role="switch"
            aria-checked={snap.clipboard}
            aria-label="同步剪贴板文本"
            onClick={() => void toggleClipboard(!snap.clipboard)}
          />
        </div>
      </div>

      <div className="quick-transfer">
        <Icon name="folder" />
        <p>
          <strong>顺手传个文件</strong>
          <span>，接着在另一台处理。</span>
        </p>
        <button className="btn-sm" onClick={() => void sendFiles()}>
          <Icon name="upload" />
          发送文件
        </button>
        <button className="btn-sm ghost" title="去文件传输页" onClick={() => onNavigate("transfers")}>
          <Icon name="chevron" />
        </button>
      </div>
      {subOn && <div className="hint">副机模式开着：这台会自动恢复并保持共用键鼠。</div>}
    </section>
  );
}

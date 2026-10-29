import { useState } from "react";
import type { Snapshot } from "../lib/types";
import { Icon } from "./IconSprite";
import {
  kvmAutoResume,
  kvmKeepAlive,
  kvmPower,
  openConfigDir,
  recHotkey,
  resetHotkey,
  setKvmScale,
  submachine,
  toggleAutostart,
  toggleClipboard,
} from "../lib/api";
import type { ThemeChoice } from "../hooks/useTheme";

type Tab = "sharing" | "startup" | "appearance";

const TABS: [Tab, string][] = [
  ["sharing", "共享与快捷键"],
  ["startup", "启动与恢复"],
  ["appearance", "外观与关于"],
];

/** 设计稿 .setting-row：左说明、右控件 */
function Row({ title, desc, children }: { title: string; desc: React.ReactNode; children: React.ReactNode }) {
  return (
    <div className="setting-row">
      <div className="setting-text">
        <strong>{title}</strong>
        <p>{desc}</p>
      </div>
      {children}
    </div>
  );
}

function Toggle({ id, on, label, onToggle }: { id: string; on: boolean; label: string; onToggle: () => void }) {
  return <button id={id} className="toggle" role="switch" aria-checked={on} aria-label={label} onClick={onToggle} />;
}

// 设置 = 设计稿 renderSettings：三个选项卡，行内容按后端真值渲染。
// 「全页只有一个实心蓝按钮」在这里同样成立 —— 所以没有任何 .primary。
export function Settings({
  snap,
  theme,
  setTheme,
  reduceMotion,
  toggleReduceMotion,
  openUpdate,
}: {
  snap: Snapshot | null;
  theme: ThemeChoice;
  setTheme: (t: ThemeChoice) => void;
  reduceMotion: boolean;
  toggleReduceMotion: () => void;
  openUpdate?: () => void;
}) {
  const [tab, setTab] = useState<Tab>("sharing");
  if (!snap) return <section className="page-view" aria-label="设置" hidden />;

  const k = snap.kvm;
  const subOn = !!k.auto_resume && !!k.keep_alive && snap.autostart;
  const scale = Number(k.recv_scale || 1);
  const keyParts = (snap.hotkey || "Ctrl+Alt+S").split("+");

  return (
    <>
      <div className="view-heading">
        <div>
          <h1>设置</h1>
          <p>把常用的习惯，设置成默认。</p>
        </div>
        <span className="tag">
          <Icon name="check" />
          自动保存
        </span>
      </div>

      <div className="settings-tabs" role="group" aria-label="设置类别">
        {TABS.map(([id, label]) => (
          <button
            key={id}
            id={`tab-${id}`}
            className={tab === id ? "selected" : ""}
            aria-pressed={tab === id}
            onClick={() => setTab(id)}
          >
            {label}
          </button>
        ))}
      </div>

      <div className="settings-rows">
        {tab === "sharing" && (
          <>
            <Row title="共用键鼠" desc="共用一套键盘鼠标，按设备布局自动交接。">
              <Toggle
                id="kvmToggle"
                on={k.on}
                label="共用键鼠"
                onToggle={() => void kvmPower(!k.on)}
              />
            </Row>
            <Row title="光标速度" desc={`仅在键鼠交给对端时生效。当前 ×${scale.toFixed(1)}`}>
              <div className="segmented" id="kvmScaleRow" role="group" aria-label="光标速度">
                {[0.9, 1, 1.1, 1.2].map((v) => (
                  <button
                    key={v}
                    className={`${Math.abs(v - scale) < 0.001 ? "sel selected" : ""}`}
                    aria-pressed={Math.abs(v - scale) < 0.001}
                    onClick={() => void setKvmScale(v)}
                  >
                    {v.toFixed(1)}
                  </button>
                ))}
              </div>
            </Row>
            <Row title="同步剪贴板文本" desc="只同步到当前使用的设备，避免多台互相覆盖。">
              <Toggle
                id="clipToggle"
                on={snap.clipboard}
                label="剪贴板同步"
                onToggle={() => void toggleClipboard(!snap.clipboard)}
              />
            </Row>
            <Row title="切屏快捷键" desc="随时切换到另一个输入源。点右侧组合键可重新录制。">
              <div className="shortcut-control">
                <button className="keybind" onClick={() => void recHotkey()} title="点一下，然后到主程序窗口里按新的组合键">
                  {keyParts.map((p) => (
                    <kbd key={p}>{p.trim()}</kbd>
                  ))}
                </button>
                <button className="text-button" onClick={() => void resetHotkey()} title="恢复默认 Ctrl + Alt + S">
                  重置
                </button>
              </div>
            </Row>
          </>
        )}

        {tab === "startup" && (
          <>
            <Row title="开机自启" desc="登录电脑后自动运行 InputSwitch。">
              <Toggle id="autoBtn" on={snap.autostart} label="开机自启" onToggle={() => void toggleAutostart(!snap.autostart)} />
            </Row>
            <Row title="自动恢复共用键鼠" desc="对端上线后重新连接，不会开机就抢走键鼠。">
              <Toggle
                id="resumeBtn"
                on={k.auto_resume}
                label="自动恢复共用键鼠"
                onToggle={() => void kvmAutoResume(!k.auto_resume)}
              />
            </Row>
            <Row title="崩溃后自动重启" desc="程序意外退出时自动恢复运行。">
              <Toggle id="guardBtn" on={k.keep_alive} label="崩溃后自动重启" onToggle={() => void kvmKeepAlive(!k.keep_alive)} />
            </Row>
            <Row title="副机模式" desc="给没接键鼠的电脑一次配好以上三项。请先固定 IP。">
              <button
                id="subBtn"
                className={`small-button${subOn ? " is-on" : ""}`}
                onClick={() => void submachine(!subOn)}
              >
                {subOn ? (
                  <>
                    <Icon name="check" />
                    已开启
                  </>
                ) : (
                  "一键开启"
                )}
              </button>
            </Row>
          </>
        )}

        {tab === "appearance" && (
          <>
            <Row title="外观" desc="选择更适合当前工作环境的明暗。">
              <div className="segmented" role="group" aria-label="外观">
                {(
                  [
                    ["light", "浅色"],
                    ["dark", "深色"],
                    ["system", "跟随系统"],
                  ] as const
                ).map(([v, label]) => (
                  <button key={v} className={theme === v ? "selected" : ""} aria-pressed={theme === v} onClick={() => setTheme(v)}>
                    {label}
                  </button>
                ))}
              </div>
            </Row>
            <Row title="减少动态效果" desc="保留状态反馈，减少连接动画和过渡。">
              <Toggle id="motionBtn" on={reduceMotion} label="减少动态效果" onToggle={toggleReduceMotion} />
            </Row>
            <Row title="软件更新" desc={`当前 v${snap.update.current}。查看新版本和更新说明。`}>
              <button className="small-button" onClick={openUpdate}>
                {snap.update.latest && snap.update.latest !== snap.update.current ? (
                  <>
                    <Icon name="download" />
                    v{snap.update.latest}
                  </>
                ) : (
                  "检查更新"
                )}
              </button>
            </Row>
            <Row title="配置与日志" desc="配置文件和日志在同一个目录里，排查问题时用得着。">
              <button className="small-button" onClick={() => void openConfigDir()}>
                <Icon name="folder" />
                查看记录
              </button>
            </Row>
            <Row title="显示器" desc="本机当前检测到的显示器型号。">
              <span className="tag">{snap.display || "—"}</span>
            </Row>
            <Row title="设备标识" desc="配对时用来区分本机的固定标识，两台各不相同。">
              <span className="tag">{snap.identity || "—"}</span>
            </Row>
          </>
        )}
      </div>

      <p className="settings-footnote">
        <Icon name="info" />
        {tab === "sharing"
          ? "紧急断开：Ctrl + Alt + Esc。光标速度变化会短暂重连。"
          : tab === "startup"
            ? "自动恢复不会一开机就抢键鼠：等对端上线后才接，失败会自动重试。"
            : "改完立即生效，不需要重启程序。"}
      </p>
    </>
  );
}

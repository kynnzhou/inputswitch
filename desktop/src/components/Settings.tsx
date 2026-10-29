import { useState } from "react";
import type { Snapshot } from "../lib/types";
import { Icon } from "./IconSprite";
import {
  kvmAutoResume,
  kvmKeepAlive,
  openConfigDir,
  recHotkey,
  resetHotkey,
  setKvmScale,
  submachine,
  toggleAutostart,
} from "../lib/api";
import type { ThemeChoice } from "../hooks/useTheme";

type Tab = "shortcut" | "startup" | "appearance";

export function Settings({
  snap,
  theme,
  setTheme,
  reduceMotion,
  toggleReduceMotion,
}: {
  snap: Snapshot | null;
  theme: ThemeChoice;
  setTheme: (t: ThemeChoice) => void;
  reduceMotion: boolean;
  toggleReduceMotion: () => void;
}) {
  const [tab, setTab] = useState<Tab>("shortcut");
  if (!snap) return <section className="page-view settings-page" aria-label="设置" hidden />;

  const k = snap.kvm;
  const subOn = !!k.auto_resume && !!k.keep_alive && snap.autostart;
  const scale = Number(k.recv_scale || 1);

  return (
    <section className="page-view settings-page" aria-label="设置">
      <div className="settings-tabs" role="tablist" aria-label="设置类别">
        {(
          [
            ["shortcut", "快捷键"],
            ["startup", "启动与恢复"],
            ["appearance", "外观与关于"],
          ] as const
        ).map(([id, label]) => (
          <button
            key={id}
            id={`tab-${id}`}
            className={tab === id ? "selected" : ""}
            role="tab"
            aria-selected={tab === id}
            onClick={() => setTab(id)}
          >
            {label}
          </button>
        ))}
      </div>

      <div className="settings-panel" id="shortcutPanel" role="tabpanel" hidden={tab !== "shortcut"}>
        <div className="setting-row">
          <div className="setting-text">
            <strong>切屏快捷键</strong>
            <p>随时把画面切到另一个输入源。点「改键」后，到主程序弹出的窗口里按组合键。</p>
          </div>
          <div className="setting-ctrl">
            <span className="setting-value">{snap.hotkey || "—"}</span>
            <button className="btn-sm" onClick={() => void recHotkey()}>
              改键
            </button>
            <button
              className="btn-sm ghost"
              title="恢复默认组合键 Ctrl + Alt + S"
              onClick={() => void resetHotkey()}
            >
              恢复默认
            </button>
          </div>
        </div>
        <div className="setting-row">
          <div className="setting-text">
            <strong>热键目标</strong>
            <p>快捷键一下切到哪个输入源。</p>
          </div>
          <div className="setting-ctrl">
            <span className="setting-value">{snap.target || "未设置"}</span>
          </div>
        </div>
        <div className="setting-row">
          <div className="setting-text">
            <strong>光标速度</strong>
            <p>仅在键鼠交给对端时生效。两个系统的指针加速曲线不同 ——「回到本机变慢」就往上调一档。</p>
          </div>
          <div className="setting-ctrl">
            <div id="kvmScaleRow">
              {[0.9, 1, 1.1, 1.2].map((v) => (
                <button
                  key={v}
                  className={Math.abs(v - scale) < 0.001 ? "sel" : ""}
                  onClick={() => void setKvmScale(v)}
                >
                  {v.toFixed(1)}
                </button>
              ))}
            </div>
          </div>
        </div>
        <div className="hint">当前 ×{scale.toFixed(1)}。改档后端会重建共用键鼠实例，会话会断一下。</div>
        <p className="settings-footnote">
          紧急断开键鼠：<kbd>Ctrl</kbd>
          <kbd>Alt</kbd>
          <kbd>Esc</kbd>，任何时候都生效，不用打开面板
        </p>
      </div>

      <div className="settings-panel" id="startupPanel" role="tabpanel" hidden={tab !== "startup"}>
        {(
          [
            ["autoBtn", "开机自启", "登录电脑后自动运行 InputSwitch。", snap.autostart, () => void toggleAutostart(!snap.autostart)],
            [
              "resumeBtn",
              "对端上线后自动恢复共用键鼠",
              "不会一开机就抢键鼠：等对端上线后才接，失败会自动重试。",
              k.auto_resume,
              () => void kvmAutoResume(!k.auto_resume),
            ],
            [
              "guardBtn",
              "崩溃后自动重启",
              "程序意外退出时自动恢复运行。",
              k.keep_alive,
              () => void kvmKeepAlive(!k.keep_alive),
            ],
          ] as const
        ).map(([id, label, desc, on, act]) => (
          <div className="setting-row" key={id}>
            <div className="setting-text">
              <strong>{label}</strong>
              <p>{desc}</p>
            </div>
            <button
              id={id}
              className={`sw${on ? " on" : ""}`}
              role="switch"
              aria-checked={on}
              aria-label={label}
              onClick={act}
            />
          </div>
        ))}
        <div className="hint">
          {k.auto_resume
            ? "自动恢复不会一开机就抢键鼠：等对端上线后才接，失败会自动重试。"
            : "自动恢复已关闭：每次启动都要手动点一次「开启」。没接键鼠的那台建议打开。"}
        </div>
        <div className="setting-row">
          <div className="setting-text">
            <strong>副机模式</strong>
            <p>
              给<b>没接键鼠</b>的那台机器用：一次打开开机自启、自动恢复共用键鼠、崩溃后自动重启，并保持共用键鼠开启。
              开启前请把这台的 IP 固定下来 —— 共用键鼠按 IP 认机器，不认机器名。
            </p>
          </div>
          <button
            id="subBtn"
            className={`btn-sm${subOn ? " is-on" : ""}`}
            onClick={() => void submachine(!k.auto_resume)}
          >
            {subOn ? "副机模式：已开启" : "副机模式：一键开启"}
          </button>
        </div>
      </div>

      <div className="settings-panel" id="appearancePanel" role="tabpanel" hidden={tab !== "appearance"}>
        <div className="setting-row">
          <div className="setting-text">
            <strong>外观</strong>
            <p>选择更适合当前工作环境的明暗。「跟随系统」跟着系统的深浅色设置走。</p>
          </div>
          <div className="segmented" role="group" aria-label="外观">
            {(
              [
                ["light", "浅色"],
                ["dark", "深色"],
                ["system", "跟随系统"],
              ] as const
            ).map(([v, label]) => (
              <button
                key={v}
                className={theme === v ? "selected" : ""}
                aria-pressed={theme === v}
                onClick={() => setTheme(v)}
              >
                {label}
              </button>
            ))}
          </div>
        </div>
        <div className="setting-row">
          <div className="setting-text">
            <strong>减少动态效果</strong>
            <p>保留状态反馈，减少连接动画和页面过渡。</p>
          </div>
          <button
            id="motionBtn"
            className={`sw${reduceMotion ? " on" : ""}`}
            role="switch"
            aria-checked={reduceMotion}
            aria-label="减少动态效果"
            onClick={toggleReduceMotion}
          />
        </div>
        <div className="setting-row">
          <div className="setting-text">
            <strong>显示器</strong>
            <p>本机当前检测到的显示器型号。</p>
          </div>
          <div className="setting-ctrl">
            <span className="setting-value">{snap.display || "—"}</span>
          </div>
        </div>
        <div className="setting-row">
          <div className="setting-text">
            <strong>设备标识</strong>
            <p>配对时用来区分本机的固定标识，两台机器各不相同。</p>
          </div>
          <div className="setting-ctrl">
            <code>{snap.identity || "—"}</code>
          </div>
        </div>
        <div className="setting-row">
          <div className="setting-text">
            <strong>软件更新</strong>
            <p>查看新版本和更新说明，下载并安装。（更新弹窗下一批迁入，先在旧面板操作）</p>
          </div>
          <span className="setting-value">v{snap.update.current}</span>
        </div>
        <div className="setting-row">
          <div className="setting-text">
            <strong>配置与日志</strong>
            <p>配置文件和日志都在同一个目录里，排查问题时用得着。</p>
          </div>
          <button className="btn-sm" onClick={() => void openConfigDir()}>
            <Icon name="folder" />
            打开配置文件夹
          </button>
        </div>
      </div>
    </section>
  );
}

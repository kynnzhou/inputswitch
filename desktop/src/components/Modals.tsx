import { useState } from "react";
import type { Snapshot } from "../lib/types";
import { Modal, ModalHead } from "./Modal";
import { setLabel, setTarget, scanPorts, updateAction } from "../lib/api";

/** 帮助弹窗：快捷键表里的热键位是活的（跟着 snap.hotkey 走） */
export function HelpModal({ snap, onClose, onDiagnostics }: { snap: Snapshot; onClose: () => void; onDiagnostics: () => void }) {
  return (
    <Modal label="帮助" onClose={onClose}>
      <ModalHead title="让设备协同起来" onClose={onClose} />
      <div className="modal-body">
        <p>单屏切换和多屏共享可以一起使用。左侧四个页面只切换视图，各台设备仍按自己的布局工作。</p>
        <div className="help-row"><span>切换显示器输入源</span><span className="keys">{snap.hotkey || "—"}</span></div>
        <div className="help-row"><span>紧急断开键鼠共享</span><span className="keys">Ctrl + Alt + Esc</span></div>
        <div className="help-row"><span>摆放已选对端</span><span className="keys">方向键 / 拖动</span></div>
        <div className="help-row"><span>把对端移回未摆位</span><span className="keys">Delete</span></div>
        <div className="modal-note">方向键和 Delete 在点选一台对端后生效。切屏快捷键由主程序全局注册，面板不是当前窗口时也有效。</div>
        <div className="modal-actions">
          <button onClick={onClose}>知道了</button>
          <button className="ghost" onClick={onDiagnostics}>
            切换遇到问题？
          </button>
        </div>
      </div>
    </Modal>
  );
}

export function DiagnosticsModal({ onClose }: { onClose: () => void }) {
  return (
    <Modal label="排查" onClose={onClose}>
      <ModalHead title="切换遇到问题？" onClose={onClose} />
      <div className="modal-body">
        <p>画面在另一台电脑时，需要由对端发出切回指令。先确认对端在线，再检查显示器连接。</p>
        <div className="help-row"><span>显示器菜单</span><span className="keys">开启 DDC/CI</span></div>
        <div className="help-row"><span>显示器连线</span><span className="keys">优先 DP / HDMI 直连</span></div>
        <div className="help-row"><span>另一台电脑</span><span className="keys">开机并运行 InputSwitch</span></div>
        <div className="help-row"><span>双方配对码</span><span className="keys">完全一致</span></div>
        <div className="modal-note">笔记本内置屏幕通常不支持 DDC/CI。顶栏「检测能否切换」给出的是本机的实际检测结果，以它为准。</div>
        <div className="modal-actions">
          <button onClick={onClose}>返回工作台</button>
        </div>
      </div>
    </Modal>
  );
}

/** 扫描确认：扫描真的会逐个切输入口，先问一句再扫 */
export function ScanModal({ onClose, refresh, flash }: { onClose: () => void; refresh: () => void; flash: (t: string) => void }) {
  return (
    <Modal label="扫描显示器输入源" onClose={onClose}>
      <ModalHead title="扫描显示器输入口" onClose={onClose} />
      <div className="modal-body">
        <p>扫描会逐个尝试输入口，屏幕可能短暂变黑几次。请先保存正在编辑的内容。</p>
        <div className="modal-note">扫描结束后会试着把画面切回本机输入口；没能切回时面板会告诉你怎么手动切回来。局域网里没有对端在线时只汇总已知信息，不切屏。</div>
        <div className="modal-actions">
          <button onClick={onClose}>暂不扫描</button>
          <button
            className="primary"
            onClick={() => {
              onClose();
              flash("正在扫描，屏幕会黑几次…");
              void scanPorts().then(refresh);
            }}
          >
            开始扫描
          </button>
        </div>
      </div>
    </Modal>
  );
}

/** 管理输入源：改名（批量保存只发改过的）+ 设为热键。 */
export function PortsModal({
  snap,
  onClose,
  refresh,
  flash,
}: {
  snap: Snapshot;
  onClose: () => void;
  refresh: () => void;
  flash: (t: string) => void;
}) {
  const [edits, setEdits] = useState<Record<string, string>>({});
  const value = (name: string, label: string) => edits[name] ?? label;

  const save = async () => {
    const changed = snap.inputs.filter((i) => edits[i.name] !== undefined && edits[i.name] !== i.label);
    for (const i of changed) {
      await setLabel(i.name, edits[i.name]);
    }
    flash(changed.length ? `已保存 ${changed.length} 个名称` : "名称没有改动");
    refresh();
    onClose();
  };

  return (
    <Modal label="管理显示器输入源" onClose={onClose}>
      <ModalHead title="管理显示器输入源" onClose={onClose} />
      <div className="modal-body">
        <p>给输入口起一个好认的名字，或设为热键目标。</p>
        <div className="ports-list">
          {snap.inputs.length === 0 && (
            <div className="empty">没有检测到输入源。接好显示器后点顶部「重新检测」再读一次。</div>
          )}
          {snap.inputs.map((i) => {
            const dead = i.presence === "invalid";
            const meta = `0x${i.value.toString(16).toUpperCase().padStart(2, "0")} · ${
              dead ? "该口不存在" : i.presence === "occupied" ? "有主机" : i.presence === "idle" ? "无信号" : "状态未知"
            } · ${i.verified ? "本机确认" : "待确认"}`;
            const isTarget = !!snap.target && snap.target === i.label;
            return (
              <div className="port-row" key={i.value}>
                <div>
                  <input
                    type="text"
                    maxLength={24}
                    value={value(i.name, i.label)}
                    aria-label="输入口名称"
                    onChange={(e) => setEdits((s) => ({ ...s, [i.name]: e.target.value }))}
                  />
                  <small>{meta}</small>
                </div>
                {dead || i.current ? (
                  <button className="btn-sm" disabled title={i.current ? "这是当前输入源" : "显示器固件不接受这个输入口"}>
                    设为热键
                  </button>
                ) : (
                  <button
                    className="btn-sm"
                    onClick={() => {
                      void setTarget(i.value).then(refresh);
                      flash(`热键目标已设为 ${i.label}`);
                    }}
                  >
                    {isTarget ? "已设为热键" : "设为热键"}
                  </button>
                )}
              </div>
            );
          })}
        </div>
        {snap.port_note && <div className="hint" style={{ marginTop: 12 }}>上次检测：{snap.port_note}</div>}
        <div className="modal-actions">
          <button onClick={onClose}>取消</button>
          <button className="primary" onClick={() => void save()}>
            保存名称
          </button>
        </div>
      </div>
    </Modal>
  );
}

/**
 * 更新弹窗（骨架版）：按 snap.update.phase 现算内容。下载进度/倒计时全屏层
 * 的完整交互下一批接入 —— 现在覆盖可用/下载/就绪三档的按钮动作。
 */
export function UpdateModal({ snap, onClose, refresh, flash }: { snap: Snapshot; onClose: () => void; refresh: () => void; flash: (t: string) => void }) {
  const u = snap.update;
  const act = (a: "check" | "download" | "install" | "dismiss") => {
    void updateAction(a).then(refresh);
    if (a === "download") flash("开始下载…");
    if (a === "dismiss") onClose();
  };
  const title =
    u.phase === "downloading" ? "正在下载更新" :
    u.phase === "ready" ? "更新已就绪" :
    u.phase === "available" ? `发现新版本 ${u.latest}` :
    `当前已是最新版（v${u.current}）`;
  return (
    <Modal label="更新" onClose={onClose}>
      <ModalHead title={title} onClose={onClose} />
      <div className="modal-body">
        {u.latest && u.latest !== u.current && (
          <>
            <p>v{u.current} → v{u.latest}{u.pub_date && `（${u.pub_date}）`}</p>
            {u.notes && <div className="modal-note">{u.notes}</div>}
          </>
        )}
        {u.error && <p style={{ color: "var(--red)" }}>{u.error}</p>}
        {u.phase === "downloading" && u.total > 0 && (
          <div className="progress">
            <i style={{ width: `${Math.round((u.downloaded / u.total) * 100)}%` }} />
          </div>
        )}
        <div className="modal-actions">
          {(u.phase === "idle" || u.phase === "failed") && <button onClick={() => act("check")}>检查更新</button>}
          {u.phase === "available" && <button className="primary" onClick={() => act("download")}>下载更新</button>}
          {u.phase === "ready" && <button className="primary" onClick={() => act("install")}>重启并安装</button>}
          {u.latest && u.latest !== u.current && u.phase !== "downloading" && (
            <button className="ghost" onClick={() => act("dismiss")}>跳过此版本</button>
          )}
          <button className="ghost" onClick={onClose}>关闭</button>
        </div>
      </div>
    </Modal>
  );
}

import { useRef, useState } from "react";
import type { Snapshot } from "../lib/types";
import { Icon } from "./IconSprite";
import { savePair, kvmHandover } from "../lib/api";
import { PlacementGrid } from "./PlacementGrid";

/**
 * 设备与布局页（P1 版）：名册列 + 配对表单完整；布局 pane（3×3 摆位图拖拽
 * + 链路行）下一批迁入 —— 现在给一个占位说明，去旧面板（/）操作。
 */
export function Devices({
  snap,
  flash,
  refresh,
  selectedNode,
  onSelectNode,
  optimistic,
  setOptimistic,
}: {
  snap: Snapshot | null;
  flash: (t: string) => void;
  refresh: () => void;
  selectedNode: string | null;
  onSelectNode: (n: string | null) => void;
  optimistic: { node: string; side: string }[];
  setOptimistic: (o: { node: string; side: string }[]) => void;
}) {
  // 已配对时默认看摆位（旧面板 autoDevicePane 同逻辑）；没配对先教配对
  const [pane, setPane] = useState<"layout" | "pair">("pair");
  // 首次拿到快照时按配对态校正默认 pane（已配对 → 摆位；旧面板 autoDevicePane 同逻辑）
  const paneInit = useRef(false);
  if (!paneInit.current && snap) {
    paneInit.current = true;
    if (snap.paired && snap.peer_verified > 0) setPane("layout");
  }
  const [code, setCode] = useState("");
  const [codeDirty, setCodeDirty] = useState(false);

  if (!snap) return <section className="page-view" aria-label="设备与布局" hidden />;

  // 服务端码回填只在自己没动过输入框时做（codeDirty 套路，对照 render.js）
  const shownCode = codeDirty ? code : snap.pair_code || "";
  const pairHelp = !snap.paired
    ? "2–32 个字符，双方一致即可"
    : snap.peer_verified > 0
      ? "身份已互相验证"
      : "码已保存，等对端确认身份…";

  return (
    <section className="page-view" aria-label="设备与布局">
      <div className="devices-layout">
        <div className="device-list">
          <p className="list-label">这台电脑</p>
          <div className="device-row local-row">
            <Icon name="monitor" />
            <span>
              <strong>{snap.node}</strong>
              <small>本机</small>
            </span>
          </div>
          <p className="list-label">已知设备 · {snap.peers.length}</p>
          {snap.peers.length === 0 && (
            <div className="device-row">
              <Icon name="wifi" />
              <span>
                <strong>还没有发现其他机器</strong>
                <small>对端运行本程序后自动出现</small>
              </span>
            </div>
          )}
          {snap.peers.map((p) => (
            <div
              key={p.node}
              className={`device-row${selectedNode === p.node ? " sel" : ""}`}
              onClick={() => {
                setPane("layout");
                onSelectNode(selectedNode === p.node ? null : p.node);
              }}
            >
              <Icon name={p.kvm_on ? "monitor" : "laptop"} />
              <span>
                <strong>{p.node}</strong>
                <small>{p.kvm_on ? "共用键鼠已开" : "共用键鼠没开"}</small>
              </span>
              {p.paired && <span className="tag green">已配对</span>}
            </div>
          ))}
          <button className="add-device" onClick={() => setPane("pair")} title="连接新设备">
            <Icon name="plus" />
            连接新设备
          </button>
          <div className="pair-summary">
            <Icon name="shield" />
            <span>
              {snap.paired && snap.peer_verified > 0
                ? "已配对 · 已连接"
                : snap.paired
                  ? "配对码已填 · 等对端确认"
                  : "尚未配对"}
            </span>
            <button className="ghost" onClick={() => setPane("pair")}>
              管理配对
            </button>
          </div>
        </div>

        <div className="device-detail">
          <div id="deviceLayoutPane" hidden={pane !== "layout"}>
            <div className="detail-heading">
              <div>
                <h2>按桌面上的位置摆放</h2>
                <p>选一台设备，再点方向。支持拖动、方向键和撤销。</p>
              </div>
              <span className="tag plain">一台一格</span>
            </div>
            <PlacementGrid
              snap={snap}
              variant="grid"
              selected={selectedNode}
              onSelect={onSelectNode}
              optimistic={optimistic}
              setOptimistic={setOptimistic}
              flash={flash}
              refresh={refresh}
            />
            <div className="rowlabel">键鼠交接</div>
            <div id="kvmLinks">
              {snap.kvm.links.map((l) => (
                <div key={l.node} className="linkrow">
                  <span className="node">
                    <span className={`dot${l.alive ? " on" : ""}`} />
                    <span className="nm">{l.node}</span>
                  </span>
                  <span className="meta">
                    {l.side ? `${l.side === "right" ? "右侧" : l.side === "left" ? "左侧" : l.side === "top" ? "上方" : "下方"} · ${l.mode === "edge" ? "多屏共享" : "跟随画面"}` : "共用一块屏"}
                    {!l.alive && " · 不在线"}
                  </span>
                  {l.active ? (
                    <span className="held">键鼠在这台</span>
                  ) : (
                    <button className="btn-sm" onClick={() => void kvmHandover(l.node).then(refresh)}>
                      {snap.kvm.state === "键鼠已交给对端" ? "转给它" : "交给它"}
                    </button>
                  )}
                </div>
              ))}
              {snap.kvm.links.length === 0 && <div className="empty">还没有配过链路</div>}
            </div>
          </div>

          <div id="devicePairPane" hidden={pane !== "pair"}>
            <form
              className="pair-form"
              onSubmit={(e) => {
                e.preventDefault();
                if (!shownCode.trim()) return flash("先填一个配对码");
                setCodeDirty(false);
                void savePair(shownCode.trim()).then(() => flash("配对码已保存"));
              }}
            >
              <Icon name="link" />
              <h2>连接另一台电脑</h2>
              <p>两台电脑接入同一局域网并打开 InputSwitch，填入相同配对码。</p>
              <div>
                <label className="field-label" htmlFor="code">
                  配对码
                </label>
                <div className="input-line">
                  <input
                    id="code"
                    type="text"
                    maxLength={32}
                    placeholder="输入双方约定的配对码"
                    autoComplete="off"
                    spellCheck={false}
                    value={shownCode}
                    onChange={(e) => {
                      setCode(e.target.value);
                      setCodeDirty(true);
                    }}
                  />
                  <button
                    type="button"
                    onClick={() => {
                      setCode(String(Math.floor(Math.random() * 1000000)).padStart(6, "0"));
                      setCodeDirty(true);
                    }}
                  >
                    随机生成
                  </button>
                </div>
                <p className="field-help">{pairHelp}</p>
              </div>
              <div className="form-actions">
                <button type="submit" className="primary">
                  保存并连接
                </button>
                <button type="button" className="ghost" onClick={() => setPane("layout")}>
                  返回布局
                </button>
                {snap.paired && (
                  <button
                    type="button"
                    className="ghost"
                    onClick={() => {
                      setCodeDirty(false);
                      void savePair("").then(() => flash("已取消配对"));
                    }}
                  >
                    取消配对
                  </button>
                )}
              </div>
              <p className="field-help">两台填同一个码。码越长越安全；换码后两边的键鼠会重连一次。</p>
            </form>
          </div>
        </div>
      </div>
    </section>
  );
}

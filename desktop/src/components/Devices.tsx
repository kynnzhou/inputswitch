import { useRef, useState } from "react";
import type { Snapshot } from "../lib/types";
import { Icon } from "./IconSprite";
import { kvmHandover, savePair, sendFiles } from "../lib/api";
import { PlacementGrid } from "./PlacementGrid";

const SIDE_NAME: Record<string, string> = { right: "右侧", left: "左侧", top: "上方", bottom: "下方" };

// 设备与布局 = 设计稿 devices-layout：左名册列（本机 + 已知设备 + 连接新设备 +
// 配对摘要），右详情 pane（摆位图 / 配对表单二选一）。
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
  const paneInit = useRef(false);
  if (!paneInit.current && snap) {
    paneInit.current = true;
    if (snap.paired && snap.peer_verified > 0) setPane("layout");
  }
  const [code, setCode] = useState("");
  const [codeDirty, setCodeDirty] = useState(false);

  if (!snap) return <section className="page-view" aria-label="设备与布局" hidden />;

  const shownCode = codeDirty ? code : snap.pair_code || "";
  const pairHelp = !snap.paired
    ? "2–32 个字符，双方一致即可"
    : snap.peer_verified > 0
      ? "身份已互相验证"
      : "码已保存，等对端确认身份…";
  const selectedPeer = snap.peers.find((p) => p.node === selectedNode) || null;
  const selectedLink = snap.kvm.links.find((l) => l.node === selectedNode) || null;

  return (
    <>
      <div className="view-heading">
        <div>
          <h1>设备与布局</h1>
          <p>连接你的电脑，让位置和桌面上一样。</p>
        </div>
        <button className="small-button" onClick={() => setPane("pair")}>
          <Icon name="plus" />
          添加设备
        </button>
      </div>

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
            <button
              key={p.node}
              className={`device-row${selectedNode === p.node ? " selected" : ""}`}
              aria-pressed={selectedNode === p.node}
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
              <i className="status-dot" aria-hidden="true" />
            </button>
          ))}

          <button className="add-device" onClick={() => setPane("pair")} title="连接新设备">
            <Icon name="plus" />
            连接新设备
          </button>

          <div className="pair-summary">
            <Icon name="shield" />
            <span>
              {snap.paired && snap.peer_verified > 0
                ? "设备身份已验证"
                : snap.paired
                  ? "码已保存 · 等对端确认"
                  : "尚未配对"}
            </span>
            <button className="text-button" onClick={() => setPane("pair")}>
              管理配对 <Icon name="chevron" />
            </button>
          </div>
        </div>

        <div className="device-detail">
          {/* 两个 pane 只渲染当前那个，且直接挂在 .device-detail 下：
              设计稿的 .pair-form 靠 margin:auto 在 detail 列里居中，
              中间再垫一层非 flex 的 div 就会掉到左上角。 */}
          {pane === "layout" ? (
            <>
              <div className="detail-heading">
                <div>
                  <h2>按桌面上的位置摆放</h2>
                  <p>选一台设备，再点方向。支持拖动、方向键和撤销。</p>
                </div>
                <span className="tag">一台一格</span>
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

              <div className="selected-detail">
                <div>
                  <strong>{selectedPeer ? selectedPeer.node : "选择一台设备"}</strong>
                  <br />
                  {selectedPeer
                    ? `${selectedPeer.addr || "地址未知"} · ${selectedLink?.alive ? "在线" : "离线"}${
                        selectedLink?.side ? ` · ${SIDE_NAME[selectedLink.side]}` : selectedLink ? " · 共用一块屏" : ""
                      }`
                    : "查看连接信息、调整位置"}
                </div>
                {selectedPeer ? (
                  <div className="form-actions">
                    <button
                      className="text-button"
                      onClick={() => void kvmHandover(selectedPeer.node).then(() => flash(`已请 ${selectedPeer.node} 接管键鼠`))}
                      title="不切画面，直接把键鼠交给这台"
                    >
                      <Icon name="mouse" />
                      {snap.kvm.active === selectedPeer.node ? "键鼠在这台" : "交给它"}
                    </button>
                    <button className="text-button" onClick={() => void sendFiles(selectedPeer.node)}>
                      <Icon name="upload" />
                      发文件
                    </button>
                  </div>
                ) : (
                  <span className={`tag ${snap.paired ? "good" : "warn"}`}>{snap.paired ? "已配对" : "未配对"}</span>
                )}
              </div>
            </>
          ) : (
            <form
              className="pair-form"
              onSubmit={(e) => {
                e.preventDefault();
                if (!shownCode.trim()) return flash("先填一个配对码");
                setCodeDirty(false);
                void savePair(shownCode.trim()).then(() => {
                  flash("配对码已保存");
                  refresh();
                });
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
                <button type="submit" className="strong-button">
                  保存并连接
                </button>
                <button type="button" className="text-button" onClick={() => setPane("layout")}>
                  返回布局
                </button>
                {snap.paired && (
                  <button
                    type="button"
                    className="text-button"
                    onClick={() => {
                      setCodeDirty(false);
                      void savePair("").then(() => {
                        flash("已取消配对");
                        refresh();
                      });
                    }}
                  >
                    取消配对
                  </button>
                )}
              </div>
              <p className="field-help">两台填同一个码。码越长越安全；换码后两边的键鼠会重连一次。</p>
            </form>
          )}
        </div>
      </div>
    </>
  );
}

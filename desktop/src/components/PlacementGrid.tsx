import { useState } from "react";
import type { Snapshot } from "../lib/types";
import { collectLayout, type PlacedCard } from "../lib/pure";
import { Icon } from "./IconSprite";
import { setLink } from "../lib/api";

const SIDE_NAME: Record<string, string> = { right: "右侧", left: "左侧", top: "上方", bottom: "下方" };
const GRID_ROWS = [
  ["corner", "top", "corner"],
  ["left", "self", "right"],
  ["corner", "bottom", "corner"],
];
const CROSS_ROWS = [
  [null, "top", null],
  ["left", "self", "right"],
  [null, "bottom", null],
];

function cardLabel(c: PlacedCard): string {
  return c.fresh ? "未摆位" : c.side ? SIDE_NAME[c.side] : "共用一块屏";
}

/**
 * 设备卡片。点击 = 选中（再点方向格放下）；HTML5 拖放为辅（WKWebView 上
 * 不稳时点选放置照样可用 —— 旧面板为此全走 Pointer Events，React 版先双路，
 * P2 Tauri 壳验证后再看要不要补 pointer 拖拽）。
 */
function Card({
  c,
  selected,
  onSelect,
  onPlace,
}: {
  c: PlacedCard;
  selected: boolean;
  onSelect: () => void;
  onPlace: (side: string) => void;
}) {
  return (
    <div
      className={`screen${selected ? " sel" : ""}`}
      data-node={c.node}
      data-side={c.side}
      tabIndex={0}
      role="button"
      aria-pressed={selected}
      title={`${c.node} · ${cardLabel(c)}（点选后点方向放下，也可拖动）`}
      aria-label={`${c.node}，${cardLabel(c)}`}
      draggable
      onDragStart={(e) => e.dataTransfer.setData("text/plain", c.node)}
      onClick={onSelect}
      onKeyDown={(e) => {
        const dir = { ArrowLeft: "left", ArrowRight: "right", ArrowUp: "top", ArrowDown: "bottom" }[e.key];
        if (dir) {
          e.preventDefault();
          onPlace(dir);
        } else if (e.key === "Delete" || e.key === "Backspace") {
          e.preventDefault();
          onPlace("");
        }
      }}
    >
      <Icon name="monitor" />
      <span className={`dot${c.alive ? " on" : ""}`} />
      <span className="sname">{c.node}</span>
      {c.active && <span className="stag">键鼠在这台</span>}
      {c.dup && <span className="stag warn">放重了</span>}
      {c.fresh && <span className="stag new">未摆位</span>}
      {c.root && !c.active && <span className="stag root">起点</span>}
    </div>
  );
}

/**
 * 摆位图：设备页 3×3（variant="grid"）与工作台多屏十字五槽（variant="cross"）
 * 共用一套交互与数据归集（collectLayout 纯函数，vitest 钉住两处同构）。
 * 语义对照旧面板批2：点卡片选中 → 点空格放下（aim 高亮）；拖放为辅；
 * 方向键 / Delete 对选中卡片生效；同侧第二台标「放重了」。
 */
export function PlacementGrid({
  snap,
  variant,
  selected,
  onSelect,
  optimistic,
  setOptimistic,
  flash,
  refresh,
}: {
  snap: Snapshot;
  variant: "grid" | "cross";
  selected: string | null;
  onSelect: (node: string | null) => void;
  optimistic: { node: string; side: string }[];
  setOptimistic: (o: { node: string; side: string }[]) => void;
  flash: (t: string) => void;
  refresh: () => void;
}) {
  const [hint, setHint] = useState("");
  const layout = collectLayout(snap.kvm, snap.peers, optimistic);
  const dup = layout.loose.filter((x) => x.dup).map((x) => x.node);
  const fresh = layout.loose.filter((x) => x.fresh).map((x) => x.node);

  // 放置：发 /api/link，乐观层立即反映（回弹问题由 optimistic 解决），
  // 原格那台先撤下（后端"每台一份 side"，两台同侧要在前端拆）
  const place = (node: string, side: string) => {
    const prev = Object.entries(layout.bySide).find(([, c]) => c.node !== node && c.side === side);
    // 乐观层：放下立即生效（旧面板 placeOn 同款 —— 不做的话卡片先弹回旧位，
    // 等轮询回来再跳过去，手感像"放不进去"）。后端快照到位后此层被覆盖。
    setOptimistic([
      ...optimistic.filter((o) => o.node !== node && o.node !== prev?.[1].node),
      { node, side },
      ...(prev ? [{ node: prev[1].node, side: "" }] : []),
    ]);
    void (async () => {
      if (prev) await setLink(prev[1].node, "");
      await setLink(node, side);
      flash(`${node} ${side ? `已放到${SIDE_NAME[side]}` : "已放回共用一块屏"}`);
      refresh();
    })();
  };

  const onDropAt = (side: string) => (e: React.DragEvent) => {
    e.preventDefault();
    const node = e.dataTransfer.getData("text/plain");
    if (node) place(node, side);
  };

  const rows = (variant === "grid" ? GRID_ROWS : CROSS_ROWS).flat();

  const cell = (slot: string | null) => {
    if (slot === null) return <div key="blank" className="cell corner" style={{ borderStyle: "hidden", opacity: 0 }} />;
    if (slot === "self") {
      return (
        <div key="self" className="cell self" data-drop="self">
          本机<small>{snap.node || "—"}</small>
        </div>
      );
    }
    if (slot === "corner") {
      // 协议里只有上下左右四个门，四角永远不能放。留空比写「斜角不连」安静
      return (
        <div key={slot} className="cell corner" data-drop="corner">—</div>
      );
    }
    const taken = layout.bySide[slot];
    const aim = selected && !taken ? " aim" : "";
    return (
      <div
        key={slot}
        className={`cell${aim}`}
        data-drop={slot}
        onDragOver={(e) => e.preventDefault()}
        onDrop={onDropAt(slot)}
        onClick={() => {
          if (!selected) {
            setHint("先点一台对端选中它，再点方向放下。");
            return;
          }
          const node = selected;
          onSelect(null);
          place(node, slot);
        }}
      >
        <span className="sideslot">{SIDE_NAME[slot]}</span>
        {taken ? (
          <Card
            c={taken}
            selected={selected === taken.node}
            onSelect={() => onSelect(selected === taken.node ? null : taken.node)}
            onPlace={(side) => place(taken.node, side)}
          />
        ) : (
          <span className="dropzone">{selected ? "放到这里" : "拖到这里"}</span>
        )}
      </div>
    );
  };

  return (
    <>
      <div className={variant === "grid" ? "grid" : "topology"} role="group" aria-label="设备摆位图：点选设备后点方向放下，或拖动">
        {rows.map(cell)}
      </div>
      {variant === "grid" && (
        <div className="loosetray" data-drop="loose" onDragOver={(e) => e.preventDefault()} onDrop={onDropAt("")}>
          <div className="loosehead">共用一块屏 · 拖/点到这里：切画面时键鼠跟着走</div>
          <div className="loosebox">
            {layout.loose.length ? (
              layout.loose.map((c) => (
                <Card
                  key={c.node}
                  c={c}
                  selected={selected === c.node}
                  onSelect={() => onSelect(selected === c.node ? null : c.node)}
                  onPlace={(side) => place(c.node, side)}
                />
              ))
            ) : (
              <span className="dropzone">（没有待摆位的对端）</span>
            )}
          </div>
        </div>
      )}
      <div className="gridhint">
        {[
          "点一台对端选中它，再点方向放下；也可以直接拖动。一个格子只放一台。",
          fresh.length ? `还没摆位：${fresh.join("、")}。` : "",
          dup.length ? `⚠ ${dup.join("、")} 也摆在同一侧，推过去会落到哪台不确定。` : "",
          hint,
        ]
          .filter(Boolean)
          .join(" ")}
      </div>
    </>
  );
}

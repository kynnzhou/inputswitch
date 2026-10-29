import { useState } from "react";
import type { Snapshot } from "../lib/types";
import { collectLayout } from "../lib/pure";
import { Icon } from "./IconSprite";
import { setLink } from "../lib/api";

const SIDE_NAME: Record<string, string> = { right: "右侧", left: "左侧", top: "上方", bottom: "下方" };
// 设计稿的摆位图是「十字五槽」：上下左右 + 本机居中（CSS 用 grid-area 定位，
// DOM 顺序不影响落位，所以这里按 上/左/本机/右/下 排，读屏顺序也顺）。
const SLOTS = ["up", "left", "self", "right", "down"] as const;
const SIDE_OF: Record<string, string> = { up: "top", down: "bottom", left: "left", right: "right" };

/**
 * 摆位图：设备页与工作台多屏共用一套交互与数据归集（collectLayout 纯函数，
 * vitest 钉住两处同构）。语义：点一格选中 → 点方向放下（aim 高亮）；拖放为辅；
 * 方向键 / Delete 对选中项生效；同一侧放第二台标「放重了」。
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
  const fresh = layout.loose.filter((x) => x.fresh && !x.dup).map((x) => x.node);
  const follows = layout.loose.filter((x) => !x.fresh && !x.dup);

  // 放置：发 /api/link，乐观层立即反映（不然卡片先弹回旧位再跳过去，像放不进去），
  // 原格那台先撤下（后端"每台一份 side"，两台同侧要在前端拆）
  const place = (node: string, side: string) => {
    const prev = Object.entries(layout.bySide).find(([, c]) => c.node !== node && c.side === side);
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

  const onSlotKey = (e: React.KeyboardEvent, node: string) => {
    const dir = { ArrowLeft: "left", ArrowRight: "right", ArrowUp: "top", ArrowDown: "bottom" }[e.key];
    if (dir) {
      e.preventDefault();
      place(node, dir);
    } else if (e.key === "Delete" || e.key === "Backspace") {
      e.preventDefault();
      place(node, "");
    }
  };

  const slot = (dir: (typeof SLOTS)[number]) => {
    if (dir === "self") {
      return (
        <div className="topology-slot self slot-self" key="self">
          <Icon name="monitor" />
          <strong>{snap.node || "本机"}</strong>
          <small>本机</small>
        </div>
      );
    }
    const side = SIDE_OF[dir];
    const taken = layout.bySide[side];
    const cls = `topology-slot slot-${dir}${taken ? " occupied" : selected ? " aim" : ""}${
      taken && selected === taken.node ? " selected" : ""
    }`;
    return (
      <button
        key={dir}
        className={cls}
        data-drop={side}
        onDragOver={(e) => e.preventDefault()}
        onDrop={onDropAt(side)}
        onClick={() => {
          if (taken) {
            onSelect(selected === taken.node ? null : taken.node);
            return;
          }
          if (!selected) {
            setHint("先点一台对端选中它，再点方向放下。");
            return;
          }
          const node = selected;
          onSelect(null);
          place(node, side);
        }}
        onKeyDown={(e) => taken && selected === taken.node && onSlotKey(e, taken.node)}
        aria-label={taken ? `${taken.node}，位于${SIDE_NAME[side]}，点击选择，可拖动` : `放到${SIDE_NAME[side]}`}
      >
        {taken ? (
          <>
            <Icon name="monitor" />
            <strong>{taken.node}</strong>
            <small>{taken.active ? "键鼠在这台" : taken.root ? "起点" : "已连接"}</small>
          </>
        ) : (
          <>
            <Icon name={selected ? "plus" : "screens"} />
            <strong>{SIDE_NAME[side]}</strong>
            <small>{selected ? "点击放置" : "放置设备"}</small>
          </>
        )}
      </button>
    );
  };

  return (
    <>
      <div className="topology" role="group" aria-label="设备摆位图：选择设备后，可点方向或用方向键移动">
        {SLOTS.map(slot)}
      </div>

      {variant === "grid" && (
        <>
          <div
            className={`shared-screen-tray${selected ? " aim" : ""}`}
            data-drop="follow"
            onDragOver={(e) => e.preventDefault()}
            onDrop={onDropAt("")}
          >
            <span>共用一块屏 · 键鼠跟随</span>
            {follows.length ? (
              follows.map((c) => (
                <button
                  key={c.node}
                  className="small-button"
                  data-peer={c.node}
                  draggable
                  aria-pressed={selected === c.node}
                  onDragStart={(e) => e.dataTransfer.setData("text/plain", c.node)}
                  onClick={() => onSelect(selected === c.node ? null : c.node)}
                >
                  <Icon name="monitor" />
                  {c.node}
                </button>
              ))
            ) : (
              <button
                className="small-button"
                onClick={() => {
                  if (!selected) return setHint("先点一台对端，再点这里让它跟随画面。");
                  const node = selected;
                  onSelect(null);
                  place(node, "");
                }}
              >
                {selected ? "放到这里" : "点此放置"}
              </button>
            )}
          </div>

          {layout.loose.some((c) => c.fresh || c.dup) && (
            <div className="shared-screen-tray loose" data-drop="loose" onDragOver={(e) => e.preventDefault()}>
              <span>未摆位</span>
              {layout.loose
                .filter((c) => c.fresh || c.dup)
                .map((c) => (
                  <button
                    key={c.node}
                    className="small-button"
                    data-peer={c.node}
                    title={`${c.node} · ${c.dup ? "和别的对端摆在同一侧" : "还没摆位"}`}
                    onClick={() => onSelect(selected === c.node ? null : c.node)}
                  >
                    {c.node}
                    <span className="tag warn">{c.dup ? "放重了" : "未摆位"}</span>
                  </button>
                ))}
            </div>
          )}
        </>
      )}

      {variant === "grid" && (
        <p className="placement-note">
          <Icon name="info" />
          {[
            selected ? `已选 ${selected} · 点方向放置，Delete 移回未摆位` : "",
            fresh.length ? `还没摆位：${fresh.join("、")}。` : "",
            dup.length ? `${dup.join("、")} 也摆在同一侧，推过去会落到哪台不确定。` : "",
            hint,
          ]
            .filter(Boolean)
            .join(" ") || "共用一块屏与多屏布局可以同时使用。"}
        </p>
      )}
    </>
  );
}

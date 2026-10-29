import type { InputView, Snapshot } from "./types";

/// "下一个能切过去的"口：主按钮用它挑目标（对照 render.js nextTarget）。
/// 优先级：有主机 > 无信号 > 待确认；「该口不存在」和当前口都不算。
/// 只决定主按钮，不参与输入口行样式 —— 实心蓝只留给顶栏那一个主按钮。
export function nextTarget(inputs: InputView[]): InputView | null {
  const cand = inputs.filter((i) => !i.current && i.presence !== "invalid");
  return (
    cand.find((i) => i.presence === "occupied") ||
    cand.find((i) => i.presence === "idle") ||
    cand[0] ||
    null
  );
}

/// 这次切屏会不会连键鼠一起走：问后端（will_follow = 有共用屏链路 && 能抓取），
/// 前端不自己推 —— 旧面板曾拿 mode+role 自己推过，判据跟 do_shift 打架。
export function kvmFollowing(s: Snapshot): boolean {
  return !!s.kvm.will_follow;
}

/// 摆位归集（对照 kvm.js renderKvmGrid 的 links/peers → bySide/loose 归集）。
/// 设备页 3×3 图和工作台十字图共用 —— 两处必须同构，同一台对端位置才不打架。
export interface PlacedCard {
  node: string;
  side: string;
  alive: boolean;
  active: boolean;
  root: boolean;
  fresh: boolean; // 已发现但还没配链路（拖进格子 = 建链路）
  dup: boolean; // 和别的对端摆了同一侧
}

export interface Layout {
  bySide: Record<string, PlacedCard>;
  loose: PlacedCard[];
}

export function collectLayout(
  kvm: { links: { node: string; side: string; alive: boolean; active: boolean }[]; root: string },
  peers: { node: string }[],
  optimistic: { node: string; side: string }[] = [],
): Layout {
  // 乐观层先落（拖完立即生效，等后端快照回来覆盖）
  const optOf = (node: string) => optimistic.find((o) => o.node === node);
  const links = kvm.links.map((l) => {
    const o = optOf(l.node);
    return o ? { ...l, side: o.side } : l;
  });
  const rootNode = kvm.root || "";
  const bySide: Record<string, PlacedCard> = {};
  const loose: PlacedCard[] = [];
  const dup: string[] = [];
  const known = new Set<string>();
  links.forEach((l) => {
    known.add(l.node);
    const item: PlacedCard = {
      node: l.node, side: l.side || "", alive: !!l.alive, active: !!l.active,
      root: rootNode === l.node, fresh: false, dup: false,
    };
    if (!l.side) loose.push(item);
    else if (bySide[l.side]) { dup.push(l.node); loose.push({ ...item, dup: true }); }
    else bySide[l.side] = item;
  });
  // 乐观新建的（后端链路表还没有它）
  optimistic.forEach((o) => {
    if (!known.has(o.node) && o.side) {
      known.add(o.node);
      const prev = bySide[o.side];
      if (prev) { dup.push(o.node); loose.push({ node: o.node, side: "", alive: true, active: false, root: false, fresh: true, dup: true }); }
      else bySide[o.side] = { node: o.node, side: o.side, alive: true, active: false, root: false, fresh: false, dup: false };
    } else if (!known.has(o.node) && !o.side) {
      known.add(o.node);
      loose.push({ node: o.node, side: "", alive: true, active: false, root: false, fresh: true, dup: false });
    }
  });
  peers.forEach((p) => {
    if (!known.has(p.node)) {
      loose.push({ node: p.node, side: "", alive: true, active: false, root: rootNode === p.node, fresh: true, dup: false });
    }
  });
  (globalThis as Record<string, unknown>).__lastDup = dup;
  return { bySide, loose };
}

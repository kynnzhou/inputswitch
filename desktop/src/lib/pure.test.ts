import { describe, expect, it } from "vitest";
import { nextTarget } from "./pure";
import type { InputView } from "./types";

const port = (over: Partial<InputView>): InputView => ({
  name: "x",
  label: "X",
  value: 1,
  current: false,
  presence: "unknown",
  verified: false,
  ...over,
});

/// 铁律迁移第一批（P1 收尾补全）：主按钮目标选择的优先级。
/// 有主机 > 无信号 > 待确认；「该口不存在」和当前口永远不算候选。
describe("nextTarget（主按钮挑目标）", () => {
  it("有主机优先于无信号与待确认", () => {
    const t = nextTarget([
      port({ value: 1, presence: "idle" }),
      port({ value: 2, presence: "occupied" }),
      port({ value: 3, presence: "unknown" }),
    ]);
    expect(t?.value).toBe(2);
  });
  it("没有有主机时退到无信号", () => {
    const t = nextTarget([port({ value: 1, presence: "unknown" }), port({ value: 2, presence: "idle" })]);
    expect(t?.value).toBe(2);
  });
  it("「该口不存在」和当前口不是候选；全被排除时返回 null（主按钮进 is-off 态）", () => {
    expect(nextTarget([port({ value: 1, presence: "invalid" }), port({ value: 2, current: true })])).toBeNull();
  });
});

import { collectLayout } from "./pure";

describe("collectLayout（摆位归集）", () => {
  const kvm = (links: { node: string; side: string }[], root = "") =>
    ({ links: links.map((l) => ({ ...l, alive: true, active: false })), root }) as never;

  it("有门链路进格子，无门进托盘，同侧第二台标 dup 落托盘", () => {
    const l = collectLayout(kvm([{ node: "A", side: "right" }, { node: "B", side: "" }, { node: "C", side: "right" }]), []);
    expect(l.bySide.right.node).toBe("A");
    expect(l.loose.map((x) => x.node)).toEqual(["B", "C"]);
    expect(l.loose[1].dup).toBe(true);
  });
  it("没配链路的对端进托盘且标 fresh（未摆位，不是「共用一块屏」）", () => {
    const l = collectLayout(kvm([]), [{ node: "NEW" }]);
    expect(l.loose[0].fresh).toBe(true);
  });
  it("乐观层立即生效：B 拖进 left 后不等后端快照就出现在左格", () => {
    const l = collectLayout(kvm([{ node: "B", side: "" }]), [], [{ node: "B", side: "left" }]);
    expect(l.bySide.left?.node).toBe("B");
    expect(l.loose).toHaveLength(0);
  });
});

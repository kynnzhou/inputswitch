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

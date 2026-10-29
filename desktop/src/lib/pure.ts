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

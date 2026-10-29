// @vitest-environment jsdom
// 铁律组件测试第一批（347 条 panelcheck 的核心子集，P2/P3 继续补全）。
// 渲染真实组件 + fixture 快照，断言「用户看得见的规则」，不是字符串匹配。
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach } from "vitest";
import { describe, expect, it } from "vitest";
import { Topbar } from "./Topbar";
import { Workspace } from "./Workspace";
import { PlacementGrid } from "./PlacementGrid";
import { Settings } from "./Settings";
import { makeSnap } from "../lib/fixtures";

const noop = () => {};
const flash = noop;

// jsdom 的 document 跨测试共享：不清理的话 getElementById 会拿到上一次
// render 的同名节点（第一个失败的根因）。
afterEach(cleanup);

describe("铁律：主按钮（永远显示，永不 disabled）", () => {
  it("有可切目标时显示「切到 X」且不 disabled、不挂 is-off", () => {
    const snap = makeSnap();
    render(<Topbar snap={snap} flash={flash} refresh={noop} openScan={noop} openShortcut={noop} />);
    const main = screen.getByText(/切到 DP/).closest("button")!;
    expect(main.id).toBe("mainSwitch");
    expect(main.className).toContain("primary");
    expect(main.className).not.toContain("is-off");
    expect((main as HTMLButtonElement).disabled).toBe(false);
  });

  it("画面在对面：文案变「请对端切回」，仍不 disabled", () => {
    const snap = makeSnap({ has_display: false });
    render(<Topbar snap={snap} flash={flash} refresh={noop} openScan={noop} openShortcut={noop} />);
    const main = screen.getByText(/把画面切回本机/).closest("button")!;
    expect((main as HTMLButtonElement).disabled).toBe(false);
  });

  it("没有可切的口：挂 is-off 但仍可点（点下去由 flash 说原因）", () => {
    const snap = makeSnap({ inputs: makeSnap().inputs.filter((i) => i.current) });
    render(<Topbar snap={snap} flash={flash} refresh={noop} openScan={noop} openShortcut={noop} />);
    const main = screen.getByText("没有其他输入口").closest("button")!;
    expect(main.className).toContain("is-off");
    expect((main as HTMLButtonElement).disabled).toBe(false);
  });

  it("维护三钮随 maint_busy 禁用（主按钮不受它管）", () => {
    const snap = makeSnap({ maint_busy: true });
    render(<Topbar snap={snap} flash={flash} refresh={noop} openScan={noop} openShortcut={noop} />);
    for (const id of ["maintRescan", "maintScan", "maintCheck"]) {
      expect((document.getElementById(id) as HTMLButtonElement).disabled).toBe(true);
    }
    expect((document.getElementById("mainSwitch") as HTMLButtonElement).disabled).toBe(false);
  });
});

describe("铁律：实心蓝只留给主按钮", () => {
  it("工作台渲染物里除 mainSwitch 外没有 primary 按钮", () => {
    const snap = makeSnap();
    const { container } = render(
      <Workspace snap={snap} flash={flash} onNavigate={noop} selectedNode={null} onSelectNode={noop} optimistic={[]} setOptimistic={noop} refresh={noop} openPorts={noop} />,
    );
    const primaries = [...container.querySelectorAll("button.primary")];
    expect(primaries).toHaveLength(0);
  });
});

describe("摆位图", () => {
  it("有门链路进格子（右侧），无门/未配进托盘且标「未摆位」不冒充共用屏", () => {
    const snap = makeSnap({
      peers: [
        ...makeSnap().peers,
        { node: "NEW", addr: "192.168.0.41", input: "", label: "", value: 0, paired: false, knows_port: false, needs_pair: false, kvm_on: false },
      ],
    });
    const { container } = render(
      <PlacementGrid snap={snap} variant="grid" selected={null} onSelect={noop} optimistic={[]} setOptimistic={noop} flash={noop} refresh={noop} />,
    );
    const rightCell = container.querySelector('[data-drop="right"]') as HTMLElement;
    expect(within(rightCell).getByText("WIN")).toBeTruthy();
    const loose = container.querySelector(".shared-screen-tray.loose") as HTMLElement;
    expect(within(loose).getByText("NEW")).toBeTruthy();
    expect(loose.textContent).toContain("未摆位");
    expect(within(loose).queryByText("共用一块屏")).toBeNull();
  });

  it("选中一台后空方位格亮 aim，已占的不亮", () => {
    const snap = makeSnap(); // WIN 占 right
    const { container, rerender } = render(
      <PlacementGrid snap={snap} variant="grid" selected={null} onSelect={noop} optimistic={[]} setOptimistic={noop} flash={noop} refresh={noop} />,
    );
    rerender(<PlacementGrid snap={snap} variant="grid" selected="NEW" onSelect={noop} optimistic={[]} setOptimistic={noop} flash={noop} refresh={noop} />);
    const aimCells = [...container.querySelectorAll(".topology-slot.aim")].map((c) => (c as HTMLElement).dataset.drop);
    expect(aimCells.sort()).toEqual(["bottom", "left", "top"]);
  });

  it("「该口不存在」的输入源按钮 disabled（点了就是黑屏）", () => {
    const snap = makeSnap();
    render(<Workspace snap={snap} flash={flash} onNavigate={noop} selectedNode={null} onSelectNode={noop} optimistic={[]} setOptimistic={noop} refresh={noop} openPorts={noop} />);
    const dead = screen.getByText(/HDMI-2 · 该口不存在/).closest("button")!;
    expect((dead as HTMLButtonElement).disabled).toBe(true);
  });
});

describe("设置页开关", () => {
  it(".sw 开关的 aria-checked 与状态同步（读屏软件读它）", () => {
    const snap = makeSnap();
    render(<Settings snap={snap} theme="system" setTheme={noop} reduceMotion={false} toggleReduceMotion={noop} />);
    // 设置页按设计稿一次只渲染一个选项卡的内容，先切到「启动与恢复」
    fireEvent.click(screen.getByText("启动与恢复"));
    expect(document.getElementById("autoBtn")!.getAttribute("aria-checked")).toBe("true");
    expect(document.getElementById("resumeBtn")!.getAttribute("aria-checked")).toBe("true");
    expect(document.getElementById("guardBtn")!.getAttribute("aria-checked")).toBe("true");
  });

  it("三项全开时副机模式显示「已开启」并挂 is-on", () => {
    const snap = makeSnap();
    render(<Settings snap={snap} theme="system" setTheme={noop} reduceMotion={false} toggleReduceMotion={noop} />);
    fireEvent.click(screen.getByText("启动与恢复"));
    const sub = document.getElementById("subBtn")!;
    expect(sub.textContent).toContain("已开启");
    expect(sub.className).toContain("is-on");
  });

  it("光标速度当前档高亮（fixture 是 1.1）", () => {
    const snap = makeSnap();
    render(<Settings snap={snap} theme="system" setTheme={noop} reduceMotion={false} toggleReduceMotion={noop} />);
    const sel = document.querySelector("#kvmScaleRow .sel")!;
    expect(sel.textContent).toBe("1.1");
  });
});

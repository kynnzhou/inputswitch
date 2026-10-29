import type { Snapshot } from "../lib/types";
import type { PageId } from "../lib/ui";
import { Icon } from "./IconSprite";

// 侧栏 = 设计稿 .sidebar：工作空间标签 → 四页导航（带计数）→ 底部「使用帮助」+
// 本机卡片。窄于 950 时设计稿自动收成图标栏（CSS 容器查询，不在 JS 里判断）。
export function Sidebar({
  snap,
  page,
  onNavigate,
  onHelp,
}: {
  snap: Snapshot | null;
  page: PageId;
  onNavigate: (p: PageId) => void;
  onHelp: () => void;
}) {
  const peerCount = snap?.peers.length ?? 0;
  const pending = (snap?.transfers ?? []).filter((t) => t.phase === "sending" || t.phase === "paused").length;
  const k = snap?.kvm;
  const owner = k?.on && k.active ? `键鼠在 ${k.active}` : "键鼠已连接";

  const items: { id: PageId; label: string; icon: string; count?: number; transfer?: boolean }[] = [
    { id: "workspace", label: "工作台", icon: "grid" },
    { id: "devices", label: "设备与布局", icon: "screens", count: peerCount },
    { id: "transfers", label: "文件传输", icon: "transfer", count: pending, transfer: true },
    { id: "settings", label: "设置", icon: "settings" },
  ];

  return (
    <aside className="sidebar" aria-label="主导航">
      <div className="workspace-label">我的工作空间</div>
      <nav className="main-nav">
        {items.map((it) => (
          <button
            key={it.id}
            className={`nav-item${page === it.id ? " selected" : ""}`}
            id={`nav-${it.id}`}
            data-page={it.id}
            aria-current={page === it.id ? "page" : undefined}
            title={it.label}
            onClick={() => onNavigate(it.id)}
          >
            <Icon name={it.icon} />
            <span>{it.label}</span>
            {it.transfer ? (
              it.count ? (
                <span className="nav-count transfer-count" id="transferCount">
                  {it.count}
                </span>
              ) : null
            ) : it.count !== undefined ? (
              <span className="nav-count" id={it.id === "devices" ? "deviceCount" : undefined}>
                {it.count}
              </span>
            ) : null}
          </button>
        ))}
      </nav>
      <div className="sidebar-bottom">
        <button className="sidebar-help" onClick={onHelp} title="快捷键与使用帮助">
          <Icon name="help" />
          <span>使用帮助</span>
        </button>
        <div className="local-machine">
          <Icon name="monitor" />
          <div>
            <strong>{snap?.node || "—"}</strong>
            <span>本机 · {owner}</span>
          </div>
          <i className="status-dot" aria-hidden="true" />
        </div>
      </div>
    </aside>
  );
}

import type { Snapshot } from "../lib/types";
import type { PageId } from "../lib/ui";
import { Icon } from "./IconSprite";

export function Sidebar({
  snap,
  page,
  onNavigate,
}: {
  snap: Snapshot | null;
  page: PageId;
  onNavigate: (p: PageId) => void;
}) {
  const n = snap?.peers.length ?? 0;
  const items: { id: PageId; label: string; icon: string; count?: number }[] = [
    { id: "workspace", label: "工作台", icon: "grid" },
    { id: "devices", label: "设备与布局", icon: "screens", count: n },
    { id: "transfers", label: "文件传输", icon: "transfer" },
    { id: "settings", label: "设置", icon: "settings" },
  ];
  return (
    <aside className="sidebar" aria-label="主导航">
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
            {it.id === "devices" && <span className="nav-count">{it.count}</span>}
          </button>
        ))}
      </nav>
      <div className="sidebar-bottom">
        <div className="local-machine">
          <Icon name="windows" />
          <div>
            <strong>{snap?.node || "—"}</strong>
            <span>本机</span>
          </div>
          <i className="status-dot" aria-hidden="true" />
        </div>
      </div>
    </aside>
  );
}

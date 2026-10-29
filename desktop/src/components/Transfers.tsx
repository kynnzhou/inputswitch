import { useState } from "react";
import type { Snapshot, TaskView } from "../lib/types";
import { Icon } from "./IconSprite";
import { chooseInbox, openInbox, sendFiles, sendFolder, transferAction } from "../lib/api";

/** 字节数格式化（对照 render.js formatSize） */
export function formatSize(bytes: number): string {
  if (!bytes) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let v = bytes;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i += 1;
  }
  return `${v >= 100 || i === 0 ? Math.round(v) : v.toFixed(1)} ${units[i]}`;
}

function transferPercent(t: TaskView): number {
  if (t.phase === "done") return 100;
  if (!t.total) return 0;
  return Math.min(100, Math.round((t.sent / t.total) * 100));
}

function phaseText(t: TaskView): string {
  switch (t.phase) {
    case "sending":
      return "发送中";
    case "paused":
      return "已暂停";
    case "done":
      return "已完成";
    case "failed":
      return `失败：${t.error || "未知原因"}`;
    case "cancelled":
      return "已取消";
    default:
      return t.phase;
  }
}

export function Transfers({ snap, flash }: { snap: Snapshot | null; flash: (t: string) => void }) {
  const [filter, setFilter] = useState<"all" | "active" | "done">("all");
  const [recipient, setRecipient] = useState("");

  if (!snap) return <section className="page-view" aria-label="文件传输" hidden />;

  const peers = snap.peers;
  const target = recipient || peers[0]?.node || "";
  const list = snap.transfers.filter((t) =>
    filter === "all" ? true : filter === "active" ? t.phase === "sending" || t.phase === "paused" : t.phase === "done",
  );

  return (
    <section className="page-view" aria-label="文件传输">
      <div className="transfer-target">
        <label htmlFor="recipient">发送到</label>
        <select id="recipient" value={target} onChange={(e) => setRecipient(e.target.value)}>
          {peers.length === 0 && <option value="">（没有在线对端）</option>}
          {peers.map((p) => (
            <option key={p.node} value={p.node}>
              {p.node}
            </option>
          ))}
        </select>
        {peers.length > 1 && <span className="tag">多台在线，点名发送</span>}
      </div>

      <div className="drop-zone">
        <Icon name="upload" />
        <h2>把文件拖到这里</h2>
        <p>支持多文件和整个文件夹，重名文件不会覆盖</p>
        <div className="drop-actions">
          <button disabled={!peers.length} onClick={() => void sendFiles(target || undefined)}>
            选择文件
          </button>
          <button disabled={!peers.length} onClick={() => void sendFolder(target || undefined)}>
            选择文件夹
          </button>
        </div>
      </div>

      <div className="transfers-list">
        <div className="list-heading">
          <span>传输记录</span>
          <div className="filter-group" role="group" aria-label="筛选传输记录">
            {(["all", "active", "done"] as const).map((f) => (
              <button
                key={f}
                className={filter === f ? "selected" : ""}
                aria-pressed={filter === f}
                onClick={() => setFilter(f)}
              >
                {f === "all" ? "全部" : f === "active" ? "进行中" : "已完成"}
              </button>
            ))}
          </div>
        </div>
        {list.length === 0 && <div className="empty">还没有传输记录</div>}
        {list.map((t) => (
          <div key={t.id} className={`transfer-row phase-${t.phase}`}>
            <Icon name={t.phase === "done" ? "check-circle" : "file"} />
            <div className="transfer-main">
              <span className="transfer-name">
                {t.name} → {t.peer}
              </span>
              <span className="transfer-meta">
                {phaseText(t)}
                {t.total > 0 && ` · ${formatSize(t.sent)} / ${formatSize(t.total)}`}
              </span>
              <div className="progress">
                <i style={{ width: `${transferPercent(t)}%` }} />
              </div>
            </div>
            {(t.phase === "sending" || t.phase === "paused") && (
              <button
                className="btn-sm"
                onClick={() =>
                  void transferAction(t.id, t.phase === "sending" ? "pause" : "resume").catch((e) =>
                    flash(String(e)),
                  )
                }
              >
                {t.phase === "sending" ? "暂停" : "继续"}
              </button>
            )}
            {t.phase !== "done" && t.phase !== "cancelled" && (
              <button className="btn-sm ghost" onClick={() => void transferAction(t.id, "cancel")}>
                取消
              </button>
            )}
          </div>
        ))}
      </div>

      <div className="inbox-line">
        <Icon name="folder" />
        <span className="grow">接收目录：{snap.inbox || "—"}</span>
        <button className="btn-sm ghost" onClick={() => void chooseInbox()}>
          更改
        </button>
        <button className="btn-sm ghost" onClick={() => void openInbox()}>
          打开
        </button>
      </div>
    </section>
  );
}

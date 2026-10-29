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

const PAGE_SIZE = 4;

// 文件传输 = 设计稿 renderTransfers：页头（接收目录）→ 发送到 → 拖放区 →
// 传输记录（筛选 + 行 + 分页）→ 接收目录行。
export function Transfers({ snap, flash }: { snap: Snapshot | null; flash: (t: string) => void }) {
  const [filter, setFilter] = useState<"all" | "active" | "done">("all");
  const [recipient, setRecipient] = useState("");
  const [page, setPage] = useState(0);

  if (!snap) return <section className="page-view" aria-label="文件传输" hidden />;

  const peers = snap.peers;
  const target = recipient || peers[0]?.node || "";
  const list = snap.transfers.filter((t) =>
    filter === "all" ? true : filter === "active" ? t.phase === "sending" || t.phase === "paused" : t.phase === "done",
  );
  const pages = Math.max(1, Math.ceil(list.length / PAGE_SIZE));
  const current = Math.min(page, pages - 1);
  const rows = list.slice(current * PAGE_SIZE, (current + 1) * PAGE_SIZE);

  return (
    <>
      <div className="view-heading">
        <div>
          <h1>文件传输</h1>
          <p>文件与文件夹，都能直接交给另一台。</p>
        </div>
        <button className="small-button" onClick={() => void chooseInbox()}>
          <Icon name="folder" />
          接收目录
        </button>
      </div>

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
        <span className={`tag ${peers.length ? "good" : "warn"}`}>
          <Icon name={peers.length ? "shield" : "wifi"} />
          {peers.length ? "配对设备" : "对端离线"}
        </span>
      </div>

      <div className="drop-zone">
        <Icon name="upload" />
        <h2>把文件拖到这里，交给 {target || "对端"}</h2>
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
                onClick={() => {
                  setFilter(f);
                  setPage(0);
                }}
              >
                {f === "all" ? "全部" : f === "active" ? "进行中" : "已完成"}
              </button>
            ))}
          </div>
        </div>

        {rows.length === 0 && (
          <div className="list-empty">
            {filter === "active" ? "没有正在进行的传输。发送一个文件即可开始。" : "这里会显示你的传输记录。"}
          </div>
        )}

        {rows.map((t) => {
          const pct = transferPercent(t);
          return (
            <div key={t.id} className={`transfer-row phase-${t.phase}`} data-transfer-row={t.id}>
              <Icon name="file" />
              <div className="transfer-file">
                <strong title={t.name}>{t.name}</strong>
                <small>
                  {t.total ? `${formatSize(t.total)} · ` : ""}发往 {t.peer}
                </small>
              </div>
              <div className="transfer-status">
                {t.phase === "done" ? (
                  <span className="success">
                    <Icon name="check-circle" />
                    已完成
                  </span>
                ) : t.phase === "failed" ? (
                  <span className="danger">连接中断</span>
                ) : t.phase === "cancelled" ? (
                  <span>已取消</span>
                ) : (
                  <>
                    <span>
                      {t.phase === "paused" ? "已暂停" : "正在发送"} · {pct}%
                    </span>
                    <div
                      className="progress-track"
                      role="progressbar"
                      aria-label={`${t.name} 传输进度`}
                      aria-valuenow={pct}
                      aria-valuemin={0}
                      aria-valuemax={100}
                    >
                      <i style={{ "--progress": pct / 100 } as React.CSSProperties} />
                    </div>
                  </>
                )}
              </div>
              <div className="transfer-actions">
                {(t.phase === "sending" || t.phase === "paused") && (
                  <>
                    <button
                      className="icon-button"
                      aria-label={t.phase === "sending" ? "暂停传输" : "继续传输"}
                      onClick={() =>
                        void transferAction(t.id, t.phase === "sending" ? "pause" : "resume").catch((e) => flash(String(e)))
                      }
                    >
                      <Icon name={t.phase === "sending" ? "pause" : "play"} />
                    </button>
                    <button
                      className="icon-button"
                      aria-label="取消传输"
                      onClick={() => void transferAction(t.id, "cancel").catch((e) => flash(String(e)))}
                    >
                      <Icon name="close" />
                    </button>
                  </>
                )}
                {t.phase === "failed" && (
                  <button className="icon-button" aria-label="重试传输" onClick={() => void sendFiles(t.peer)}>
                    <Icon name="refresh" />
                  </button>
                )}
              </div>
            </div>
          );
        })}

        <div className="list-pagination">
          {pages > 1 ? (
            <>
              <span>
                {current + 1} / {pages}
              </span>
              <button disabled={current === 0} onClick={() => setPage(current - 1)}>
                上一页
              </button>
              <button disabled={current >= pages - 1} onClick={() => setPage(current + 1)}>
                下一页
              </button>
            </>
          ) : (
            <span>{list.length} 项记录</span>
          )}
        </div>
      </div>

      <div className="inbox-line">
        <Icon name="folder" />
        <span>接收至 {snap.inbox || "—"}</span>
        <button className="text-button" onClick={() => void openInbox()}>
          打开
        </button>
        <button className="text-button" onClick={() => void chooseInbox()}>
          更改
        </button>
      </div>
    </>
  );
}

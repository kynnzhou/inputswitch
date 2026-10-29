import { useEffect, useState } from "react";

/** 面板快照里我们暂时只关心这两个字段；P1 会换成完整 Snapshot 类型（对照 panel.rs）。 */
interface Snapshot0 {
  node: string;
  version: string;
}

export function App() {
  const [snap, setSnap] = useState<Snapshot0 | null>(null);
  const [err, setErr] = useState("");

  useEffect(() => {
    // 与旧面板同一条数据通道：同源 fetch /api/state（dev 下由 vite 代理）。
    fetch("/api/state", { cache: "no-store" })
      .then((r) => r.json())
      .then((s: Snapshot0) => setSnap(s))
      .catch(() => setErr("面板服务没连上（主程序没在跑？）"));
  }, []);

  return (
    <main style={{ fontFamily: "system-ui", padding: 24 }}>
      <h1>InputSwitch 控制台 · 新前端（P0）</h1>
      <p>
        {err
          ? err
          : snap
            ? `节点：${snap.node ?? "?"} · 版本：${snap.version ?? "?"}`
            : "读取状态中…"}
      </p>
      <p style={{ opacity: 0.6, fontSize: 13 }}>
        这是 Tauri 2 + React 迁移的地基页（占位）。旧面板仍在 <code>/</code>，本页在{" "}
        <code>/app/</code>；P1 起逐页迁入后切换。
      </p>
    </main>
  );
}

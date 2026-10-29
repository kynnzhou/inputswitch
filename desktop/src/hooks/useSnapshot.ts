import { useCallback, useEffect, useRef, useState } from "react";
import { fetchState } from "../lib/api";
import type { Snapshot } from "../lib/types";

/**
 * 面板状态轮询（对照旧面板 core.js 的 pull 语义）：
 * - 常态 1200ms 一拉（旧面板同值）；downloading / restarting 提到 250ms
 *   （对齐后端采样节奏，进度条才走得动）
 * - 文档不可见时停拍、恢复可见立刻拉一次再续拍 —— 面板窗口"关闭"实为
 *   隐藏（panelwin 的行为），没人看的时候别打 /api/state
 * - 失联保留最后一份快照（UI 显示失联横幅），不清空 —— 瞬断回来不闪空白
 */
export function useSnapshot(): {
  snap: Snapshot | null;
  offline: boolean;
  refresh: () => void;
} {
  const [snap, setSnap] = useState<Snapshot | null>(null);
  const [offline, setOffline] = useState(false);
  const snapRef = useRef<Snapshot | null>(null);
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const pull = useCallback(async () => {
    try {
      const s = await fetchState();
      snapRef.current = s;
      setSnap(s);
      setOffline(false);
    } catch {
      setOffline(true);
    }
  }, []);

  useEffect(() => {
    let stopped = false;
    const tick = async () => {
      await pull();
      if (stopped) return;
      const phase = snapRef.current?.update.phase;
      const interval = phase === "downloading" || phase === "restarting" ? 250 : 1200;
      timerRef.current = setTimeout(tick, interval);
    };
    const stop = () => {
      if (timerRef.current) {
        clearTimeout(timerRef.current);
        timerRef.current = null;
      }
    };
    const onVis = () => {
      if (document.visibilityState === "visible") {
        stop();
        void tick();
      } else {
        stop();
      }
    };
    document.addEventListener("visibilitychange", onVis);
    void tick();
    return () => {
      stopped = true;
      document.removeEventListener("visibilitychange", onVis);
      stop();
    };
  }, [pull]);

  return { snap, offline, refresh: pull };
}

import { useCallback, useEffect, useState } from "react";

// 主题三态 + 减少动态效果。对照旧面板 core.js 的 applyTheme / localStorage 键名
// （键名不变 —— 用户已存的偏好直接带过来）。防闪脚本在 index.html 首屏跑。
export type ThemeChoice = "light" | "dark" | "system";

export function useTheme() {
  const [theme, setThemeState] = useState<ThemeChoice>(() => {
    try {
      return (localStorage.getItem("inputswitch.panel.theme") as ThemeChoice) || "system";
    } catch {
      return "system";
    }
  });
  const [reduceMotion, setReduceMotionState] = useState(() => {
    try {
      return localStorage.getItem("inputswitch.panel.reduce-motion") === "1";
    } catch {
      return false;
    }
  });

  useEffect(() => {
    const dark =
      theme === "dark" ||
      (theme === "system" && matchMedia("(prefers-color-scheme: dark)").matches);
    document.documentElement.dataset.theme = dark ? "dark" : "light";
    try {
      localStorage.setItem("inputswitch.panel.theme", theme);
    } catch { /* 私隐模式等场景静默 */ }
  }, [theme]);

  useEffect(() => {
    document.documentElement.classList.toggle("reduced-motion", reduceMotion);
    try {
      localStorage.setItem("inputswitch.panel.reduce-motion", reduceMotion ? "1" : "0");
    } catch { /* 同上 */ }
  }, [reduceMotion]);

  const setTheme = useCallback((t: ThemeChoice) => setThemeState(t), []);
  const toggleReduceMotion = useCallback(() => setReduceMotionState((v) => !v), []);

  return { theme, setTheme, reduceMotion, toggleReduceMotion };
}

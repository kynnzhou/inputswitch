import React from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";

// P0 地基：这一页只是占位，证明「Vite 产物 → panel.rs 同源托管 → WebView」
// 整条链路通。P1 起按四页（工作台/设备/传输/设置）替换为真实组件。
const el = document.getElementById("root");
if (!el) throw new Error("#root 不存在");
createRoot(el).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);

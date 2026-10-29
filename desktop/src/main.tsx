import React from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";

// 面板窗口本身就是设计稿里的 .app-window：加 body.embed 让设计稿原样的
// 「内嵌模式」规则生效（整窗铺满、无圆角阴影）。样式基座见 styles/panel.css。
document.body.classList.add("embed");

const el = document.getElementById("root");
if (!el) throw new Error("#root 不存在");
createRoot(el).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);

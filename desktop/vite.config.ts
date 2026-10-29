import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// dev server 起在 5173，/api 与 /panel.css 等代理到本机面板服务，
// 前端开发时与生产同源同路由，零 CORS。
// 生产构建产物由 tools/copy-panel-dist.mjs 拷进 assets/panel-dist/，
// 由 panel.rs 同源托管（见 docs/tauri-react-migration-plan.md D2）。
export default defineConfig({
  plugins: [react()],
  base: "/app/",
  server: {
    port: 5173,
    proxy: {
      "/api": { target: "http://127.0.0.1:47833", changeOrigin: false },
    },
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
  },
});

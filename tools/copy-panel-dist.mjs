#!/usr/bin/env node
// 把 Vite 构建产物（desktop/dist）拷进 assets/panel-dist/，供 panel.rs 编译期内嵌。
// 为什么走「拷贝 + 内嵌」而不是运行时读文件：面板要编进单 binary（零运行时是项目卖点），
// 而直接 include desktop/dist 会把 node_modules 旁边的世界搅进 cargo 的依赖跟踪——
// 隔离目录让「没跑 npm 的协作者也能 cargo build」成为可能（build.rs 检测空目录时退占位页）。
//
// 用法：node tools/copy-panel-dist.mjs   （通常由 build 脚本 / CI 调用，不用手跑）
import { cpSync, existsSync, mkdirSync, rmSync } from "node:fs";
import { join } from "node:path";

const root = join(import.meta.dirname, "..");
const src = join(root, "desktop", "dist");
const dst = join(root, "assets", "panel-dist");

if (!existsSync(join(src, "index.html"))) {
  console.error("desktop/dist 里没有 index.html —— 先在 desktop/ 跑 npm run build");
  process.exit(1);
}
rmSync(dst, { recursive: true, force: true });
mkdirSync(dst, { recursive: true });
cpSync(src, dst, { recursive: true });
console.log(`panel-dist 已更新：${dst}`);

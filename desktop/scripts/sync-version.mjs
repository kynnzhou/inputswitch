#!/usr/bin/env node
// 版本同步：根 Cargo.toml 是唯一真源 → desktop/package.json + assets/badge-release.svg。
// P2 落地 Tauri 壳后，src-tauri/Cargo.toml 与 tauri.conf.json 也从这里同步（对齐 cc-bridge）。
// 发版流程第一步（CLAUDE.md 发版五步的第 1 步），不同步就推 tag 会被 release.yml 的
// 「tag 与 Cargo.toml version 一致」校验红掉。
import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const root = join(import.meta.dirname, "..", "..");
const cargo = readFileSync(join(root, "Cargo.toml"), "utf8");
const m = cargo.match(/^version\s*=\s*"([^"]+)"/m);
if (!m) throw new Error("Cargo.toml 里读不到 version");
const ver = m[1];
let touched = [];

// ① desktop/package.json
const pkgPath = join(root, "desktop", "package.json");
const pkg = JSON.parse(readFileSync(pkgPath, "utf8"));
if (pkg.version !== ver) {
  pkg.version = ver;
  writeFileSync(pkgPath, JSON.stringify(pkg, null, 2) + "\n");
  touched.push("desktop/package.json");
}

// ② assets/badge-release.svg（README 版本徽章，三处 v0.0.0）
const badgePath = join(root, "assets", "badge-release.svg");
let badge = readFileSync(badgePath, "utf8");
const next = badge.replace(/v\d+\.\d+\.\d+/g, `v${ver}`);
if (next !== badge) {
  writeFileSync(badgePath, next);
  touched.push("assets/badge-release.svg");
}

console.log(touched.length ? `已同步 v${ver} → ${touched.join(", ")}` : `v${ver} 已是同步状态`);

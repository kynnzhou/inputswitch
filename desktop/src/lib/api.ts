// panel.rs 约 40 个 /api/* 端点的逐一封装。路由表对照 src/panel.rs 的 match 臂；
// 函数名跟旧面板 actions.js 的入口一致（switchTo / setTarget / savePair …），
// 迁移期对照得起来。全部同源 fetch（dev 由 vite 代理），错误时 Promise reject
// 带后端的 error 字段。
import type { Snapshot } from "./types";

async function call(path: string, body?: unknown): Promise<unknown> {
  const opt: RequestInit = { method: "POST" };
  if (body !== undefined) opt.body = JSON.stringify(body);
  const res = await fetch(path, opt);
  const json = (await res.json().catch(() => ({ ok: true }))) as {
    ok?: boolean;
    error?: string;
  };
  if (!json.ok) throw new Error(json.error || "请求失败");
  return json;
}

export function fetchState(): Promise<Snapshot> {
  return fetch("/api/state", { cache: "no-store" }).then((r) => {
    if (!r.ok) throw new Error("主程序没在跑");
    return r.json() as Promise<Snapshot>;
  });
}

// ---- 切屏与热键 ----
export const switchTo = (value: number, follow = true) =>
  call(`/api/switch?value=${value}&follow=${follow}`);
export const setTarget = (value: number) => call(`/api/target?value=${value}`);
export const clearTarget = () => call("/api/target?value=0");
export const askPeer = (value: number) => call(`/api/ask-peer?value=${value}`);
export const recHotkey = () => call("/api/hotkey-record");
export const resetHotkey = () => call("/api/hotkey-reset");

// ---- 检测 ----
export const rescan = () => call("/api/rescan");
export const scanPorts = () => call("/api/scan");
export const checkSupport = () => call("/api/check");

// ---- 配对 ----
export const savePair = (code: string) => call("/api/pair", { code });

// ---- 开关类 ----
export const toggleAutostart = (on: boolean) => call("/api/autostart", { on });
export const toggleClipboard = (on: boolean) => call("/api/clipboard", { on });
export const kvmPower = (on: boolean) => call(`/api/kvm?on=${on}`);
export const kvmAutoResume = (on: boolean) => call("/api/kvm-autoresume", { on });
export const kvmKeepAlive = (on: boolean) => call("/api/kvm-keepalive", { on });
export const submachine = (on: boolean) => call("/api/submachine", { on });

// ---- 文件传输 ----
export const sendFiles = (node?: string) =>
  call(node ? `/api/send?node=${encodeURIComponent(node)}` : "/api/send");
export const sendFolder = (node?: string) =>
  call(node ? `/api/send-folder?node=${encodeURIComponent(node)}` : "/api/send-folder");
export const sendTest = (node?: string) =>
  call(node ? `/api/send-test?node=${encodeURIComponent(node)}` : "/api/send-test");
export const chooseInbox = () => call("/api/inbox");
export const openInbox = () => call("/api/open-inbox");
export const transferAction = (id: number, act: "pause" | "resume" | "cancel") =>
  call(`/api/transfer?id=${id}&action=${act}`);

// ---- 共用键鼠 ----
export const kvmHandover = (node?: string) =>
  call(node ? `/api/kvm-handover?node=${encodeURIComponent(node)}` : "/api/kvm-handover");
export const kvmTakeback = () => call("/api/kvm-takeback");
export const setKvmScale = (v: number) => call(`/api/kvm-scale?value=${v}`);
export const setLink = (node: string, side: string) =>
  call(`/api/link?node=${encodeURIComponent(node)}&side=${encodeURIComponent(side)}`);

// ---- 其他 ----
export const setLabel = (name: string, label: string) => call("/api/label", { name, label });
export const openConfigDir = () => call("/api/open-config");
export const restartApp = () => call("/api/restart");
export const updateAction = (act: "check" | "download" | "install" | "cancel" | "dismiss") =>
  call(`/api/update?action=${act}`);

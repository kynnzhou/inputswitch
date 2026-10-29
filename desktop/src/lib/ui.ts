// 壳层共享的 UI 类型。独立成文件，避免 App ↔ 各组件的循环导入
// （Sidebar/Topbar 引 PageId，App 引 Sidebar——类型放 App 里会绕环）。

export type PageId = "workspace" | "devices" | "transfers" | "settings";

/** 动作回执 toast（对照旧面板 flash：3.2 秒生命周期，轮询不覆盖） */
export interface Toast {
  text: string;
  until: number;
}

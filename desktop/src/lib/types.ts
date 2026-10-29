// Snapshot 及子结构的 TS 镜像 —— 逐字段对照 src/panel.rs / transfer/tasks.rs /
// update/mod.rs 的 serde 序列化。字段改名在这里编译期就会暴露（api.ts 的用法
// 全部过 tsc），这正是迁移要的第一个收益（细案 A1）。

export type Presence = "unknown" | "occupied" | "idle" | "invalid";

export interface InputView {
  name: string;
  label: string;
  value: number;
  current: boolean;
  presence: Presence;
  verified: boolean;
}

export interface PeerView {
  node: string;
  addr: string;
  input: string;
  label: string;
  value: number;
  paired: boolean;
  knows_port: boolean;
  needs_pair: boolean;
  kvm_on: boolean;
}

export interface LinkView {
  node: string;
  addr: string;
  /** 空 = 无门（跟随画面链路） */
  side: string;
  mode: string;
  alive: boolean;
  active: boolean;
  can_retarget: boolean;
}

export interface KvmView {
  on: boolean;
  mode: string;
  role: string;
  state: string;
  peer_alive: boolean;
  active: string;
  links: LinkView[];
  follow_link: boolean;
  will_follow: boolean;
  root: string;
  recv_scale: number;
  auto_resume: boolean;
  keep_alive: boolean;
}

export interface PermsView {
  capture: boolean;
  inject: boolean;
}

/** 传输任务注册表（transfer/tasks.rs TaskView） */
export interface TaskView {
  id: number;
  peer: string;
  name: string;
  total: number;
  sent: number;
  /** sending / paused / done / failed / cancelled */
  phase: "sending" | "paused" | "done" | "failed" | "cancelled" | string;
  error: string;
  started_at: number;
}

export type UpdatePhase =
  | "idle"
  | "checking"
  | "available"
  | "downloading"
  | "ready"
  | "restarting"
  | "failed"
  | string;

export interface UpdateState {
  phase: UpdatePhase;
  current: string;
  latest: string;
  notes: string;
  pub_date: string;
  url: string;
  checksum: string;
  error: string;
  code: string;
  skipped: string;
  checked_at: number;
  downloaded: number;
  total: number;
  bytes_per_sec: number;
  restart_in: number;
}

export interface Snapshot {
  node: string;
  display: string;
  identity: string;
  status: string;
  support: string;
  current: number;
  current_name: string;
  hotkey: string;
  target: string;
  paired: boolean;
  pair_code: string;
  rejected: number;
  replayed: number;
  peer_verified: number;
  has_display: boolean;
  own_value: number;
  port_note: string;
  maint_busy: boolean;
  autostart: boolean;
  clipboard: boolean;
  inbox: string;
  port: number;
  peers: PeerView[];
  inputs: InputView[];
  inputs_hidden: InputView[];
  transfer: string;
  transfers: TaskView[];
  kvm: KvmView;
  perms: PermsView;
  update: UpdateState;
}

import { useEffect, useRef, type ReactNode } from "react";
import { Icon } from "./IconSprite";

/**
 * 弹窗骨架 = 设计稿的 <dialog class="modal"> + .modal-content：
 * - 原生 dialog.showModal()：Esc 关闭、外点关闭、焦点困在框内都由平台负责
 * - 关闭后把焦点还给打开它的那个按钮
 */
export function Modal({
  label,
  onClose,
  children,
}: {
  label: string;
  onClose: () => void;
  children: ReactNode;
}) {
  const ref = useRef<HTMLDialogElement>(null);
  const openerRef = useRef<Element | null>(null);
  // onClose 是调用方每次渲染新建的箭头函数（快照 400ms 一刷 → 每次都是新的）。
  // 直接进依赖数组会让「关掉再打开」每 400ms 跑一遍，弹窗闪、焦点跳；
  // 用 ref 顶住，挂载/卸载只在真正开合时发生。
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;

  useEffect(() => {
    openerRef.current = document.activeElement;
    const d = ref.current;
    if (d && !d.open && typeof d.showModal === "function") d.showModal();
    const onCancel = (e: Event) => {
      e.preventDefault();
      onCloseRef.current();
    };
    d?.addEventListener("cancel", onCancel);
    return () => {
      d?.removeEventListener("cancel", onCancel);
      if (d?.open && typeof d.close === "function") d.close();
      (openerRef.current as HTMLElement | null)?.focus?.();
    };
  }, []);

  return (
    <dialog
      ref={ref}
      aria-label={label}
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div className="modal-content">{children}</div>
    </dialog>
  );
}

export function ModalHead({ title, onClose }: { title: string; onClose: () => void }) {
  return (
    <div className="modal-head">
      <h2>{title}</h2>
      <button className="icon-button" onClick={onClose} aria-label="关闭对话框">
        <Icon name="close" />
      </button>
    </div>
  );
}

import { useEffect, useRef, type ReactNode } from "react";

/**
 * 弹窗骨架（对照批3 openModal/closeModal 三件套）：
 * - Esc 关最上层（多层各关各的）；遮罩点击关闭；开时焦点进框、关时还焦
 * - role=dialog + aria-modal；标题由 aria-label 给
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
  const boxRef = useRef<HTMLDivElement>(null);
  const openerRef = useRef<Element | null>(null);

  useEffect(() => {
    openerRef.current = document.activeElement;
    boxRef.current?.focus();
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        onClose();
      }
    };
    document.addEventListener("keydown", onKey, true);
    return () => {
      document.removeEventListener("keydown", onKey, true);
      (openerRef.current as HTMLElement | null)?.focus?.();
    };
  }, [onClose]);

  return (
    <div
      className="modal-mask"
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div className="modal glass" role="dialog" aria-modal="true" aria-label={label} ref={boxRef} tabIndex={-1}>
        {children}
      </div>
    </div>
  );
}

export function ModalHead({ title, onClose }: { title: string; onClose: () => void }) {
  return (
    <div className="modal-head">
      <b>{title}</b>
      <button className="modal-x" onClick={onClose} aria-label="关闭">
        ×
      </button>
    </div>
  );
}

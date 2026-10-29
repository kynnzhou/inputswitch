// 33 个线性图标 + i-download，从旧面板 index.html 的 <symbol> 原样搬来。
// 一次挂在 App 根部，各处 <Icon name="grid" /> 引用。
export function IconSprite() {
  return (
    <svg className="icon-definitions" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
      <defs>
        <symbol id="i-grid" viewBox="0 0 24 24"><rect x="3" y="3" width="7" height="7" rx="1.5"/><rect x="14" y="3" width="7" height="7" rx="1.5"/><rect x="3" y="14" width="7" height="7" rx="1.5"/><rect x="14" y="14" width="7" height="7" rx="1.5"/></symbol>
        <symbol id="i-monitor" viewBox="0 0 24 24"><rect x="3" y="4" width="18" height="13" rx="2"/><path d="M8 21h8M12 17v4"/></symbol>
        <symbol id="i-screens" viewBox="0 0 24 24"><rect x="2" y="3" width="14" height="11" rx="2"/><path d="M5 18h8M9 14v4M19 8h1a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H10a2 2 0 0 1-2-2"/></symbol>
        <symbol id="i-transfer" viewBox="0 0 24 24"><path d="M7 3v16m-4-4 4 4 4-4M17 21V5m-4 4 4-4 4 4"/></symbol>
        <symbol id="i-settings" viewBox="0 0 24 24"><path d="M4 7h7m6 0h3M4 17h3m6 0h7"/><circle cx="14" cy="7" r="3"/><circle cx="10" cy="17" r="3"/></symbol>
        <symbol id="i-arrow" viewBox="0 0 24 24"><path d="M4 12h16m-6-6 6 6-6 6"/></symbol>
        <symbol id="i-swap" viewBox="0 0 24 24"><path d="M3 7h17m-4-4 4 4-4 4M21 17H4m4-4-4 4 4 4"/></symbol>
        <symbol id="i-keyboard" viewBox="0 0 24 24"><rect x="2" y="5" width="20" height="14" rx="2"/><path d="M6 9h.01M10 9h.01M14 9h.01M18 9h.01M6 12h.01M10 12h.01M14 12h.01M18 12h.01M7 15h10" stroke-linecap="round"/></symbol>
        <symbol id="i-mouse" viewBox="0 0 24 24"><rect x="6" y="2" width="12" height="20" rx="6"/><path d="M12 2v7M6 10h12"/></symbol>
        <symbol id="i-clipboard" viewBox="0 0 24 24"><rect x="8" y="2" width="8" height="4" rx="1"/><path d="M8 4H5a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2V6a2 2 0 0 0-2-2h-3M8 11h8M8 16h6"/></symbol>
        <symbol id="i-shield" viewBox="0 0 24 24"><path d="m12 2 8 3v6c0 5-4 9-8 11-4-2-8-6-8-11V5l8-3Z"/><path d="m8 12 3 3 5-6"/></symbol>
        <symbol id="i-refresh" viewBox="0 0 24 24"><path d="M20 7v5h-5M4 17v-5h5"/><path d="M6 7a7 7 0 0 1 12-1l2 3M4 15l2 3a7 7 0 0 0 12-1"/></symbol>
        <symbol id="i-scan" viewBox="0 0 24 24"><path d="M8 3H5a2 2 0 0 0-2 2v3m13-5h3a2 2 0 0 1 2 2v3M3 16v3a2 2 0 0 0 2 2h3m8 0h3a2 2 0 0 0 2-2v-3M3 12h18"/></symbol>
        <symbol id="i-check" viewBox="0 0 24 24"><path d="m5 12 4 4L19 6"/></symbol>
        <symbol id="i-check-circle" viewBox="0 0 24 24"><circle cx="12" cy="12" r="9"/><path d="m8 12 3 3 5-6"/></symbol>
        <symbol id="i-help" viewBox="0 0 24 24"><circle cx="12" cy="12" r="9"/><path d="M9.5 9a2.5 2.5 0 1 1 4 2c-1 .6-1.5 1-1.5 2M12 17h.01"/></symbol>
        <symbol id="i-plus" viewBox="0 0 24 24"><path d="M12 5v14M5 12h14"/></symbol>
        <symbol id="i-close" viewBox="0 0 24 24"><path d="m6 6 12 12M18 6 6 18"/></symbol>
        <symbol id="i-chevron" viewBox="0 0 24 24"><path d="m9 5 7 7-7 7"/></symbol>
        <symbol id="i-folder" viewBox="0 0 24 24"><path d="M3 7V5a2 2 0 0 1 2-2h5l2 3h7a2 2 0 0 1 2 2v11a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V7h18"/></symbol>
        <symbol id="i-upload" viewBox="0 0 24 24"><path d="M12 16V3m-5 5 5-5 5 5M4 15v5a1 1 0 0 0 1 1h14a1 1 0 0 0 1-1v-5"/></symbol>
        <symbol id="i-download" viewBox="0 0 24 24"><path d="M12 3v12m-5-5 5 5 5-5M4 21h16"/></symbol>
        <symbol id="i-file" viewBox="0 0 24 24"><path d="M14 2H5a1 1 0 0 0-1 1v18a1 1 0 0 0 1 1h14a1 1 0 0 0 1-1V9l-7-7Zm0 0v7h7M8 14h8M8 18h5"/></symbol>
        <symbol id="i-sun" viewBox="0 0 24 24"><circle cx="12" cy="12" r="4"/><path d="M12 2v2m0 16v2M2 12h2m16 0h2M5 5l1 1m12 12 1 1M5 19l1-1M18 6l1-1"/></symbol>
        <symbol id="i-moon" viewBox="0 0 24 24"><path d="M21 13a9 9 0 0 1-10-10 9 9 0 1 0 10 10Z"/></symbol>
        <symbol id="i-clock" viewBox="0 0 24 24"><circle cx="12" cy="12" r="9"/><path d="M12 7v5l3 2"/></symbol>
        <symbol id="i-info" viewBox="0 0 24 24"><circle cx="12" cy="12" r="9"/><path d="M12 11v6m0-10h.01"/></symbol>
        <symbol id="i-wifi" viewBox="0 0 24 24"><path d="M2 8a16 16 0 0 1 20 0M5 12a11 11 0 0 1 14 0M8 16a6 6 0 0 1 8 0M12 20h.01"/></symbol>
        <symbol id="i-pause" viewBox="0 0 24 24"><path d="M8 5v14M16 5v14"/></symbol>
        <symbol id="i-play" viewBox="0 0 24 24"><path d="m8 4 12 8-12 8V4Z"/></symbol>
        <symbol id="i-link" viewBox="0 0 24 24"><path d="m10 13 4-4M8 15l-2 2a4 4 0 0 0 6 6l3-3a4 4 0 0 0 0-6M16 9l2-2a4 4 0 0 0-6-6L9 4a4 4 0 0 0 0 6" transform="translate(0 -1)"/></symbol>
        <symbol id="i-expand" viewBox="0 0 24 24"><path d="M8 3H3v5m13-5h5v5M3 16v5h5m13-5v5h-5"/></symbol>
        <symbol id="i-windows" viewBox="0 0 24 24"><path d="M3 4h8v8H3zm10 0h8v8h-8zM3 14h8v7H3zm10 0h8v7h-8z" fill="currentColor" stroke="none"/></symbol>
        <symbol id="i-laptop" viewBox="0 0 24 24"><rect x="4" y="3" width="16" height="13" rx="2"/><path d="m4 16-2 4h20l-2-4M9 20h6"/></symbol>
      </defs>
    </svg>
  );
}

/** 行内图标引用：stroke 继承 currentColor（同旧面板 <svg><use>）。 */
export function Icon({ name }: { name: string }) {
  return (
    <svg className="icon" aria-hidden="true">
      <use href={`#i-${name}`} />
    </svg>
  );
}

//! 新前端（React）构建产物的内嵌表，由 build.rs 的 `emit_panel_dist` 生成。
//! 没跑过 `npm build` 时表为空，`/app/` 回提示页 —— cargo 单独构建不依赖 node。
//!

include!(concat!(env!("OUT_DIR"), "/panel_dist.rs"));

/// 找不到产物时给一句人话，而不是空白页/404。
pub(super) fn lookup_html(name: &str) -> &'static str {
    for (url, _, bytes) in PANEL_DIST {
        if *url == name {
            return std::str::from_utf8(bytes).unwrap_or(FALLBACK);
        }
    }
    FALLBACK
}

const FALLBACK: &str = "<!DOCTYPE html><html lang=zh-CN><meta charset=utf-8>\
<title>InputSwitch</title><body style=\"font:14px system-ui;padding:24px\">\
新前端还没有构建产物：先在 desktop/ 跑 <code>npm ci && npm run build</code>，\
再跑 <code>node tools/copy-panel-dist.mjs</code>，然后重新 cargo build。\
旧面板在 <a href=/ >这里</a>。</body></html>";

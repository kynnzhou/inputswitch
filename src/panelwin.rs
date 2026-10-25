//! 控制面板的原生窗口。
//!
//! 界面的那套 HTML 不动（同一份 `assets/panel/`），只是把"放到浏览器里打开"
//! 换成"挂进程序自己的窗口"：macOS 用系统的 WKWebView，Windows 用 WebView2。
//! 两者都由系统自带，不额外打包内核，安装包不会因此变大。
//!
//! 窗口对象跟着程序活，用户点关闭只是隐藏，下次点菜单是秒开，不用重新加载。

#[cfg(any(target_os = "macos", windows))]
mod imp {
    use tao::dpi::LogicalSize;
    use tao::event_loop::EventLoopWindowTarget;
    use tao::window::{Window, WindowBuilder, WindowId};
    use wry::dpi::{LogicalPosition as WryPos, LogicalSize as WrySize, Position, Size};
    use wry::Rect;

    const WIDTH: f64 = 820.0;
    const HEIGHT: f64 = 840.0;

    pub struct Panel {
        // 字段顺序不能改：Rust 按声明顺序 drop，WebView 必须先于 Window 释放，
        // 否则窗口没了 WebView 还挂在一个已经销毁的 NSView 上。
        window: Window,
        webview: wry::WebView,
        /// 第一次显示时把页面滚回顶部（WebView 尺寸一变会保留旧的滚动位置）
        fresh: std::cell::Cell<bool>,
    }

    impl Panel {
        /// 建窗口 + 挂 WebView。必须在主线程（也就是事件循环里）调用。
        pub fn open<T>(
            target: &EventLoopWindowTarget<T>,
            port: u16,
            title: &str,
        ) -> Result<Self, String> {
            let (width, height) = fit_screen(target);
            let window = WindowBuilder::new()
                .with_title(title)
                .with_inner_size(LogicalSize::new(width, height))
                .with_min_inner_size(LogicalSize::new(600.0, 520.0))
                .with_visible(false)
                .build(target)
                .map_err(|e| format!("建窗口失败：{e}"))?;

            center(&window, width, height);

            let rect = Rect {
                position: Position::Logical(WryPos::new(0.0, 0.0)),
                size: Size::Logical(WrySize::new(width, height)),
            };
            let webview = wry::WebViewBuilder::new()
                .with_url(format!("http://127.0.0.1:{port}/"))
                .with_bounds(rect)
                .with_background_color((244, 245, 247, 255))
                .build_as_child(&window)
                .map_err(|e| format!("WebView 创建失败：{e}"))?;

            Ok(Self {
                window,
                webview,
                fresh: std::cell::Cell::new(true),
            })
        }

        pub fn show(&self) {
            self.window.set_visible(true);
            self.window.set_focus();
            // 托盘程序是 LSUIElement（没有 Dock 图标），光 set_focus 抢不到前台，
            // 窗口会藏在别的 app 后面，用户会以为点了没反应。
            activate_app();
            if self.fresh.replace(false) {
                let _ = self.webview.evaluate_script("window.scrollTo(0, 0)");
            }
        }

        pub fn hide(&self) {
            self.window.set_visible(false);
        }

        pub fn owns(&self, id: WindowId) -> bool {
            self.window.id() == id
        }

        pub fn reload(&self) {
            let _ = self.webview.reload();
        }

        /// 窗口被拉动时把 WebView 的大小跟上去（wry 的 child 模式不会自动跟随）
        pub fn fit(&self) {
            let size = self.window.inner_size().to_logical::<f64>(self.window.scale_factor());
            let rect = Rect {
                position: Position::Logical(WryPos::new(0.0, 0.0)),
                size: Size::Logical(WrySize::new(size.width, size.height)),
            };
            let _ = self.webview.set_bounds(rect);
        }
    }

    /// 把本进程提到前台。LSUIElement 的 app 光靠 makeKeyAndOrderFront
    /// 抢不到焦点，得显式 `[NSApp activateIgnoringOtherApps:YES]`。
    #[cfg(target_os = "macos")]
    fn activate_app() {
        use std::ffi::{c_char, c_void, CString};
        use std::mem::transmute;

        extern "C" {
            fn objc_getClass(name: *const c_char) -> *mut c_void;
            fn sel_registerName(name: *const c_char) -> *mut c_void;
            fn objc_msgSend();
        }
        type MsgId = unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void;
        type MsgActivate = unsafe extern "C" fn(*mut c_void, *mut c_void, i8);

        unsafe fn sel(name: &str) -> *mut c_void {
            let c = CString::new(name).expect("选择器不含 NUL");
            sel_registerName(c.as_ptr())
        }

        unsafe {
            let cls = CString::new("NSApplication").expect("类名不含 NUL");
            let cls = objc_getClass(cls.as_ptr());
            if cls.is_null() {
                return;
            }
            let shared: MsgId = transmute(objc_msgSend as unsafe extern "C" fn());
            let app = shared(cls, sel("sharedApplication"));
            if app.is_null() {
                return;
            }
            let activate: MsgActivate = transmute(objc_msgSend as unsafe extern "C" fn());
            activate(app, sel("activateIgnoringOtherApps:"), 1);
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn activate_app() {}

    /// 窗口别比屏幕还高：13 寸笔记本可用高度不到 900，840 的窗会被 Dock 吃掉底部。
    fn fit_screen<T>(target: &EventLoopWindowTarget<T>) -> (f64, f64) {
        let available = target
            .primary_monitor()
            .or_else(|| target.available_monitors().next())
            .map(|m| m.size().to_logical::<f64>(m.scale_factor()).height)
            .unwrap_or(900.0);
        let height = HEIGHT.min(available - 140.0).max(520.0);
        (WIDTH, height)
    }

    /// 摆到屏幕中间。tao 没有现成的居中方法，拿主屏尺寸自己算。
    fn center(window: &Window, width: f64, height: f64) {
        let Some(monitor) = window.current_monitor().or_else(|| {
            window
                .available_monitors()
                .next()
        }) else {
            return;
        };
        let scale = monitor.scale_factor();
        let screen = monitor.size().to_logical::<f64>(scale);
        let origin = monitor.position().to_logical::<f64>(scale);
        let x = origin.x + (screen.width - width) / 2.0;
        let y = origin.y + (screen.height - height) / 2.4;
        window.set_outer_position(tao::dpi::LogicalPosition::new(x, y));
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod imp {
    use tao::event_loop::EventLoopWindowTarget;
    use tao::window::WindowId;

    /// Linux 上系统 WebView 依赖 webkit2gtk，不是所有发行版都有。
    /// 这里保持"用浏览器打开"的老路子，接口留空但编译得过。
    pub struct Panel;

    impl Panel {
        pub fn open<T>(
            _target: &EventLoopWindowTarget<T>,
            _port: u16,
            _title: &str,
        ) -> Result<Self, String> {
            Err("这个平台没有内置 WebView，用浏览器打开".to_string())
        }
        pub fn show(&self) {}
        pub fn hide(&self) {}
        pub fn owns(&self, _id: WindowId) -> bool {
            false
        }
        pub fn reload(&self) {}
        pub fn fit(&self) {}
    }
}

pub use imp::Panel;

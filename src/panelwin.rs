//! 控制面板的原生窗口。
//!
//! 界面的那套 HTML 不动（同一份 `assets/panel/`），只是把"放到浏览器里打开"
//! 换成"挂进程序自己的窗口"：macOS 用系统的 WKWebView，Windows 用 WebView2。
//! 两者都由系统自带，不额外打包内核，安装包不会因此变大。
//!
//! 窗口对象跟着程序活，用户点关闭只是隐藏，下次点菜单是秒开，不用重新加载。
//!
//! Windows 上窗口是**无边框**的：系统那条款式老旧的标题栏去掉，换成页面里
//! 自绘的一条（品牌 + 更新/帮助 + 最小化/最大化/关闭）。macOS 保留系统红绿灯
//! —— 那边三个钮要处理 hover 三色和拖拽，自绘收益低。
//!
//! 无边框带来一个麻烦：tao 的八向 hit-test 写在**顶层窗口**的 WndProc 里，
//! 而 WebView2 是铺满客户区的子窗口，鼠标消息先落到它身上，父窗口根本收不到
//! （`probe` 实测客户区四边的 WindowFromPoint 全返回 Chrome_RenderWidgetHostHWND）。
//! 所以拖拽和缩放不能指望系统，得由页面主动喊一声 —— `drag()` / `resize()`
//! 就是那两个入口，走的都是 tao 现成的 API（内部发 WM_NCLBUTTONDOWN），
//! 之后系统接管到松手，和点系统标题栏没区别。

#[cfg(any(target_os = "macos", windows))]
mod imp {
    use tao::dpi::LogicalSize;
    use tao::event_loop::EventLoopWindowTarget;
    use tao::window::{Window, WindowBuilder, WindowId};
    use wry::dpi::{LogicalPosition as WryPos, LogicalSize as WrySize, Position, Size};
    use wry::Rect;

    const WIDTH: f64 = 820.0;
    const HEIGHT: f64 = 840.0;

    /// 这个平台的窗口是不是自绘边框。编译期定死，前端靠 URL 里的
    /// `?frame=none` 知道自己该不该画那条标题栏和三个窗口钮。
    const SELF_DRAWN_FRAME: bool = cfg!(windows);

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
            let mut builder = WindowBuilder::new()
                .with_title(title)
                .with_inner_size(LogicalSize::new(width, height))
                .with_min_inner_size(LogicalSize::new(600.0, 520.0))
                .with_visible(false);
            #[cfg(windows)]
            {
                // 只去掉系统标题栏，不动 resizable：tao 保留四边缩放能力，
                // 只是那条分支要靠页面主动调 resize() 才会跑到（见模块注释）。
                builder = builder.with_decorations(false);
            }
            let window = builder
                .build(target)
                .map_err(|e| format!("建窗口失败：{e}"))?;

            // 圆角必须在 show 之前打：DWM 属性是"下次合成时用"，show 之后再设
            // 会先闪出一帧直角再变圆。
            #[cfg(windows)]
            round_corners(&window);

            center(&window, width, height);

            // 建窗时 with_visible(false)：系统可能把这扇窗的初始 placement 记成
            // 最小化。这里先还原一次 —— show() 里那次检查不够，因为启动即开面板
            // 时 show() 跑在窗口刚建好、还没进任务栏的那一刻，is_minimized()
            // 仍返回 false，等系统补记最小化态时已经晚了（实测窗口停在
            // -32000,-32000，IsIconic=1，页脚全在屏幕外）。
            window.set_minimized(false);

            let rect = Rect {
                position: Position::Logical(WryPos::new(0.0, 0.0)),
                size: Size::Logical(WrySize::new(width, height)),
            };
            let url = if SELF_DRAWN_FRAME {
                format!("http://127.0.0.1:{port}/?frame=none")
            } else {
                format!("http://127.0.0.1:{port}/")
            };
            let webview = wry::WebViewBuilder::new()
                .with_url(url)
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
            // 窗口是在 with_visible(false) 下建的，系统会把它的初始 placement
            // 记成最小化；直接 set_visible(true) 出来的是一个缩在任务栏上的
            // 159×27 空壳（`INPUTSWITCH_OPEN_PANEL=1` 自测路径实测是这样，
            // 托盘路径因为隔了几百毫秒才 show 所以没事）。先还原再显示。
            if self.window.is_minimized() {
                self.window.set_minimized(false);
            }
            self.window.set_visible(true);
            // 显示之后再查一次：最小化是系统在 ShowWindow 时才补记的，显示
            // 之前那次查询可能还是 false（启动即开面板时实测就是这样）。
            if self.window.is_minimized() {
                self.window.set_minimized(false);
            }
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

        /// 最小化到任务栏。
        pub fn minimize(&self) {
            self.window.set_minimized(true);
        }

        /// 最大化 / 还原。tao 只有 set_maximized（没有 toggle），状态自己翻。
        pub fn toggle_maximize(&self) {
            let now = self.window.is_maximized();
            self.window.set_maximized(!now);
        }

        /// 现在是不是最大化态。面板窗口收到 Resized 时读一次写进快照，
        /// 前端据此把「□」换成「❐」（Win+↑、双击拖拽区、系统菜单都会改这个态，
        /// 光靠点击事件自己记是记不准的）。
        pub fn is_maximized(&self) -> bool {
            self.window.is_maximized()
        }

        /// 跟着鼠标拖窗口。发一条 WM_NCLBUTTONDOWN(HTCAPTION) 给系统，
        /// 之后由系统的移动循环接管到松手 —— 和拖原生标题栏完全一样，
        /// 边缘吸附、Win+方向键、双击最大化全都自带。
        pub fn drag(&self) {
            let _ = self.window.drag_window();
        }

        /// 从某条边/角开始缩放，同样交给系统的尺寸循环。
        ///
        /// 方向用字符串传：`panel.rs` 不引 tao 的类型，只做 HTTP 参数到
        /// 字符串的转发，映射留在这里。
        pub fn resize(&self, dir: &str) {
            use tao::window::ResizeDirection as D;
            let direction = match dir {
                "n" => D::North,
                "s" => D::South,
                "e" => D::East,
                "w" => D::West,
                "ne" => D::NorthEast,
                "nw" => D::NorthWest,
                "se" => D::SouthEast,
                "sw" => D::SouthWest,
                _ => return,
            };
            let _ = self.window.drag_resize_window(direction);
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

    /// Win11 的圆角。无边框窗口 DWM 默认给直角，属性 33（
    /// DWMWA_WINDOW_CORNER_PREFERENCE）= 2 才是圆角。Win10 会返回失败，
    /// 那里本来也没有圆角，忽略即可。
    #[cfg(windows)]
    fn round_corners(window: &Window) {
        use std::ffi::c_void;
        use tao::platform::windows::WindowExtWindows;

        #[link(name = "dwmapi")]
        extern "system" {
            fn DwmSetWindowAttribute(
                hwnd: isize,
                attr: u32,
                value: *const c_void,
                size: u32,
            ) -> i32;
        }

        const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
        const DWMWCP_ROUND: u32 = 2;

        let preference: u32 = DWMWCP_ROUND;
        unsafe {
            let _ = DwmSetWindowAttribute(
                window.hwnd(),
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &preference as *const u32 as *const c_void,
                std::mem::size_of::<u32>() as u32,
            );
        }
    }

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
        pub fn minimize(&self) {}
        pub fn toggle_maximize(&self) {}
        pub fn is_maximized(&self) -> bool {
            false
        }
        pub fn drag(&self) {}
        pub fn resize(&self, _dir: &str) {}
        pub fn owns(&self, _id: WindowId) -> bool {
            false
        }
        pub fn reload(&self) {}
        pub fn fit(&self) {}
    }
}

pub use imp::Panel;

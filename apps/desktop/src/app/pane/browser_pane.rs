//! 浏览器 Pane：GPUI 画的地址栏外壳 + `wry`（WebView2）的原生子窗口。
//!
//! ## 为什么是原生子窗口
//!
//! WebView2 只有两种嵌法：`WS_CHILD` 子窗口（wry 的 `build_as_child`）或
//! `ICoreWebView2CompositionController` 的视觉树。后者没有 HWND，等于要自己把鼠标 /
//! 键盘 / IME / DPI 全手工转发进去，工程量远超一个 Pane。所以走子窗口，并给
//! `vendor/gpui-pre-windows` 打了一行补丁（合成目标不再置顶），否则整条交换链会盖在
//! 子窗口上面，网页永远看不见。
//!
//! ## 代价：Airspace
//!
//! 子窗口永远浮在 GPUI 画的内容之上，所以凡是「盖在它上面」的东西都得让它先躲开：
//! 不在活跃 Tab（`set_active(false)`）、命令面板 / 快捷键速查 / 原始请求（都是对话框）、
//! 下拉菜单（`gpui_base::GlobalState::is_in_deferred_context` 就是「当前有 Popover 打开」）、
//! 设置页 / 审查页 / 回退浮层（`ChatApp` 自己的标记）。判据在 prepaint 里算一次，
//! 位置 / 可见性跟上次写进去的值比一遍，变了才跨进程调用。
//!
//! ## 焦点在网页里时的快捷键
//!
//! 子窗口拿到键盘焦点后，按键走的是 WebView2 自己的窗口过程，GPUI 一条也收不到，
//! `Ctrl+K` / `Ctrl+;` / `Ctrl+\`` 这类全局快捷键会突然失灵。所以建好 WebView2 时挂一次
//! `AcceleratorKeyPressed`，把「按住 Ctrl 或 Alt」的组合键按当前键盘布局翻成
//! [`Keystroke`] 再 `Window::dispatch_keystroke` 回 GPUI；GPUI 真处理了才 `Handled(true)`，
//! 没人接的键仍归网页（不然网页连自己的 `Ctrl+C` 都用不了）。
//! 转发只在消息回调里做，那一层 App 没被借出（`AsyncApp::update_window` 是 `try_borrow_mut`，
//! 忙就返回 Err 并原样让给网页），不会打崩。
//!
//! ## 创建时机
//!
//! wry 建 WebView2 时用 `webview2_com::wait_with_pump`，会**泵掉所有消息**。GPUI 的
//! `App` 是 `RefCell`，但消息回调用的是 `try_borrow_mut`（忙就丢消息，不 panic），
//! `draw_window` 本身也有重入守卫，所以在绘制栈里泵消息不会崩，只会丢掉那一小段时间的输入。
//! 为了连这点代价都省掉，创建放进前台任务（`spawn_in` + `update_in`），那时已经退出绘制栈。

use super::{ClaudesktopPane, PaneKind};
use crate::app::ChatApp;
use crate::theme::rgb;
use crate::theme::*;
use gpui_base::dock::{Panel, PanelEvent, PanelState};
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;
use wry::dpi::{PhysicalPosition, PhysicalSize};
use wry::{Rect, WebContext, WebView, WebViewBuilder};

/// 地址栏空着时的落地页：本地空白页，不联网
const START_URL: &str = "about:blank";

/// WebView2 的用户数据目录名（放系统数据目录下；写 exe 旁边在只读安装目录会直接失败）
const DATA_DIR: &str = "webview2";

/// 物理矩形（父窗口客户区像素）：`wry::Rect` 的坐标是枚举，自己存一份平铺的更好比
type Physical = (i32, i32, u32, u32);

pub(crate) struct BrowserPane {
    app: WeakEntity<ChatApp>,
    focus_handle: FocusHandle,
    /// 地址栏（Pane 自己的输入框，不抢主输入框）
    address: Entity<InputState>,
    /// 当前地址：地址栏回车、页面内跳转都会更新它，退出时靠它恢复
    url: String,
    /// 页内跳转（点链接 / 重定向）由导航回调写进这里，下一帧取走更新地址栏
    seen: Rc<RefCell<Option<String>>>,
    webview: Option<WebView>,
    /// 已经排过一次创建（防止每帧都排）
    creating: bool,
    /// 建不起来的原因（缺 WebView2 运行时最常见），画在占位底上
    error: Option<SharedString>,
    /// 上一次量到的正文区物理矩形
    body: Option<Physical>,
    /// 上一次写给 WebView2 的值：没变就不跨进程调用
    applied: Option<(Physical, bool)>,
    /// 是不是所在 Tab 的活跃页（切走时立刻把子窗口藏起来）
    active: bool,
}

impl BrowserPane {
    pub(crate) fn new(
        app: WeakEntity<ChatApp>,
        url: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        super::follow_hub(app.clone(), cx);
        let url = url.unwrap_or_default();
        let address = address_input(&url, window, cx);
        Self {
            app,
            focus_handle: cx.focus_handle(),
            address,
            url,
            seen: Rc::new(RefCell::new(None)),
            webview: None,
            creating: false,
            error: None,
            body: None,
            applied: None,
            active: true,
        }
    }

    // ---------- 起 WebView2 ----------

    /// 换窗口时地址栏也要重建（`InputState` 绑着旧 `Window`）
    fn rebuild_address(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.address = address_input(&self.url, window, cx);
    }

    /// 排一次创建；真正的构建在前台任务里做（见文件头「创建时机」）。
    fn start_webview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.creating || self.webview.is_some() || self.error.is_some() || headless() {
            return;
        }
        // 拿不到原生窗口（无头测试平台）时 build_as_child 会返回错误，走 error 分支即可
        self.creating = true;
        cx.spawn_in(window, async move |this, cx| {
            // 让出一拍：退出绘制栈后再建，泵消息不会和正在绘制的帧抢渲染器
            cx.background_executor().timer(Duration::ZERO).await;
            let _ = this.update_in(cx, |pane, window, cx| pane.build_webview(window, cx));
        })
        .detach();
    }

    fn build_webview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.creating = false;
        if self.webview.is_some() || headless() {
            return;
        }
        let Some(dir) = data_dir() else {
            self.error = Some("找不到可写的用户数据目录".into());
            cx.notify();
            return;
        };
        let urlish = if self.url.is_empty() { START_URL.to_string() } else { self.url.clone() };
        let (x, y, w, h) = self.body.unwrap_or((0, 0, 1, 1));
        let seen = self.seen.clone();
        // 页内跳转也要唤醒界面（地址栏 / Tab 标题才跟得上）。回调在 WebView2 的消息里跑，
        // 只能 try_borrow 拿 App：建窗口那一下 App 正被借走，这时宁可不唤醒也不能 panic。
        let pane = cx.entity().downgrade();
        // 导航回调是 `Fn`（会反复调用），`AsyncApp::update_window` 要 `&mut`，用 RefCell 兜一层
        let notify_app = std::cell::RefCell::new(cx.to_async());
        let notify_window = window.window_handle();
        let mut context = WebContext::new(Some(dir));
        let built = WebViewBuilder::new_with_web_context(&mut context)
            .with_url(urlish)
            .with_bounds(Rect {
                position: PhysicalPosition::new(x, y).into(),
                size: PhysicalSize::new(w.max(1), h.max(1)).into(),
            })
            .with_background_color((0, 0, 0, 0))
            .with_navigation_handler(move |url| {
                *seen.borrow_mut() = Some(url);
                let _ = notify_app.borrow_mut().update_window(notify_window, |_, _, cx| {
                    if let Some(pane) = pane.upgrade() {
                        pane.update(cx, |_, cx| cx.notify());
                    }
                });
                true
            })
            .build_as_child(&*window);
        match built {
            Ok(webview) => {
                // 焦点进网页后按键不再经过 GPUI，建好就把组合键接回来（见文件头）
                let focus = self.focus_handle.clone();
                install_accelerators(&webview, window, &focus, cx);
                self.webview = Some(webview);
                self.applied = None;
                cx.notify();
            }
            Err(err) => {
                self.error = Some(format!("WebView2 启动失败：{err}").into());
                cx.notify();
            }
        }
    }

    /// prepaint 量到的正文区 + 这一帧该不该露面。值没变就不碰 WebView2。
    fn on_body(&mut self, rect: Physical, covered: bool) {
        self.body = Some(rect);
        let visible = self.active && !covered;
        let next = (rect, visible);
        if self.applied == Some(next) {
            return;
        }
        if let Some(webview) = &self.webview {
            let _ = webview.set_bounds(Rect {
                position: PhysicalPosition::new(rect.0, rect.1).into(),
                size: PhysicalSize::new(rect.2.max(1), rect.3.max(1)).into(),
            });
            let _ = webview.set_visible(visible);
            self.applied = Some(next);
        }
    }

    // ---------- 导航 ----------

    /// 地址栏回车：裸域名补 `https://`，其余当搜索词
    fn navigate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let typed = self.address.read(cx).value().trim().to_string();
        if typed.is_empty() {
            return;
        }
        let target = normalize_url(&typed);
        self.url = target.clone();
        if let Some(webview) = &self.webview {
            let _ = webview.load_url(&target);
        }
        self.address.update(cx, |state, cx| state.set_value(target, window, cx));
        cx.notify();
    }

    fn run_js(&mut self, js: &str, cx: &mut Context<Self>) {
        if let Some(webview) = &self.webview {
            let _ = webview.evaluate_script(js);
        }
        cx.notify();
    }

    // ---------- 地址栏按钮 ----------

    fn nav_button(&self, id: &'static str, icon: IconAll, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id((id, cx.entity_id()))
            .h_full()
            .flex_shrink_0()
            .px_1()
            .flex()
            .items_center()
            .rounded_md()
            .text_color(rgb(INK_SOFT))
            .hover(|d| d.bg(rgb(HOVER)).text_color(rgb(INK)))
            .child(Icon::new(icon).small())
            .on_click(cx.listener(move |this, _, _, cx| {
                let js = match id {
                    "browser-back" => "history.back()",
                    "browser-forward" => "history.forward()",
                    _ => "location.reload()",
                };
                this.run_js(js, cx);
            }))
            .into_any_element()
    }
}

impl ClaudesktopPane for BrowserPane {
    fn pane_kind(&self) -> PaneKind {
        PaneKind::Browser
    }

    fn icon(&self, _cx: &App) -> IconAll {
        IconAll::Globe
    }

    fn title(&self, _cx: &App) -> SharedString {
        host_of(&self.url).map_or_else(|| PaneKind::Browser.title(), |host| host.into())
    }

    /// 换窗口（弹出 / 吸回）：WebView2 绑死在创建时的 HWND 上，只能丢掉等下一帧重建；
    /// 地址栏的 `InputState` 同样绑着旧 `Window`（焦点 / 测量 / IME 都在那边），一并重建。
    fn on_window_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.webview = None;
        self.applied = None;
        self.body = None;
        self.active = true;
        self.creating = false;
        self.rebuild_address(window, cx);
        self.start_webview(window, cx);
    }
}

impl Panel for BrowserPane {
    fn panel_name(&self) -> &'static str {
        PaneKind::Browser.panel_name()
    }

    /// 切走 Tab：子窗口必须立刻藏起来，否则它会浮在别的 Pane 上面
    fn set_active(&mut self, active: bool, _window: &mut Window, cx: &mut Context<Self>) {
        if self.active == active {
            return;
        }
        self.active = active;
        if let Some(webview) = &self.webview {
            let _ = webview.set_visible(active);
            if active {
                let _ = webview.focus();
            }
        }
        // 记下来，免得下一帧又原样写一遍
        if let Some((rect, _)) = self.applied {
            self.applied = Some((rect, active));
        }
        cx.notify();
    }

    fn dump(&self, _cx: &App) -> PanelState {
        super::pane_state(PaneKind::Browser, serde_json::json!({ "url": self.url }))
    }

    /// 关掉 Pane：立刻放掉 WebView2（连同它的子窗口和渲染进程）
    fn on_removed(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.webview = None;
        self.applied = None;
        self.creating = false;
        cx.notify();
    }
}

impl EventEmitter<PanelEvent> for BrowserPane {}

impl Focusable for BrowserPane {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for BrowserPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 懒起：一进来就排一次创建（真正的构建在前台任务里）
        if self.webview.is_none() && !self.creating && self.error.is_none() {
            self.start_webview(window, cx);
        }
        // 页面自己跳走过：把地址栏跟上（改别的实体放这轮之后，别在 render 里做）
        if let Some(url) = self.seen.borrow_mut().take()
            && url != self.url
        {
            self.url = url.clone();
            let address = self.address.clone();
            window.defer(cx, move |window, cx| {
                address.update(cx, |state, cx| state.set_value(url, window, cx));
            });
        }

        let address = self.address.clone();
        let error = self.error.clone();
        let has_webview = self.webview.is_some();
        let chrome = h_flex()
            .h(px(30.))
            .flex_shrink_0()
            .items_center()
            .gap_1()
            .px_2()
            .border_b_1()
            .border_color(rgb(LINE))
            .bg(rgb(CARD))
            .child(self.nav_button("browser-back", IconAll::ChevronLeft, cx))
            .child(self.nav_button("browser-forward", IconAll::ChevronRight, cx))
            .child(self.nav_button("browser-reload", IconAll::RotateCw, cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .items_center()
                    .px_1()
                    .child(Input::new(&address).appearance(false).bordered(false).small()),
            )
            .child(
                div()
                    .id(("browser-go", cx.entity_id()))
                    .h_full()
                    .flex_shrink_0()
                    .px_1()
                    .flex()
                    .items_center()
                    .rounded_md()
                    .text_color(rgb(INK_SOFT))
                    .hover(|d| d.bg(rgb(HOVER)).text_color(rgb(INK)))
                    .child(Icon::new(IconAll::ArrowUpRight).small())
                    .on_click(cx.listener(|this, _, window, cx| this.navigate(window, cx))),
            );

        // 正文区：WebView2 是原生子窗口，这里只画「它不在时」的占位底
        let placeholder: Option<SharedString> = match (&error, has_webview) {
            (Some(error), _) => Some(error.clone()),
            (None, false) if headless() => Some("无头模式不启动浏览器".into()),
            (None, false) => Some("正在启动 WebView2…".into()),
            (None, true) => None,
        };
        let app = self.app.clone();
        let body = div()
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .bg(rgb(PAPER))
            // 点正文区把键盘焦点交给网页。只挂在正文上：挂在包着地址栏的外层会把
            // 「点地址栏」的点击冒泡到这里，光标一进去就被抢回 WebView2。
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.focus_handle.focus(window, cx);
                    if let Some(webview) = &this.webview {
                        let _ = webview.focus();
                    }
                    cx.notify();
                }),
            )
            .on_prepaint({
                let handle = cx.entity();
                move |bounds, window, cx| {
                    let scale = window.scale_factor();
                    let rect: Physical = (
                        (f32::from(bounds.origin.x) * scale).round() as i32,
                        (f32::from(bounds.origin.y) * scale).round() as i32,
                        (f32::from(bounds.size.width) * scale).round().max(1.) as u32,
                        (f32::from(bounds.size.height) * scale).round().max(1.) as u32,
                    );
                    // 盖在子窗口上面的东西：对话框（命令面板等）+ 下拉菜单 + Dock 拖拽的落点预览
                    let covered = window.has_active_dialog(cx)
                        || cx.has_active_drag()
                        || gpui_base::GlobalState::is_in_deferred_context(cx)
                        || app.upgrade().is_some_and(|app| {
                            let app = app.read(cx);
                            app.settings_open || app.review.is_some() || app.rewind.is_some()
                        });
                    handle.update(cx, |pane, _| pane.on_body(rect, covered));
                }
            })
            .when_some(placeholder, |d, text| {
                d.child(
                    div()
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .px_6()
                        .text_size(TEXT_SMALL)
                        .text_color(rgb(INK_FAINT))
                        .child(text),
                )
            });

        div()
            .id(("browser-pane", cx.entity_id()))
            .track_focus(&self.focus_handle)
            .size_full()
            .overflow_hidden()
            .v_flex()
            .child(chrome)
            .child(body)
    }
}

impl ChatApp {
    /// `Ctrl+Shift+B`：起一块（或聚焦已有的）浏览器 Pane。
    pub(crate) fn open_browser(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.focus_pane_of_kind(PaneKind::Browser, window, cx) {
            return;
        }
        let app = cx.entity().downgrade();
        let pane = cx.new(|cx| BrowserPane::new(app, None, window, cx));
        let dock = self.dock.clone();
        let handle = std::sync::Arc::new(super::PaneHandle::new(pane))
            as std::sync::Arc<dyn gpui_base::dock::PanelView>;
        dock.update(cx, |dock, cx| {
            dock.add_panel_view(handle, gpui_base::dock::DockPlacement::Center, None, window, cx);
        });
        cx.notify();
    }
}

/// 地址栏：建 `InputState` + 回车导航的订阅。首建和换窗口都走它，
// 因为 `InputState` 绑着创建时的 `Window`（焦点 / 测量 / IME 都在那边）。
fn address_input(
    url: &str,
    window: &mut Window,
    cx: &mut Context<BrowserPane>,
) -> Entity<InputState> {
    let address = cx.new(|cx| InputState::new(window, cx).placeholder("输入网址或搜索…"));
    // 地址栏回车 = 导航（和右侧 → 按钮等价）
    cx.subscribe_in(
        &address,
        window,
        |this: &mut BrowserPane, _, event: &InputEvent, window, cx| {
            if matches!(event, InputEvent::PressEnter { secondary, .. } if !secondary) {
                this.navigate(window, cx);
            }
        },
    )
    .detach();
    if !url.is_empty() {
        address.update(cx, |state, cx| state.set_value(url.to_string(), window, cx));
    }
    address
}

// ---------- 快捷键透传 ----------

/// 挂 `AcceleratorKeyPressed`：焦点在网页里时把工作区级组合键交回 GPUI。
///
/// 只在 GPUI 真处理了才 `Handled(true)`，其余（`Ctrl+C`、页面自己的快捷键）原样留给网页。
#[cfg(target_os = "windows")]
fn install_accelerators(
    webview: &WebView,
    window: &mut Window,
    focus: &FocusHandle,
    cx: &mut Context<BrowserPane>,
) {
    use webview2_com::AcceleratorKeyPressedEventHandler;
    use wry::WebViewExtWindows as _;

    let mut app = cx.to_async();
    let handle = window.window_handle();
    let focus = focus.clone();
    let mut token = 0i64;
    // SAFETY: controller() 来自建好的 WebView2；回调只在这一条 webview 的键事件里跑，
    // 且 App 是 try_borrow（忙则放弃转发），不会与绘制栈抢借用。
    unsafe {
        let _ = webview.controller().add_AcceleratorKeyPressed(
            &AcceleratorKeyPressedEventHandler::create(Box::new(move |_, args| {
                let Some(args) = args else { return Ok(()) };
                let Some(keystroke) = shortcut_of(&args) else {
                    return Ok(());
                };
                let consumed = app
                    .update_window(handle, |_, window, cx| {
                        // 先让 GPUI 的焦点落到 Pane 自己身上：点进网页时鼠标被 WebView2
                        // 的子窗口截走，GPUI 的焦点还停在主输入框上，直接分发会让
                        // Ctrl+V / Ctrl+Z 改到聊天草稿；Pane 不是输入框，它只会把键往
                        // 上冒到 ChatApp 键上下文。
                        focus.focus(window, cx);
                        let consumed = window.dispatch_keystroke(keystroke, cx);
                        if consumed {
                            // GPUI 收下了这条键，系统键盘焦点还在子窗口上：
                            // 交还主窗口，否则用户接着打字会打进网页
                            return_gui_focus(window);
                        }
                        consumed
                    })
                    .unwrap_or(false);
                if consumed {
                    // `Handled` 是 propget（读回 WebView2 的当前值），设置要用 SetHandled
                    let _ = args.SetHandled(true);
                }
                Ok(())
            })),
            &mut token,
        );
    }
}

#[cfg(not(target_os = "windows"))]
fn install_accelerators(
    _: &WebView,
    _: &mut Window,
    _: &FocusHandle,
    _: &mut Context<BrowserPane>,
) {
}

/// 把系统键盘焦点从 WebView2 子窗口交还主窗口的客户区
#[cfg(target_os = "windows")]
fn return_gui_focus(window: &Window) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;

    let Ok(handle) = raw_window_handle::HasWindowHandle::window_handle(window) else {
        return;
    };
    if let raw_window_handle::RawWindowHandle::Win32(win32) = handle.as_raw() {
        // SAFETY: hwnd 是 GPUI 自己的窗口（同线程），SetFocus 对它安全
        unsafe {
            let _ = SetFocus(Some(HWND(win32.hwnd.get() as *mut _)));
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn return_gui_focus(_: &Window) {}

/// 这一条按键要不要交给 GPUI：只接「按住 Ctrl 或 Alt」的组合键，纯打字留给网页。
#[cfg(target_os = "windows")]
fn shortcut_of(
    args: &webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2AcceleratorKeyPressedEventArgs,
) -> Option<Keystroke> {
    use webview2_com::Microsoft::Web::WebView2::Win32::COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN;
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL, VK_MENU, VK_SHIFT};

    let mut kind = Default::default();
    // SAFETY: args 在这个回调里有效
    unsafe { args.KeyEventKind(&mut kind).ok()? };
    if kind != COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN {
        return None;
    }
    let mut vk = 0u32;
    // SAFETY: 同上
    unsafe { args.VirtualKey(&mut vk).ok()? };
    // SAFETY: GetKeyState 只是查当前线程的键状态
    let down = |vk: u16| unsafe { GetKeyState(vk as i32) } < 0;
    let modifiers = Modifiers {
        control: down(VK_CONTROL.0),
        alt: down(VK_MENU.0),
        shift: down(VK_SHIFT.0),
        ..Modifiers::none()
    };
    if !modifiers.control && !modifiers.alt {
        return None;
    }
    Some(Keystroke { modifiers, key: key_name(vk)?, key_char: None })
}

/// 虚拟键 → GPUI 认的键名。走 `MapVirtualKeyW` 跟着用户的键盘布局走，
/// 不写死「VK 0xBA 就是分号」这种美式键盘假设。
#[cfg(target_os = "windows")]
fn key_name(vk: u32) -> Option<String> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{MAPVK_VK_TO_CHAR, MapVirtualKeyW};

    // SAFETY: 纯查表，不改任何状态
    let raw = unsafe { MapVirtualKeyW(vk, MAPVK_VK_TO_CHAR) };
    // 最高位表示死键（等下一个键才出字符），这种不转发
    if raw & 0x8000_0000 != 0 {
        return None;
    }
    let ch = char::from_u32(raw & 0xFFFF)?;
    if !ch.is_ascii() || ch.is_ascii_control() {
        return None;
    }
    Some(ch.to_ascii_lowercase().to_string())
}

/// `CLAUDESKTOP_HEADLESS` / 截图模式下不拉 WebView2：验收和截图不该起 Edge 进程
fn headless() -> bool {
    ["CLAUDESKTOP_SCREENSHOT", "CLAUDESKTOP_HEADLESS"]
        .iter()
        .any(|key| std::env::var_os(key).is_some_and(|v| v != "0"))
}

/// WebView2 用户数据目录：跟设置同一个数据根，不写 exe 旁边
fn data_dir() -> Option<PathBuf> {
    let base = claudesktop_core::default_data_dir().ok()?;
    let dir = base.join(DATA_DIR);
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// 地址栏输入 → 真正的 URL：带 scheme 的原样用，本机地址补 `http://`，
/// 像域名的补 `https://`，其余当搜索词
fn normalize_url(typed: &str) -> String {
    if typed.contains("://") || typed.starts_with("about:") || typed.starts_with("file:") {
        return typed.to_string();
    }
    let looks_like_host =
        !typed.contains(char::is_whitespace) && (typed.contains('.') || is_local(typed));
    if looks_like_host {
        // 本地 dev server 基本跑 plain http，补 https 会直接连不上
        let scheme = if is_local(typed) { "http" } else { "https" };
        return format!("{scheme}://{typed}");
    }
    // 搜索：不做「猜你想搜什么」的追踪，直接走关键词页
    format!("https://duckduckgo.com/?q={}", percent_encode(typed))
}

/// 本机地址：`localhost` / `localhost:3000` / `127.0.0.1:8080` / `[::1]:3000`
fn is_local(typed: &str) -> bool {
    let authority = typed.split('/').next().unwrap_or_default();
    let host = match authority.strip_prefix('[') {
        // IPv6 字面量：[::1]:3000
        Some(rest) => rest.split(']').next().unwrap_or_default(),
        None => authority.split(':').next().unwrap_or_default(),
    };
    host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1"
}

/// 查询串编码：ASCII 里除了保留字符原样，其余（中文、空格）都转义
fn percent_encode(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() * 3);
    for byte in raw.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            b' ' => out.push('+'),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// URL 里的主机名，用来当 Pane 标题
fn host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = host.rsplit_once('@').map_or(host, |(_, host)| host);
    let host = host.split_once(':').map_or(host, |(host, _)| host);
    (!host.is_empty() && url != "about:blank").then(|| host.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // `use gpui_kit::*` 把 gpui 的 `test` 宏也带进来了，裸 `#[test]` 会展开成无限递归
    #[core::prelude::v1::test]
    fn address_bar_input_becomes_a_url() {
        assert_eq!(normalize_url("example.com"), "https://example.com");
        assert_eq!(normalize_url("https://example.com/a"), "https://example.com/a");
        // 带空格的一律当搜索词
        assert!(normalize_url("rust borrow checker").starts_with("https://duckduckgo.com/?q="));
        // 中文必须百分号编码，不能原样塞进查询串
        assert!(!normalize_url("生命周期").contains('生'));
    }

    #[core::prelude::v1::test]
    fn local_dev_servers_get_plain_http() {
        // 本机地址补 https 会直接连不上（dev server 基本没有证书）
        assert_eq!(normalize_url("localhost"), "http://localhost");
        assert_eq!(normalize_url("localhost:3000"), "http://localhost:3000");
        assert_eq!(normalize_url("LOCALHOST:8080/app"), "http://LOCALHOST:8080/app");
        assert_eq!(normalize_url("127.0.0.1:5173"), "http://127.0.0.1:5173");
        assert_eq!(normalize_url("[::1]:3000"), "http://[::1]:3000");
        // 普通站点仍是 https
        assert_eq!(normalize_url("example.com:8443"), "https://example.com:8443");
        // 带空格的 localhost 不是地址，是搜索词
        assert!(normalize_url("localhost 端口").starts_with("https://duckduckgo.com/?q="));
    }

    #[core::prelude::v1::test]
    fn title_falls_back_to_the_host() {
        assert_eq!(host_of("https://example.com/a/b").as_deref(), Some("example.com"));
        assert_eq!(host_of("https://user@example.com:8080/x").as_deref(), Some("example.com"));
        assert_eq!(host_of("about:blank"), None);
    }
}

//! 自绘窗口框架：不用系统标题栏，侧栏一直延伸到窗口顶，会话区顶栏兼做标题栏。
//!
//! Windows 的 hit-test 只看鼠标下的元素链里有没有 `Drag` 区：拖拽区包住的按钮
//! 一律收不到点击。所以拖拽区只能是按钮的**兄弟**（填空白的 flex_1），不能当容器。

use super::menu::{menu_action, menu_item, menu_popover, MenuChip};
use super::ChatApp;
use crate::model::ViewMode;
use crate::theme::*;
use crate::theme::rgb;
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::base::PopoverState;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

/// 顶部一行的高度：侧栏头部与会话区顶栏对齐
pub(super) const CHROME_H: Pixels = px(44.);

/// macOS 红绿灯占位（窗口按钮由系统画在左上角）
const TRAFFIC_LIGHT_W: Pixels = px(72.);

/// 拖动窗口的空白区（Windows 上双击最大化由系统处理）；里面只能放不可点的内容。
pub(super) fn drag_area() -> Div {
    div()
        .flex_1()
        .min_w_0()
        .h_full()
        .window_control_area(WindowControlArea::Drag)
        .when(!cfg!(target_os = "windows"), |d| {
            d.on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move())
        })
}

/// 左上角给系统红绿灯留的位置（只有 macOS 需要）
pub(super) fn traffic_light_gap() -> Option<Div> {
    cfg!(target_os = "macos").then(|| div().w(TRAFFIC_LIGHT_W).flex_shrink_0())
}

/// 最小化 / 最大化（还原）/ 关闭。Windows 靠 hit-test 区域交给系统处理点击；
/// 其他平台自己响应。macOS 用系统红绿灯，不画。
pub(super) fn window_controls(window: &Window) -> Option<Div> {
    if cfg!(target_os = "macos") {
        return None;
    }
    let button = |id: &'static str, icon: IconAll, area: WindowControlArea, close: bool| {
        div()
            .id(id)
            .w(px(46.))
            .h_full()
            .flex_shrink_0()
            .flex()
            .justify_center()
            .items_center()
            .text_color(rgb(INK_SOFT))
            .when(close, |d| d.hover(|d| d.bg(rgb(DANGER)).text_color(rgb(WHITE))))
            .when(!close, |d| d.hover(|d| d.bg(rgb(HOVER)).text_color(rgb(INK))))
            .window_control_area(area)
            .when(!cfg!(target_os = "windows"), |d| {
                d.on_click(move |_, window, _| match area {
                    WindowControlArea::Min => window.minimize_window(),
                    WindowControlArea::Max => window.zoom_window(),
                    _ => window.remove_window(),
                })
            })
            .child(Icon::new(icon).small())
    };
    let max_icon = if window.is_maximized() {
        IconAll::WindowRestore
    } else {
        IconAll::WindowMaximize
    };
    Some(
        div()
            .h_flex()
            .h_full()
            .flex_shrink_0()
            .child(button("win-min", IconAll::WindowMinimize, WindowControlArea::Min, false))
            .child(button("win-max", max_icon, WindowControlArea::Max, false))
            .child(button("win-close", IconAll::WindowClose, WindowControlArea::Close, true)),
    )
}

impl ChatApp {
    /// 会话区顶栏：「项目 / 会话标题」面包屑 + 视图模式 + 导出 + 窗口按钮。整行除按钮外都能拖动窗口。
    pub(super) fn render_header(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let s = self.session();
        let crumb = div()
            .h_flex()
            .h_full()
            .items_center()
            .gap_1p5()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_size(TEXT_BODY);
        // 设置页自己有大标题，顶栏只留拖拽区
        let crumb = if self.settings_open {
            crumb
        } else {
            let project = s
                .cwd
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| self.project_name.to_string());
            crumb
                .child(Icon::new(IconAll::Folder).size_3p5().text_color(rgb(INK_SOFT)))
                .child(div().flex_shrink_0().text_color(rgb(INK_SOFT)).child(project))
                .child(div().flex_shrink_0().text_color(rgb(INK_FAINT)).child("/"))
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .text_color(rgb(INK))
                        .font_weight(FontWeight::MEDIUM)
                        .child(s.title.clone()),
                )
        };
        let can_export = !self.settings_open && s.has_user_message();
        let view = self.settings.view_mode;

        div()
            .h_flex()
            .h(CHROME_H)
            .flex_shrink_0()
            .items_center()
            .when(!self.sidebar_open, |d| {
                d.children(traffic_light_gap())
                    .pl_2()
                    .child(self.sidebar_toggle("header-sidebar-toggle", cx))
            })
            .child(drag_area().pl_4().pr_2().child(crumb))
            // 视图模式（Ctrl+O 循环）：只在有对话时出现
            .when(can_export, |d| {
                d.child(
                    div().mr_1().child(menu_popover(
                        "view-menu",
                        Anchor::TopRight,
                        px(280.),
                        &cx.entity(),
                        MenuChip::new(view.icon(), view.label()),
                        view_menu_items,
                    )),
                )
            })
            .when(can_export, |d| {
                d.child(
                    div()
                        .id("share-session")
                        .size(px(28.))
                        .mr_2()
                        .flex_shrink_0()
                        .rounded_md()
                        .flex()
                        .justify_center()
                        .items_center()
                        .cursor_pointer()
                        .hover(|d| d.bg(rgb(HOVER)))
                        .tooltip(|window, cx| Tooltip::new("导出为 Markdown").build(window, cx))
                        .on_click(cx.listener(|this, _, window, cx| this.share_session(window, cx)))
                        .child(Icon::new(IconAll::Share).size_4().text_color(rgb(INK_SOFT))),
                )
            })
            .children(window_controls(window))
    }
}

/// 视图模式条目：摘要 / 标准 / 详细，末尾提示快捷键。
fn view_menu_items(
    app: &ChatApp,
    handle: &Entity<ChatApp>,
    popover: &Entity<PopoverState>,
) -> Div {
    let current = app.settings.view_mode;
    let mut menu = div().v_flex().gap_0p5();
    for (i, mode) in ViewMode::ALL.into_iter().enumerate() {
        let (handle, popover) = (handle.clone(), popover.clone());
        menu = menu.child(
            menu_item(("view-item", i), mode.icon(), mode.label(), mode.desc(), mode == current)
                .on_click(menu_action!(handle, popover, |this, cx| this.set_view_mode(mode, cx))),
        );
    }
    menu.child(
        div()
            .px_2p5()
            .py_1p5()
            .text_size(TEXT_META)
            .text_color(rgb(INK_SOFT))
            .child("Ctrl+O 循环切换"),
    )
}

//! 自绘窗口框架：不用系统标题栏，侧栏一直延伸到窗口顶，会话区顶栏兼做标题栏。
//!
//! Windows 的 hit-test 只看鼠标下的元素链里有没有 `Drag` 区：拖拽区包住的按钮
//! 一律收不到点击。所以拖拽区只能是按钮的**兄弟**（填空白的 flex_1），不能当容器。

use super::ChatApp;
use super::keys::{CHANGES_KEY, FILES_KEY};
use super::menu::{MenuList, menu_item, menu_panel};
use super::project_label;
use super::widgets::Surface;
use crate::theme::rgb;
use crate::theme::*;
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::base::PopoverState;
use gpui_kit::component::popover::Popover;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

/// 顶部一行的高度：侧栏头部与会话区顶栏对齐
pub(super) const CHROME_H: Rems = rems(3.);

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

/// macOS 红绿灯摆在哪（窗口左上角为原点）。gpui-kit `TitleBar::title_bar_options` 给的 (9, 9)
/// 是按默认字号（根字号 16px）下 44px 高的顶栏摆的；`CHROME_H` 跟字号缩放，顶栏变高后
/// 红绿灯往下挪半个增量，留在顶栏中线上（系统画的按钮本身不缩放）。
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(super) fn traffic_light_origin(rem: Pixels) -> Point<Pixels> {
    let grow = CHROME_H.to_pixels(rem) - CHROME_H.to_pixels(px(16.));
    point(px(9.), px(9.) + grow * 0.5)
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
    let max_icon =
        if window.is_maximized() { IconAll::WindowRestore } else { IconAll::WindowMaximize };
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
    /// 会话区顶栏：「项目 / 会话标题」面包屑 + 右侧动作（工作区面板、`⋯` 会话菜单）+ 窗口按钮。
    /// 整行除按钮外都能拖动窗口。
    pub(super) fn render_header(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let s = self.session();
        let crumb_branch =
            (!s.is_chat() && !s.task && s.worktree.is_none() && s.has_user_message())
                .then(|| s.git_branch.clone())
                .flatten();
        let crumb = div()
            .h_flex()
            .h_full()
            .items_center()
            .gap_1p5()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_size(TEXT_BODY);
        // 设置页有自己的分类页头，顶栏只留拖拽区
        let crumb = if self.settings_open {
            crumb
        } else {
            // 任务会话没有项目：面包屑里写「任务」，不拿默认目录的名字冒充项目；
            // 聊天会话没有工作目录，写「聊天」
            let (icon, project) = if s.is_chat() {
                (IconAll::MessageSquare, SharedString::from("聊天"))
            } else if s.task {
                (IconAll::ListTodo, SharedString::from("任务"))
            } else {
                (IconAll::Folder, project_label(s.group_dir()))
            };
            // 顶栏窄到放不下「项目 / 标题」时只留标题（项目名在侧栏分组里也有）：
            // 带省略号的文字缩不到零，留着项目名会把标题挤没
            let sidebar = if self.sidebar_open {
                self.sidebar_width().to_pixels(window.rem_size())
            } else {
                px(0.)
            };
            let compact =
                window.viewport_size().width - sidebar < rems(25.).to_pixels(window.rem_size());
            crumb
                .when(!compact, |crumb| {
                    crumb
                        .child(Icon::new(icon).size_3p5().text_color(rgb(INK_SOFT)))
                        // 宽度不够时先缩标题（至少留几个字），标题缩到头了再缩项目名
                        .child(
                            div()
                                .min_w_0()
                                .map(|mut d| {
                                    d.style().flex_shrink = Some(0.01);
                                    d
                                })
                                .overflow_hidden()
                                .text_ellipsis()
                                .text_color(rgb(INK_SOFT))
                                .child(project),
                        )
                        .when_some(crumb_branch, |crumb, branch| {
                            crumb.child(
                                div()
                                    .flex_shrink_0()
                                    .min_w_0()
                                    .max_w(rems(10.))
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .font_family(MONO)
                                    .text_size(TEXT_META)
                                    .text_color(rgb(INK_SOFT))
                                    .child(format!("({branch})")),
                            )
                        })
                        .child(div().flex_shrink_0().text_color(rgb(INK_FAINT)).child("/"))
                })
                .child(
                    div()
                        .min_w(rems(5.))
                        .overflow_hidden()
                        .text_ellipsis()
                        .text_color(rgb(INK))
                        .font_weight(FontWeight::MEDIUM)
                        .child(s.title.clone()),
                )
        };
        let can_export = !self.settings_open && s.has_user_message();
        // 这个会话在跑的后台命令：顶栏给一个胶囊（点开是清单，见 `bgtasks_view`）
        let running_count = self
            .kernels
            .iter()
            .flat_map(|(_, slot)| slot.tasks.iter())
            .filter(|t| t.running && s.acp_id.as_deref() == Some(t.session.as_str()))
            .count();
        // 设置页有自己的页头，右上不挂会话动作；空白聊天也没有可做的动作
        let has_header_actions = !self.settings_open
            && (!s.is_chat()
                || can_export
                || running_count > 0
                || (s.has_user_message() && s.worktree.is_some()));
        let panel_open = self.right_panel_controls();
        let panel_tip: SharedString = if panel_open {
            "收起工作区面板".into()
        } else {
            format!(
                "工作区面板（文件 {} / 改动 {}）",
                Self::key_text(FILES_KEY),
                Self::key_text(CHANGES_KEY)
            )
            .into()
        };

        div()
            .h_flex()
            .h(CHROME_H)
            .flex_shrink_0()
            .items_center()
            .when(!self.sidebar_open && !self.settings_navigation_only(), |d| {
                d.children(traffic_light_gap())
                    .pl_2()
                    .child(self.sidebar_toggle("header-sidebar-toggle", cx))
            })
            .child(drag_area().pl_4().pr_2().child(crumb))
            // 跑在 git worktree 里：面包屑后面一个分支 chip（点开是打开目录 / 合并 / 删除）。
            // 分支名很长，chip 自己可缩（`worktree_chip`）；外层不设 min_w_0，让它的最小宽度
            // 正好等于 chip 的最小宽度，缩到头就停，别让 chip 画到旁边的按钮上
            .when_some(
                super::worktree::render_worktree_chip(self, &cx.entity(), &self.menu_focus),
                |d, chip| d.child(div().mr_1().child(chip)),
            )
            // 后台命令在跑：胶囊给条数，点开是清单（和输入区停靠条同时出现）。
            // 设置页有自己的一块，不残留
            .when(!self.settings_open, |d| match self.render_bg_tasks_pill(cx) {
                Some(pill) => d.child(div().mr_1().child(pill)),
                None => d,
            })
            // 文件 / 改动共用一个开关：开的时候停在上次那个 Tab（对话里的改动卡片仍直达「改动」）
            .when(!self.settings_open && !s.is_chat(), |d| {
                d.child(
                    Self::quiet_button(
                        "toggle-right-panel",
                        IconAll::PanelRight,
                        Surface::Paper,
                        cx,
                    )
                    .mr_1()
                    .tooltip(panel_tip)
                    .when(panel_open, |b| {
                        b.icon(Icon::new(IconAll::PanelRight).text_color(rgb(CORAL)))
                    })
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_right_panel_open(cx))),
                )
            })
            // 导出 / 在项目里继续这类会话动作收进 `⋯`，只在有对话时出现
            .when(can_export, |d| d.child(div().mr_1().child(self.render_session_menu(cx))))
            // 空白 Code 首页（只有面板开关）也要有分隔线；设置页和空白聊天页不画
            .when(!panel_open && window_controls(window).is_some() && has_header_actions, |d| {
                d.child(div().w(px(1.)).h(rems(1.)).mr_1().bg(rgb(LINE)))
            })
            .when(!panel_open, |d| d.children(window_controls(window)))
    }

    /// 顶栏的 `⋯` 会话菜单：在项目里继续（仅聊天）、导出 Markdown。
    fn render_session_menu(&self, cx: &mut Context<Self>) -> Popover {
        let handle = cx.entity();
        let focus = self.menu_focus.clone();
        let trigger = Self::quiet_button("session-more", IconAll::Ellipsis, Surface::Paper, cx)
            .tooltip("更多操作");
        menu_panel(Popover::new("session-more-menu"), &self.menu_focus)
            .anchor(Anchor::TopRight)
            .w(rems(15.))
            .trigger(trigger)
            .content(move |_, window, cx| {
                let popover = cx.entity();
                session_menu_items(handle.read(cx), &handle, &popover).finish(&focus, window, cx)
            })
    }
}

/// `⋯` 里的会话动作：「在项目里继续…」（聊天才有，接进 Code 会话，见设计说明第八章）、
/// 「导出为 Markdown」。
fn session_menu_items(
    app: &ChatApp,
    handle: &Entity<ChatApp>,
    popover: &Entity<PopoverState>,
) -> MenuList {
    let chat = app.session().is_chat();
    let mut menu = MenuList::new("session-menu-list", handle, popover);
    if chat {
        menu = menu.pick_in(
            menu_item(
                "session-continue",
                IconAll::FolderOpen,
                "在项目里继续…",
                "复制到代码空间的新会话",
                false,
            ),
            |this, window, cx| this.continue_in_project(window, cx),
        );
    }
    menu.pick_in(
        menu_item("session-export", IconAll::Share, "导出为 Markdown", "", false),
        |this, window, cx| this.share_session(window, cx),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 默认字号下保持 gpui-kit 原来的位置；放大后跟着顶栏中线往下挪。
    #[core::prelude::v1::test]
    fn traffic_lights_follow_chrome_height() {
        assert_eq!(traffic_light_origin(px(16.)), point(px(9.), px(9.)));
        // 1.25 倍：顶栏 48 → 60px，长了 12px，往下挪一半
        assert_eq!(traffic_light_origin(px(20.)), point(px(9.), px(15.)));
    }
}

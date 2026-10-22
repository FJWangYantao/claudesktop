//! 自绘窗口框架：不用系统标题栏，侧栏一直延伸到窗口顶，会话区顶栏兼做标题栏。
//!
//! Windows 的 hit-test 只看鼠标下的元素链里有没有 `Drag` 区：拖拽区包住的按钮
//! 一律收不到点击。所以拖拽区只能是按钮的**兄弟**（填空白的 flex_1），不能当容器。

use super::ChatApp;
use super::keys::CYCLE_VIEW_KEY;
use super::menu::{MenuChip, MenuList, menu_item, menu_popover};
use super::widgets::Surface;
use crate::model::ViewMode;
use crate::theme::rgb;
use crate::theme::*;
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::base::PopoverState;
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
            // 任务会话没有项目：面包屑里写「任务」，不拿默认目录的名字冒充项目；
            // 聊天会话没有工作目录，写「聊天」
            let (icon, project) = if s.is_chat() {
                (IconAll::MessageSquare, "聊天".to_string())
            } else if s.task {
                (IconAll::ListTodo, "任务".to_string())
            } else {
                let label = s
                    .cwd
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| self.project_name.to_string());
                (IconAll::Folder, label)
            };
            // 顶栏窄到放不下「项目 / 标题」时只留标题（项目名在侧栏分组里也有）：
            // 带省略号的文字缩不到零，留着项目名会把标题挤没
            let sidebar = if self.sidebar_open { SIDEBAR_W } else { px(0.) };
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
        let view = self.settings.view_mode;
        // 这个会话在跑的后台命令：顶栏给一个小标（悬停看命令行，点一下停掉）
        let running: Vec<_> = self
            .kernels
            .iter()
            .flat_map(|(_, slot)| slot.tasks.iter())
            .filter(|t| t.running && s.acp_id.as_deref() == Some(t.session.as_str()))
            .collect();
        let running_count = running.len();
        let running_names: Vec<&str> = running.iter().map(|t| t.command.as_str()).collect();
        let running_ids: Vec<String> = running.iter().map(|t| t.id.clone()).collect();

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
            // 跑在 git worktree 里：面包屑后面一个分支 chip（点开是打开目录 / 合并 / 删除）。
            // 分支名很长，chip 自己可缩（`worktree_chip`）；外层不设 min_w_0，让它的最小宽度
            // 正好等于 chip 的最小宽度，缩到头就停，别让 chip 画到旁边的按钮上
            .when_some(
                super::worktree::render_worktree_chip(self, &cx.entity(), &self.menu_focus),
                |d, chip| d.child(div().mr_1().child(chip)),
            )
            // 后台命令在跑：一个小标（悬停看命令行，点一下停掉）
            .when(running_count > 0, |d| {
                d.child(
                    Self::quiet_button("bg-tasks", IconAll::SquareTerminal, Surface::Paper, cx)
                        .mr_1()
                        .tooltip(if running_count == 1 {
                            format!("后台命令在跑：{}（点击停止）", running_names[0])
                        } else {
                            format!("{running_count} 条后台命令在跑（点击全部停止）")
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            for id in &running_ids {
                                this.stop_background(id, cx);
                            }
                        })),
                )
            })
            // 视图模式（Ctrl+O 循环）：只在有对话时出现
            .when(can_export, |d| {
                d.child(div().mr_1().child(menu_popover(
                    "view-menu",
                    Anchor::TopRight,
                    rems(17.5),
                    &cx.entity(),
                    &self.menu_focus,
                    MenuChip::new(view.icon(), view.label()),
                    view_menu_items,
                )))
            })
            // 聊天会话：接到某个项目里继续（复制日志成 Code 会话，见设计说明第八章）
            .when(self.session().is_chat() && can_export, |d| {
                d.child(
                    Self::quiet_button(
                        "continue-in-project",
                        IconAll::FolderOpen,
                        Surface::Paper,
                        cx,
                    )
                    .mr_1()
                    .tooltip("在项目里继续…（把这段聊天接到一个项目的 Code 会话里）")
                    .on_click(
                        cx.listener(|this, _, window, cx| this.continue_in_project(window, cx)),
                    ),
                )
            })
            .when(can_export, |d| {
                d.child(
                    Self::quiet_button("share-session", IconAll::Share, Surface::Paper, cx)
                        .mr_2()
                        .tooltip("导出为 Markdown")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.share_session(window, cx)),
                        ),
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
) -> MenuList {
    let current = app.settings.view_mode;
    let mut menu = MenuList::new("view-menu-list", handle, popover);
    for (i, mode) in ViewMode::ALL.into_iter().enumerate() {
        menu = menu.pick(
            menu_item(("view-item", i), mode.icon(), mode.label(), mode.desc(), mode == current),
            move |this, cx| this.set_view_mode(mode, cx),
        );
    }
    menu.child(
        div()
            .h_flex()
            .items_center()
            .gap_1p5()
            .px_2p5()
            .py_1p5()
            .text_size(TEXT_META)
            .text_color(rgb(INK_SOFT))
            .child(ChatApp::kbd(CYCLE_VIEW_KEY))
            .child("循环切换"),
    )
}

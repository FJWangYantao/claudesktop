//! 会话区顶栏与底部输入区（含模式下拉）。

use super::ChatApp;
use crate::theme::*;
use gpui_kit::component::input::Input;
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

impl ChatApp {
    pub(super) fn render_top_bar(&self) -> Div {
        div()
            .h_flex()
            .items_center()
            .gap_2p5()
            .px_5()
            .py_2p5()
            .border_b_1()
            .border_color(rgb(LINE))
            .child(
                div()
                    .h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .size(px(18.))
                            .rounded_md()
                            .bg(rgb(0xC2694E))
                            .flex()
                            .justify_center()
                            .items_center()
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(rgb(0xFFFFFF))
                                    .child("甲"),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(14.5))
                            .text_color(rgb(INK))
                            .child(self.session().title.clone()),
                    ),
            )
            .child(div().flex_1())
            .child(
                div()
                    .h_flex()
                    .gap_1p5()
                    .px_2p5()
                    .py_1()
                    .rounded_full()
                    .bg(rgb(BLUE_TINT))
                    .child(
                        Icon::new(IconAll::Folder)
                            .size_3p5()
                            .text_color(rgb(BLUE)),
                    )
                    .child(
                        div()
                            .text_size(px(12.5))
                            .text_color(rgb(BLUE))
                            .child("claudesktop"),
                    ),
            )
            .child(
                div()
                    .h_flex()
                    .gap_1()
                    .px_2p5()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(LINE))
                    .bg(rgb(CARD))
                    .child(
                        Icon::new(IconAll::Bot)
                            .size_3p5()
                            .text_color(rgb(INK_SOFT)),
                    )
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(rgb(INK))
                            .child(self.session().model_label.clone()),
                    )
                    .child(
                        Icon::new(IconAll::ChevronDown)
                            .size_3()
                            .text_color(rgb(INK_SOFT)),
                    ),
            )
            .child(
                div()
                    .h_flex()
                    .gap_1p5()
                    .px_2p5()
                    .py_1()
                    .child(
                        Icon::new(IconAll::Share)
                            .size_3p5()
                            .text_color(rgb(INK_SOFT)),
                    )
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(rgb(INK_SOFT))
                            .child("分享"),
                    ),
            )
    }

    // ---------- 输入区 ----------
    /// 侧栏开关。Windows 上标题栏整条是系统拖拽区，里面的按钮收不到点击，
    /// 所以开关放在侧栏头部（展开）和左侧窄栏（收起）。
    pub(super) fn sidebar_toggle(&self, id: &'static str, cx: &mut Context<Self>) -> Stateful<Div> {
        div()
            .id(id)
            .size(px(28.))
            .flex_shrink_0()
            .rounded_md()
            .flex()
            .justify_center()
            .items_center()
            .cursor_pointer()
            .hover(|d| d.bg(rgb(SAND_ACTIVE)))
            .on_click(cx.listener(|this, _, _, cx| {
                this.sidebar_open = !this.sidebar_open;
                cx.notify();
            }))
            .child(Icon::new(IconAll::PanelLeft).size_4().text_color(rgb(INK_SOFT)))
    }

    /// 权限模式弹层：锚在输入卡片左下，向上展开。
    pub(super) fn render_mode_menu(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let mut menu = div()
            .id("mode-menu")
            .occlude()
            .absolute()
            .left(px(44.))
            .bottom(px(104.))
            .w(px(300.))
            .v_flex()
            .p_1()
            .gap_0p5()
            .bg(rgb(CARD))
            .border_1()
            .border_color(rgb(LINE))
            .rounded_lg()
            .shadow_md();
        let session = self.session();
        for (i, mode) in session.modes.iter().enumerate() {
            let active = session.permission_mode == i;
            let has_desc = !mode.desc.is_empty();
            menu = menu.child(
                div()
                    .id(("mode-item", i))
                    .h_flex()
                    .items_center()
                    .gap_2p5()
                    .px_2p5()
                    .py_2()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|d| d.bg(rgb(SAND)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.mode_menu_open = false;
                        let kernel = this.kernel.clone();
                        let s = &mut this.sessions[this.active];
                        s.permission_mode = i;
                        // 会话已建立时同步到 agent（ACP session/set_mode）
                        if let (Some(kernel), Some(id), Some(mode)) =
                            (kernel, s.acp_id.clone(), s.modes.get(i))
                        {
                            let _ = kernel.set_mode(&id, &mode.id);
                        }
                        cx.notify();
                    }))
                    .child(Icon::new(mode.icon).size_4().text_color(rgb(INK_SOFT)))
                    .child(
                        div()
                            .v_flex()
                            .flex_1()
                            .child(
                                div()
                                    .text_size(px(14.))
                                    .text_color(rgb(INK))
                                    .child(mode.name.clone()),
                            )
                            .when(has_desc, |d| {
                                d.child(
                                    div()
                                        .text_size(px(12.5))
                                        .text_color(rgb(INK_SOFT))
                                        .child(mode.desc.clone()),
                                )
                            }),
                    )
                    .when(active, |d| {
                        d.child(Icon::new(IconAll::Check).size_4().text_color(rgb(CORAL)))
                    }),
            );
        }
        menu
    }

    pub(super) fn render_composer(&self, cx: &mut Context<Self>) -> Div {
        let busy = self.session().busy;
        let send_bg = if busy { rgb(INK_SOFT) } else { rgb(CORAL) };
        let (mode_name, mode_icon): (SharedString, IconAll) = self
            .session()
            .modes
            .get(self.session().permission_mode)
            .map(|m| (m.name.clone(), m.icon))
            .unwrap_or_else(|| ("—".into(), IconAll::Gauge));
        let card = div()
            .id("composer")
            .relative()
            .v_flex()
            .w_full()
            .gap_2()
            .bg(rgb(CARD))
            .border_1()
            .border_color(rgb(LINE))
            .rounded(px(22.))
            .shadow_sm()
            .px_4()
            .pt_3()
            .pb_3()
            .child(Input::new(&self.input).appearance(false).bordered(false))
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .size(px(30.))
                            .flex_shrink_0()
                            .rounded_full()
                            .border_1()
                            .border_color(rgb(LINE))
                            .flex()
                            .justify_center()
                            .items_center()
                            .child(Icon::new(IconAll::Plus).size_4().text_color(rgb(INK_SOFT))),
                    )
                    .child(
                        div()
                            .id("mode-chip")
                            .h_flex()
                            .gap_1p5()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .cursor_pointer()
                            .when(self.mode_menu_open, |d| d.bg(rgb(SAND)))
                            .hover(|d| d.bg(rgb(SAND)))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.mode_menu_open = !this.mode_menu_open;
                                cx.notify();
                            }))
                            .child(
                                Icon::new(mode_icon)
                                    .size_3p5()
                                    .text_color(rgb(INK_SOFT)),
                            )
                            .child(
                                div()
                                    .text_size(px(13.5))
                                    .text_color(rgb(INK_SOFT))
                                    .child(mode_name),
                            )
                            .child(
                                Icon::new(IconAll::ChevronDown)
                                    .size_3()
                                    .text_color(rgb(INK_SOFT)),
                            ),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .h_flex()
                            .gap_1()
                            .px_2()
                            .py_1()
                            .child(
                                div()
                                    .text_size(px(13.5))
                                    .text_color(rgb(INK_SOFT))
                                    .child(self.session().model_label.clone()),
                            )
                            .child(
                                Icon::new(IconAll::ChevronDown)
                                    .size_3()
                                    .text_color(rgb(INK_SOFT)),
                            ),
                    )
                    .child(
                        div()
                            .id("send")
                            .size(px(32.))
                            .flex_shrink_0()
                            .rounded_full()
                            .bg(send_bg)
                            .flex()
                            .justify_center()
                            .items_center()
                            .cursor_pointer()
                            .child(
                                Icon::new(if busy {
                                    IconAll::CircleStop
                                } else {
                                    IconAll::ArrowUp
                                })
                                .size_4()
                                .text_color(rgb(0xFFFFFF)),
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                if this.session().busy {
                                    this.stop(cx);
                                } else {
                                    this.send(window, cx);
                                }
                            })),
                    ),
            )
            .when(self.mode_menu_open, |d| d.child(self.render_mode_menu(cx)));

        div()
            .v_flex()
            .max_w(px(760.))
            .mx_auto()
            .w_full()
            .gap_2p5()
            .child(card)
            .child(
                // 卡片下方：环境 + 项目
                div()
                    .h_flex()
                    .gap_2()
                    .child(Self::chip(IconAll::Monitor, "Local"))
                    .child(Self::chip(IconAll::Folder, self.project_name.clone())),
            )
    }
}

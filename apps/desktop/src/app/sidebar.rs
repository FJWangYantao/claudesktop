//! 侧栏：工作区分组 / 会话列表 / 内核卡。

use super::ChatApp;
use crate::theme::*;
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::*;
use gpui_kit::*;

impl ChatApp {
    pub(super) fn render_sidebar(&self, cx: &mut Context<Self>) -> Div {
        let mut body = v_flex().gap_0().flex_1().min_h_0().overflow_hidden();

        // 工作区头部行
        body = body.child(
            div()
                .h_flex()
                .items_center()
                .px_3()
                .pt_3()
                .pb_1()
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(rgb(INK_SOFT))
                        .child("工作区"),
                )
                .child(div().flex_1())
                .child(
                    Icon::new(IconAll::ArrowUpDown)
                        .size_3p5()
                        .text_color(rgb(INK_SOFT)),
                )
                .child(
                    div().pl_2().child(
                        Icon::new(IconAll::ExternalLink)
                            .size_3p5()
                            .text_color(rgb(INK_SOFT)),
                    ),
                ),
        );

        // 当前项目：真实会话，可点击切换
        body = body.child(
            div()
                .px_3()
                .pt_3()
                .pb_1()
                .text_size(px(13.5))
                .text_color(rgb(INK))
                .child(self.project_name.clone()),
        );
        for (i, s) in self.sessions.iter().enumerate() {
            let active = self.active == i;
            let dot = if s.busy {
                CORAL
            } else if s.ready {
                GREEN
            } else {
                LINE
            };
            body = body.child(
                div()
                    .id(("session", i))
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .mx_2()
                    .px_2()
                    .py(px(5.))
                    .rounded_md()
                    .bg(if active { rgb(SAND_ACTIVE) } else { rgba(0x00000000) })
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.active = i;
                        this.mode_menu_open = false;
                        cx.notify();
                    }))
                    .child(div().size(px(5.)).flex_shrink_0().rounded_full().bg(rgb(dot)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(px(14.))
                            .text_color(if active { rgb(INK) } else { rgb(INK_SOFT) })
                            .child(s.title.clone()),
                    ),
            );
        }

        // 其余分组：示意数据，不可点击
        for ws in &self.workspaces {
            body = body.child(
                div()
                    .px_3()
                    .pt_3()
                    .pb_1()
                    .text_size(px(13.5))
                    .text_color(rgb(INK))
                    .child(ws.name.clone()),
            );
            for s in &ws.sessions {
                let mut row = div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .mx_2()
                    .px_2()
                    .py(px(5.))
                    .rounded_md()
                    .child(
                        div()
                            .size(px(5.))
                            .flex_shrink_0()
                            .rounded_full()
                            .bg(if let Some(c) = s.dot { rgb(c) } else { rgb(LINE) }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(px(14.))
                            .text_color(rgb(INK_SOFT))
                            .child(s.title.clone()),
                    );
                if !s.age.is_empty() {
                    row = row.child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(12.))
                            .text_color(rgb(INK_FAINT))
                            .child(s.age.clone()),
                    );
                }
                body = body.child(row);
            }
        }

        // 任务分组
        body = body.child(
            div()
                .px_3()
                .pt_4()
                .pb_1()
                .text_size(px(12.))
                .text_color(rgb(INK_SOFT))
                .child("任务"),
        );
        for (i, (title, age)) in [
            ("看一下我电脑上 antigravity to…", "9 分钟"),
            ("Investigating zcode codingpla…", "1 天"),
            ("检查一下我的 dsh 的插件，有一…", "2 天"),
        ]
        .iter()
        .enumerate()
        {
            body = body.child(
                div()
                    .id(("task", i))
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .mx_2()
                    .px_2()
                    .py(px(5.))
                    .rounded_md()
                    .cursor_pointer()
                    .child(
                        div().size(px(5.)).flex_shrink_0().rounded_full().bg(rgb(LINE)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(px(14.))
                            .text_color(rgb(INK_SOFT))
                            .child(SharedString::from(*title)),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(12.))
                            .text_color(rgb(INK_FAINT))
                            .child(SharedString::from(*age)),
                    ),
            );
        }

        v_flex()
            .w(px(248.))
            .flex_shrink_0()
            .h_full()
            .bg(rgb(SAND))
            .border_r_1()
            .border_color(rgb(LINE))
            .child(
                // 品牌行
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .pt_3()
                    .pb_1()
                    .child(
                        Icon::new(IconAll::Sparkles)
                            .size_4()
                            .text_color(rgb(CORAL)),
                    )
                    .child(
                        div()
                            .font_family(SERIF)
                            .text_size(px(18.))
                            .text_color(rgb(INK))
                            .child("Claude"),
                    )
                    .child(div().flex_1())
                    .child(self.sidebar_toggle("sidebar-toggle", cx)),
            )
            .child(
                div()
                    .id("new-chat")
                    .h_flex()
                    .gap_2()
                    .mx_2()
                    .mt_2()
                    .px_2()
                    .py_1p5()
                    .rounded_md()
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.add_session(cx);
                    }))
                    .child(
                        Icon::new(IconAll::SquarePen)
                            .size_3p5()
                            .text_color(rgb(INK)),
                    )
                    .child(div().text_size(px(14.)).text_color(rgb(INK)).child("新会话")),
            )
            .child(
                div()
                    .id("plugins")
                    .h_flex()
                    .gap_2()
                    .mx_2()
                    .px_2()
                    .py_1p5()
                    .rounded_md()
                    .cursor_pointer()
                    .child(
                        Icon::new(IconAll::Blocks)
                            .size_3p5()
                            .text_color(rgb(INK_SOFT)),
                    )
                    .child(div().text_size(px(14.)).text_color(rgb(INK_SOFT)).child("插件")),
            )
            .child(div().flex_1().min_h_0().child(body))
            .child(
                // 底部：更多 + 内核状态
                div()
                    .v_flex()
                    .gap_2()
                    .p_3()
                    .child(
                        div()
                            .h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                Icon::new(IconAll::Ellipsis)
                                    .size_3p5()
                                    .text_color(rgb(INK_SOFT)),
                            )
                            .child(
                                div()
                                    .text_size(px(13.5))
                                    .text_color(rgb(INK_SOFT))
                                    .child("更多"),
                            ),
                    )
                    .child(
                        div()
                            .h_flex()
                            .gap_1p5()
                            .items_center()
                            .child(div().size(px(6.)).rounded_full().bg(rgb(
                                if self.kernel_error {
                                    RED
                                } else if self.kernel_ready {
                                    GREEN
                                } else {
                                    CORAL
                                },
                            )))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .text_size(px(12.))
                                    .text_color(rgb(INK_SOFT))
                                    .child(self.kernel_status.clone()),
                            ),
                    ),
            )
    }
}

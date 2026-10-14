//! 侧栏：工作区分组 / 会话列表 / 内核卡。

use super::ChatApp;
use crate::settings::KernelChoice;
use crate::theme::*;
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

impl ChatApp {
    pub(super) fn render_sidebar(&self, cx: &mut Context<Self>) -> Div {
        let mut body = v_flex().gap_0().pb_2();

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

        // 会话：按工作目录分组，当前项目在前，其余按最近活动排序
        for (name, members) in self.session_groups() {
            body = body.child(
                div()
                    .px_3()
                    .pt_3()
                    .pb_1()
                    .text_size(px(13.5))
                    .text_color(rgb(INK))
                    .child(name),
            );
            for i in members {
                body = body.child(self.render_session_row(i, cx));
            }
        }

        // 进行中：正在跑或在等授权的会话（跨项目、跨内核），点一下切过去
        let running: Vec<usize> = (0..self.sessions.len())
            .filter(|i| self.sessions[*i].busy)
            .collect();
        if !running.is_empty() {
            body = body.child(
                div()
                    .px_3()
                    .pt_4()
                    .pb_1()
                    .text_size(px(12.))
                    .text_color(rgb(INK_SOFT))
                    .child("进行中"),
            );
            for i in running {
                let s = &self.sessions[i];
                let waiting = s.waiting_permission();
                body = body.child(
                    div()
                        .id(("running", i))
                        .h_flex()
                        .items_center()
                        .gap_2()
                        .mx_2()
                        .px_2()
                        .py(px(5.))
                        .rounded_md()
                        .cursor_pointer()
                        .hover(|d| d.bg(rgb(SAND_ACTIVE)))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.active = i;
                            this.settings_open = false;
                            cx.notify();
                        }))
                        .child(div().size(px(5.)).flex_shrink_0().rounded_full().bg(rgb(
                            if waiting { RED } else { CORAL },
                        )))
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
                        )
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_size(px(12.))
                                .text_color(rgb(if waiting { RED } else { INK_FAINT }))
                                .child(format!(
                                    "{} · {}",
                                    s.kernel.tag(),
                                    if waiting { "待授权" } else { "运行中" }
                                )),
                        ),
                );
            }
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
            .child(
                div()
                    .id("sidebar-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(body),
            )
            .child(
                // 底部：更多 + 内核状态
                div()
                    .v_flex()
                    .gap_2()
                    .p_3()
                    .child(
                        div()
                            .id("open-settings")
                            .h_flex()
                            .gap_2()
                            .items_center()
                            .px_1()
                            .py_0p5()
                            .rounded_md()
                            .cursor_pointer()
                            .when(self.settings_open, |d| d.bg(rgb(SAND_ACTIVE)))
                            .hover(|d| d.bg(rgb(SAND_ACTIVE)))
                            .on_click(cx.listener(|this, _, window, cx| {
                                if this.settings_open {
                                    this.settings_open = false;
                                    cx.notify();
                                } else {
                                    this.open_settings(window, cx);
                                }
                            }))
                            .child(
                                Icon::new(IconAll::Settings)
                                    .size_3p5()
                                    .text_color(rgb(INK_SOFT)),
                            )
                            .child(
                                div()
                                    .text_size(px(13.5))
                                    .text_color(rgb(INK_SOFT))
                                    .child("设置"),
                            ),
                    )
                    .children(self.kernel_status_rows()),
            )
    }

    /// 每个已启动内核一行：状态点 + 「自研 · xxx · 已连接」
    fn kernel_status_rows(&self) -> Vec<Div> {
        KernelChoice::ALL
            .into_iter()
            .filter(|k| self.kernels[k.index()].started)
            .map(|kind| {
                let slot = &self.kernels[kind.index()];
                let dot = if slot.error {
                    RED
                } else if slot.ready {
                    GREEN
                } else {
                    CORAL
                };
                div()
                    .h_flex()
                    .gap_1p5()
                    .items_center()
                    .child(div().size(px(6.)).flex_shrink_0().rounded_full().bg(rgb(dot)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(px(12.))
                            .text_color(rgb(INK_SOFT))
                            .child(format!("{} · {}", kind.tag(), slot.status)),
                    )
            })
            .collect()
    }

    /// 侧栏分组：(组名, 会话下标)。当前工作目录一组在最前，组内保持会话顺序。
    fn session_groups(&self) -> Vec<(SharedString, Vec<usize>)> {
        let mut groups: Vec<(&Path, Vec<usize>)> = vec![(self.cwd.as_path(), Vec::new())];
        for (i, s) in self.sessions.iter().enumerate() {
            match groups.iter_mut().find(|(cwd, _)| *cwd == s.cwd.as_path()) {
                Some((_, members)) => members.push(i),
                None => groups.push((s.cwd.as_path(), vec![i])),
            }
        }
        groups
            .into_iter()
            .enumerate()
            .map(|(g, (cwd, members))| {
                let name = if g == 0 {
                    self.project_name.clone()
                } else {
                    cwd.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| cwd.display().to_string())
                        .into()
                };
                (name, members)
            })
            .collect()
    }

    fn render_session_row(&self, i: usize, cx: &mut Context<Self>) -> Stateful<Div> {
        let s = &self.sessions[i];
        let active = self.active == i;
        let dot = if s.busy {
            CORAL
        } else if s.ready {
            GREEN
        } else {
            LINE
        };
        let age = s.meta.as_ref().map(|m| age_label(m.updated_ms));
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
                this.kernel_menu_open = false;
                this.model_menu_open = false;
                this.settings_open = false;
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
            )
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(px(11.))
                    .text_color(rgb(INK_FAINT))
                    .child(s.kernel.tag()),
            )
            .when_some(age, |row, age| {
                row.child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(12.))
                        .text_color(rgb(INK_FAINT))
                        .child(age),
                )
            })
    }
}

/// 「刚刚 / 5 分钟 / 3 小时 / 2 天」
fn age_label(updated_ms: u64) -> SharedString {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    let mins = now.saturating_sub(updated_ms) / 60_000;
    match mins {
        0 => "刚刚".into(),
        m if m < 60 => format!("{m} 分钟").into(),
        m if m < 60 * 24 => format!("{} 小时", m / 60).into(),
        m => format!("{} 天", m / (60 * 24)).into(),
    }
}

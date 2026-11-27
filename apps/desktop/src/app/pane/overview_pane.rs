//! 概览 Pane：右侧检视区默认首页，由一组按需生长的独立圆角矩形卡片组成：
//!
//! 1. **常驻概览卡**：当前分支 + 文件改动汇总（点进 `改动`）+ 上下文用量（点进 `用量`）；
//! 2. **计划卡**（仅本会话有计划时出现）：步骤清单，点标题进独立 `计划` Pane；
//! 3. **Outputs 卡**（仅本会话有改动文件时出现）：改过的文件清单，点任一行直达该文件 Diff；
//! 4. **子代理卡**（仅有子代理时出现）：运行中 / 已完成的子任务列表，点任一行看详情。

use super::{ClaudesktopPane, PaneKind};
use crate::app::ChatApp;
use crate::app::menu::diff_stat;
use crate::theme::rgb;
use crate::theme::*;
use gpui_base::dock::{Panel, PanelEvent, PanelState};
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

pub(crate) struct OverviewPane {
    app: WeakEntity<ChatApp>,
    focus_handle: FocusHandle,
}

impl OverviewPane {
    pub(crate) fn new(app: WeakEntity<ChatApp>, cx: &mut Context<Self>) -> Self {
        super::follow_hub(app.clone(), cx);
        Self { app, focus_handle: cx.focus_handle() }
    }
}

impl ClaudesktopPane for OverviewPane {
    fn pane_kind(&self) -> PaneKind {
        PaneKind::Overview
    }

    fn icon(&self, _cx: &App) -> IconAll {
        IconAll::LayoutDashboard
    }

    fn session_token(&self, cx: &App) -> Option<u64> {
        self.app.upgrade().map(|app| app.read(cx).session().token)
    }

    fn badge(&self, cx: &App) -> Option<SharedString> {
        let app = self.app.upgrade()?;
        let count = app.read(cx).right_panel.changed_count;
        (count > 0).then(|| SharedString::from(count.to_string()))
    }
}

impl Panel for OverviewPane {
    fn panel_name(&self) -> &'static str {
        PaneKind::Overview.panel_name()
    }

    fn dump(&self, _cx: &App) -> PanelState {
        super::pane_state(PaneKind::Overview, serde_json::json!({ "kind": "overview" }))
    }
}

impl EventEmitter<PanelEvent> for OverviewPane {}

impl Focusable for OverviewPane {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for OverviewPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(app) = self.app.upgrade() else {
            return div().size_full().into_any_element();
        };
        app.update(cx, |app, cx| app.render_overview_tab(window, cx))
    }
}

/// 一张独立圆角矩形卡片的外壳：纸卡底 + 1px 细边 + 贴地阴影，融入与中间聊天区统一的 PAPER 画布。
fn card_box() -> Div {
    div()
        .v_flex()
        .rounded_xl()
        .border_1()
        .border_color(rgb(LINE))
        .bg(rgb(CARD))
        .shadow(raised_shadow())
        .p_3()
        .gap_2()
}

/// 卡片内可点行：悬停高亮 + 右侧 chevron。
fn card_action_row(id: impl Into<ElementId>) -> Stateful<Div> {
    div()
        .id(id)
        .h_flex()
        .items_center()
        .justify_between()
        .gap_2()
        .px_2()
        .py_1()
        .mx(px(-8.))
        .rounded_md()
        .cursor_pointer()
        .hover(|d| d.bg(rgb(HOVER)))
}

impl ChatApp {
    /// 概览 Pane 正文：按条件纵向堆叠多张独立圆角矩形卡片。
    pub(crate) fn render_overview_tab(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let s = self.session();
        let origin = (s.token, s.diff_epoch);
        let cwd = s.cwd.clone();
        let branch: SharedString = if let Some(wt) = s.worktree.as_ref() {
            wt.branch.clone().into()
        } else if let Some(b) = s.git_branch.as_ref() {
            b.clone()
        } else if s.git_detached {
            "HEAD".into()
        } else if s.task {
            "任务".into()
        } else {
            crate::app::project_label(s.group_dir())
        };

        let changed_count = self.right_panel.changed_count;
        let turn_changes = self.right_panel.turn_changes.clone();
        let (total_added, total_removed) =
            turn_changes.iter().fold((0u32, 0u32), |(a, r), f| (a + f.added, r + f.removed));
        let gauge = self.context_gauge();
        let cost = s.context_cost.clone();

        // ---------- 卡 1：分支 + 文件改动汇总 + 上下文用量 ----------
        let branch_row = div()
            .h_flex()
            .items_center()
            .gap_2()
            .pb_1()
            .border_b_1()
            .border_color(rgb(LINE))
            .child(Icon::new(IconAll::GitBranch).size_3p5().text_color(rgb(INK_SOFT)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .font_family(MONO)
                    .text_size(TEXT_SMALL)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(rgb(INK))
                    .child(branch),
            );

        let diff_summary_label: SharedString = if changed_count > 0 {
            format!("{changed_count} 个文件改动").into()
        } else {
            "暂无文件改动".into()
        };
        let diff_row = card_action_row("overview-diff-summary")
            .child(
                div()
                    .h_flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap_2()
                    .child(Icon::new(IconAll::FileDiff).size_3p5().text_color(rgb(INK_SOFT)))
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(TEXT_SMALL)
                            .text_color(rgb(if changed_count > 0 { INK } else { INK_SOFT }))
                            .child(diff_summary_label),
                    ),
            )
            .when(changed_count > 0, |d| d.child(diff_stat(total_added, total_removed)))
            .child(Icon::new(IconAll::ChevronRight).size_3().text_color(rgb(INK_FAINT)))
            .on_click(cx.listener(|this, _, window, cx| {
                this.open_pane(PaneKind::Diff, window, cx);
            }));

        let fill_pct = gauge.as_ref().and_then(|g| g.percent).map(|p| p.min(100) as f32 / 100.);
        let fill_color = match gauge.as_ref().and_then(|g| g.percent) {
            Some(p) if p >= 90 => RED,
            Some(p) if p >= 70 => CORAL_DEEP,
            _ => CORAL,
        };
        let usage_text: SharedString = match &gauge {
            Some(g) => match g.window {
                Some(w) => format!(
                    "{} · {}/{}",
                    g.label,
                    crate::usage::short_tokens(g.used),
                    crate::usage::short_tokens(w),
                )
                .into(),
                None => format!("{} token", crate::usage::short_tokens(g.used)).into(),
            },
            None => "尚无用量".into(),
        };
        let usage_row = card_action_row("overview-usage-row")
            .child(
                div()
                    .h_flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap_2()
                    .when_some(fill_pct, |d, pct| {
                        d.child(
                            div()
                                .w(rems(3.75))
                                .h(px(4.))
                                .flex_shrink_0()
                                .rounded_full()
                                .bg(rgb(LINE))
                                .overflow_hidden()
                                .child(div().h_full().w(rems(3.75 * pct)).bg(rgb(fill_color))),
                        )
                    })
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(TEXT_SMALL)
                            .text_color(rgb(if gauge.is_some() { INK } else { INK_FAINT }))
                            .font_features(tabular_nums())
                            .child(usage_text),
                    ),
            )
            .when_some(cost, |d, cost| {
                d.child(
                    div()
                        .flex_shrink_0()
                        .text_size(TEXT_META)
                        .text_color(rgb(INK_SOFT))
                        .font_features(tabular_nums())
                        .child(format!("{:.3} {}", cost.amount, cost.currency)),
                )
            })
            .child(Icon::new(IconAll::ChevronRight).size_3().text_color(rgb(INK_FAINT)))
            .on_click(cx.listener(|this, _, window, cx| {
                this.open_usage(window, cx);
            }));

        let summary_card = card_box().child(branch_row).child(diff_row).child(usage_row);

        // ---------- 卡 2：计划卡（有计划才长出来） ----------
        let plan_rows = self.plan_rows();
        let plan_card = (!plan_rows.is_empty()).then(|| {
            let (done, total, _) = crate::model::plan_progress(&plan_rows);
            let head = card_action_row("overview-plan-card")
                .child(
                    div()
                        .h_flex()
                        .items_center()
                        .gap_1p5()
                        .text_size(TEXT_SMALL)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(rgb(INK))
                        .child(Icon::new(IconAll::ListTodo).size_3p5().text_color(rgb(INK_SOFT)))
                        .child("计划"),
                )
                .child(
                    div()
                        .h_flex()
                        .items_center()
                        .gap_1()
                        .text_size(TEXT_META)
                        .text_color(rgb(INK_SOFT))
                        .font_features(tabular_nums())
                        .child(format!("{done}/{total}"))
                        .child(
                            Icon::new(IconAll::ChevronRight).size_3().text_color(rgb(INK_FAINT)),
                        ),
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.open_pane(PaneKind::Plan, window, cx);
                }));
            card_box().child(head).child(Self::render_plan(&plan_rows, false))
        });

        // ---------- 卡 3：Outputs / 本轮已改文件卡（改过文件才长出来，点哪行跳哪个 Diff） ----------
        let outputs_card = (!turn_changes.is_empty()).then(|| {
            let head = card_action_row("overview-outputs-card")
                .child(
                    div()
                        .h_flex()
                        .items_center()
                        .gap_1p5()
                        .text_size(TEXT_SMALL)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(rgb(INK))
                        .child(Icon::new(IconAll::FileDiff).size_3p5().text_color(rgb(INK_SOFT)))
                        .child("Outputs"),
                )
                .child(
                    div()
                        .h_flex()
                        .items_center()
                        .gap_1()
                        .text_size(TEXT_META)
                        .text_color(rgb(INK_SOFT))
                        .font_features(tabular_nums())
                        .child(turn_changes.len().to_string())
                        .child(
                            Icon::new(IconAll::ChevronRight).size_3().text_color(rgb(INK_FAINT)),
                        ),
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.open_pane(PaneKind::Diff, window, cx);
                }));
            let mut list = div().v_flex().gap_0p5();
            for (ix, file) in turn_changes.iter().enumerate() {
                let path = claudesktop_core::review::display_path(&file.path, Some(&cwd));
                let location = file.location;
                let row = card_action_row(("overview-output-file", ix))
                    .child(
                        div()
                            .h_flex()
                            .flex_1()
                            .min_w_0()
                            .items_center()
                            .gap_1p5()
                            .child(
                                Icon::new(if file.created {
                                    IconAll::FilePlus
                                } else {
                                    IconAll::FileText
                                })
                                .size_3p5()
                                .text_color(rgb(INK_SOFT)),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .font_family(MONO)
                                    .text_size(TEXT_META)
                                    .text_color(rgb(INK))
                                    .child(path),
                            ),
                    )
                    .child(diff_stat(file.added, file.removed))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let s = this.session();
                        if (s.token, s.diff_epoch) == origin {
                            this.open_right_diff(location, window, cx);
                        }
                    }));
                list = list.child(row);
            }
            card_box().child(head).child(list)
        });

        // ---------- 卡 4：子代理卡（有子代理才长出来） ----------
        let mut subagents: Vec<(String, SharedString, SharedString)> = s
            .subagents
            .iter()
            .map(|(id, run)| (id.clone(), run.agent.clone(), run.prompt.clone()))
            .collect();
        subagents.sort_by(|a, b| a.0.cmp(&b.0));
        let subagent_card = (!subagents.is_empty()).then(|| {
            let head = card_action_row("overview-subagent-card")
                .child(
                    div()
                        .h_flex()
                        .items_center()
                        .gap_1p5()
                        .text_size(TEXT_SMALL)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(rgb(INK))
                        .child(Icon::new(IconAll::GitBranch).size_3p5().text_color(rgb(INK_SOFT)))
                        .child("子代理"),
                )
                .child(
                    div()
                        .h_flex()
                        .items_center()
                        .gap_1()
                        .text_size(TEXT_META)
                        .text_color(rgb(INK_SOFT))
                        .font_features(tabular_nums())
                        .child(subagents.len().to_string())
                        .child(
                            Icon::new(IconAll::ChevronRight).size_3().text_color(rgb(INK_FAINT)),
                        ),
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.open_pane(PaneKind::Subagent, window, cx);
                }));
            let mut list = div().v_flex().gap_0p5();
            for (ix, (id, agent, prompt)) in subagents.into_iter().enumerate() {
                let row = card_action_row(("overview-subagent-row", ix))
                    .child(
                        div()
                            .h_flex()
                            .flex_1()
                            .min_w_0()
                            .items_center()
                            .gap_1p5()
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_size(TEXT_META)
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(rgb(INK))
                                    .child(if agent.is_empty() {
                                        "子代理".into()
                                    } else {
                                        agent
                                    }),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .text_size(TEXT_META)
                                    .text_color(rgb(INK_SOFT))
                                    .child(prompt),
                            ),
                    )
                    .child(Icon::new(IconAll::ChevronRight).size_3().text_color(rgb(INK_FAINT)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_task(id.clone(), cx);
                    }));
                list = list.child(row);
            }
            card_box().child(head).child(list)
        });

        let tool_chip = |kind: PaneKind| {
            div()
                .id(ElementId::Name(format!("overview-quick-{}", kind.panel_name()).into()))
                .h_flex()
                .items_center()
                .gap_1()
                .px_2()
                .py_1()
                .rounded_lg()
                .border_1()
                .border_color(rgb(LINE))
                .bg(rgb(CARD))
                .cursor_pointer()
                .text_size(TEXT_META)
                .text_color(rgb(INK_SOFT))
                .hover(|d| d.bg(rgb(HOVER)).text_color(rgb(INK)))
                .child(Icon::new(kind.icon()).size_3p5())
                .child(kind.title())
        };
        let quick_row = div()
            .h_flex()
            .flex_wrap()
            .gap_1p5()
            .child(tool_chip(PaneKind::Files).on_click(cx.listener(|this, _, window, cx| {
                this.open_pane(PaneKind::Files, window, cx);
            })))
            .child(tool_chip(PaneKind::Browser).on_click(cx.listener(|this, _, window, cx| {
                this.open_browser(window, cx);
            })))
            .child(tool_chip(PaneKind::Terminal).on_click(cx.listener(|this, _, window, cx| {
                this.open_terminal(window, cx);
            })))
            .child(tool_chip(PaneKind::SideChat).on_click(cx.listener(|this, _, window, cx| {
                this.open_side_chat(window, cx);
            })));

        div()
            .id("overview-scroll")
            .size_full()
            .overflow_y_scroll()
            .bg(rgb(PAPER))
            .p_3()
            .v_flex()
            .gap_3()
            .child(summary_card)
            .when_some(plan_card, |d, card| d.child(card))
            .when_some(outputs_card, |d, card| d.child(card))
            .when_some(subagent_card, |d, card| d.child(card))
            .child(quick_row)
            .into_any_element()
    }
}

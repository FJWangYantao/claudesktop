//! 计划 Pane：把当前会话的计划 / 待办从对话流里抽出来单独看。
//!
//! 数据来源就是对话里的 `ChatItem::Plan`（内核 `TodoWrite` 那一类工具写的），
//! 取最近一条：计划是「当前这版」，不是历史每版的累加。渲染复用 `chat::render_plan`，
//! 所以这里的样式、已完成项的淡出、进行中那一条的强调，和对话里完全一致。

use super::{ClaudesktopPane, PaneKind};
use crate::app::ChatApp;
use crate::theme::*;
use gpui_base::dock::{Panel, PanelEvent, PanelState};
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::*;
use gpui_kit::*;

pub(crate) struct PlanPane {
    app: WeakEntity<ChatApp>,
    focus_handle: FocusHandle,
}

impl PlanPane {
    pub(crate) fn new(app: WeakEntity<ChatApp>, cx: &mut Context<Self>) -> Self {
        if let Some(upgraded) = app.upgrade() {
            let hub = upgraded.read(cx).hub.clone();
            super::follow_hub(&hub, cx);
        }
        Self { app, focus_handle: cx.focus_handle() }
    }
}

impl ClaudesktopPane for PlanPane {
    fn pane_kind(&self) -> PaneKind {
        PaneKind::Plan
    }

    fn icon(&self, _cx: &App) -> IconAll {
        IconAll::ListTodo
    }

    fn session_token(&self, cx: &App) -> Option<u64> {
        self.app.upgrade().map(|app| app.read(cx).session().token)
    }

    /// Tab 上的 badge：`完成/总数`
    fn badge(&self, cx: &App) -> Option<SharedString> {
        let app = self.app.upgrade()?;
        let app = app.read(cx);
        let rows = app.plan_rows();
        let (done, total, _) = crate::model::plan_progress(&rows);
        (total > 0).then(|| SharedString::from(format!("{done}/{total}")))
    }
}

impl Panel for PlanPane {
    fn panel_name(&self) -> &'static str {
        PaneKind::Plan.panel_name()
    }

    fn dump(&self, _cx: &App) -> PanelState {
        super::pane_state(PaneKind::Plan, serde_json::json!({ "kind": "plan" }))
    }
}

impl EventEmitter<PanelEvent> for PlanPane {}

impl Focusable for PlanPane {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for PlanPane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(app) = self.app.upgrade() else {
            return div().size_full().into_any_element();
        };
        app.update(cx, |app, cx| app.render_plan_tab(cx))
    }
}

impl ChatApp {
    /// 最近一条计划（内核 `TodoWrite` 写的那一版）；没有就是空。
    /// `transcript` 里存的就是 `ChatItem`，从后往前找第一条 `Plan`。
    pub(crate) fn plan_rows(&self) -> Vec<crate::model::PlanRow> {
        self.session()
            .transcript
            .iter()
            .rev()
            .find_map(|item| match item {
                crate::model::ChatItem::Plan(rows) if !rows.is_empty() => Some(rows.to_vec()),
                _ => None,
            })
            .unwrap_or_default()
    }

    /// 计划 Pane 的正文。
    pub(crate) fn render_plan_tab(&mut self, _cx: &mut Context<Self>) -> AnyElement {
        let rows = self.plan_rows();
        if rows.is_empty() {
            return empty_note("这个会话还没有计划：让它先把步骤列出来").into_any_element();
        }
        let (done, total, current) = crate::model::plan_progress(&rows);
        div()
            .v_flex()
            .size_full()
            .gap_3()
            .p_4()
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .text_size(TEXT_SMALL)
                    .text_color(crate::theme::rgb(crate::theme::INK_SOFT))
                    .child(format!("{done}/{total} 已完成"))
                    .children(current.map(|row| {
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(format!("· 正在做：{}", row.text))
                    })),
            )
            .child(
                div()
                    .id("plan-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(Self::render_plan(&rows, false)),
            )
            .into_any_element()
    }
}

fn empty_note(text: impl Into<SharedString>) -> Div {
    div()
        .size_full()
        .p_4()
        .text_size(TEXT_SMALL)
        .text_color(crate::theme::rgb(crate::theme::INK_SOFT))
        .child(text.into())
}

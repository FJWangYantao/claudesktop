//! 子代理 Pane：当前会话的子代理（`Session::subagents`）与正在跑的子任务一览，
//! 点一条就地展开它的过程（复用 `subagent_view.rs` 的详情页）。
//!
//! 和对话里的子任务卡片是同一份数据：卡片是「顺着对话读到这一张」，
//! 这里是「一次看全部在跑的」，所以适合拖到右边或弹成独立窗盯着。

use super::{ClaudesktopPane, PaneKind};
use crate::app::ChatApp;
use crate::theme::rgb;
use crate::theme::*;
use gpui_base::dock::{Panel, PanelEvent, PanelState};
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::*;
use gpui_kit::*;

pub(crate) struct SubagentPane {
    app: WeakEntity<ChatApp>,
    focus_handle: FocusHandle,
}

impl SubagentPane {
    pub(crate) fn new(app: WeakEntity<ChatApp>, cx: &mut Context<Self>) -> Self {
        super::follow_hub(app.clone(), cx);
        Self { app, focus_handle: cx.focus_handle() }
    }
}

impl ClaudesktopPane for SubagentPane {
    fn pane_kind(&self) -> PaneKind {
        PaneKind::Subagent
    }

    fn icon(&self, _cx: &App) -> IconAll {
        IconAll::GitBranch
    }

    fn session_token(&self, cx: &App) -> Option<u64> {
        self.app.upgrade().map(|app| app.read(cx).session().token)
    }

    /// Tab 上的 badge：有子代理在跑就显示条数
    fn badge(&self, cx: &App) -> Option<SharedString> {
        let app = self.app.upgrade()?;
        let app = app.read(cx);
        let count = app.session().subagents.len();
        (count > 0).then(|| SharedString::from(count.to_string()))
    }
}

impl Panel for SubagentPane {
    fn panel_name(&self) -> &'static str {
        PaneKind::Subagent.panel_name()
    }

    fn dump(&self, _cx: &App) -> PanelState {
        super::pane_state(PaneKind::Subagent, serde_json::json!({ "kind": "subagent" }))
    }
}

impl EventEmitter<PanelEvent> for SubagentPane {}

impl Focusable for SubagentPane {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SubagentPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(app) = self.app.upgrade() else {
            return div().size_full().into_any_element();
        };
        app.update(cx, |app, cx| app.render_subagent_tab(window, cx))
    }
}

impl ChatApp {
    /// 子代理 Pane 的正文：列当前会话的全部子代理，点一条在对话区打开它的过程。
    ///
    /// 详情页**不在这里画**：`content.rs` 已经在 `viewing_task` 有值时接管内容区了，
    /// 两处各画一份会共用 `task_scroll`、元素 id 也撞车（和审查页同一个坑）。
    pub(crate) fn render_subagent_tab(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let session = self.session();
        if session.subagents.is_empty() {
            return div()
                .size_full()
                .p_4()
                .text_size(TEXT_SMALL)
                .text_color(rgb(INK_SOFT))
                .child("还没有子代理：内核把活分出去时，这里会列出来")
                .into_any_element();
        }
        // 稳定的顺序：按 task id 排，避免 HashMap 每帧换位置
        let mut runs: Vec<(String, SharedString, SharedString)> = session
            .subagents
            .iter()
            .map(|(id, run)| (id.clone(), run.agent.clone(), run.prompt.clone()))
            .collect();
        runs.sort_by(|a, b| a.0.cmp(&b.0));
        let rows = runs.into_iter().map(|(id, agent, prompt)| {
            let task = id.clone();
            div()
                .id(ElementId::Name(format!("subagent-row-{id}").into()))
                .h_flex()
                .items_center()
                .gap_2()
                .px_4()
                .py_2()
                .rounded_md()
                .cursor_pointer()
                .hover(|d| d.bg(rgb(HOVER)))
                .child(Icon::new(IconAll::GitBranch).size_3p5().text_color(rgb(INK_SOFT)))
                .child(
                    div()
                        .flex_shrink_0()
                        .text_size(TEXT_SMALL)
                        .font_weight(FontWeight::MEDIUM)
                        .child(if agent.is_empty() { "子代理".into() } else { agent }),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .text_size(TEXT_SMALL)
                        .text_color(rgb(INK_SOFT))
                        .child(prompt),
                )
                .child(Icon::new(IconAll::ChevronRight).size_3().text_color(rgb(INK_FAINT)))
                .on_click(cx.listener(move |this, _, _, cx| this.open_task(task.clone(), cx)))
        });
        div().v_flex().size_full().p_2().children(rows).into_any_element()
    }
}

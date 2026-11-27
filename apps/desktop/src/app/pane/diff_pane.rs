//! 改动 Pane：把右侧面板的「改动」工作区（清单 + 逐文件 Diff）装进自由 Pane。
//!
//! 渲染仍走 `right_panel.rs`（Diff 预览、`DiffSelection` 解析都在那边），
//! Phase 3 会把 Scope 分段切换、行内批注、审查页的逐块撤回接到这个 Pane 上。

use super::{ClaudesktopPane, PaneKind};
use crate::app::ChatApp;
use crate::theme::rgb;
use crate::theme::*;
use gpui_base::dock::{Panel, PanelEvent, PanelState};
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

pub(crate) struct DiffPane {
    app: WeakEntity<ChatApp>,
    focus_handle: FocusHandle,
    /// 上一帧量到的面板宽度（双栏 Diff 的阈值按它判）
    width: crate::app::widgets::MeasuredWidth,
}

impl DiffPane {
    pub(crate) fn new(app: WeakEntity<ChatApp>, cx: &mut Context<Self>) -> Self {
        super::follow_hub(app.clone(), cx);
        Self { app, focus_handle: cx.focus_handle(), width: Default::default() }
    }

    /// 这一轮能不能打开审查页（能的话 Tab 栏给一个「逐块撤回」）
    fn review_anchor_of(&self, cx: &App) -> Option<usize> {
        let app = self.app.upgrade()?;
        let app = app.read(cx);
        let session = app.session();
        if session.busy || session.latest_turn().is_none() {
            return None;
        }
        session.latest_turn_anchor(app.settings.view_mode)
    }
}

impl ClaudesktopPane for DiffPane {
    fn pane_kind(&self) -> PaneKind {
        PaneKind::Diff
    }

    fn icon(&self, _cx: &App) -> IconAll {
        IconAll::FileDiff
    }

    fn session_token(&self, cx: &App) -> Option<u64> {
        self.app.upgrade().map(|app| app.read(cx).session().token)
    }

    /// Tab 上的 badge：这一轮改了几个文件（读缓存，不在这里扫记录）
    fn badge(&self, cx: &App) -> Option<SharedString> {
        let app = self.app.upgrade()?;
        let app = app.read(cx);
        let count = app.right_panel.changed_count;
        (count > 0).then(|| SharedString::from(count.to_string()))
    }

    /// Tab 栏右侧：四档取数范围（本轮 / 会话 / 未提交 / 基线）。
    /// 数据源不同，但都是「这个会话改了什么」，所以放在同一个分段控件里切。
    fn header_actions(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let app = self.app.clone();
        let current =
            self.app.upgrade().map(|app| app.read(cx).right_panel.scope).unwrap_or_default();
        let mut items: Vec<AnyElement> = crate::app::right_panel::DiffScope::ALL
            .into_iter()
            .map(|scope| {
                let app = app.clone();
                let active = scope == current;
                div()
                    .id(ElementId::Name(format!("diff-scope-{}", scope.label()).into()))
                    .px_2()
                    .py_0p5()
                    .rounded_md()
                    .text_size(TEXT_META)
                    .cursor_pointer()
                    .when(active, |d| d.bg(rgb(SELECTED)).text_color(rgb(INK)))
                    .when(!active, |d| {
                        d.text_color(rgb(INK_SOFT)).hover(|d| d.bg(rgb(HOVER)).text_color(rgb(INK)))
                    })
                    .child(scope.label())
                    .on_click(cx.listener(move |_, _, _, cx| {
                        if let Some(app) = app.upgrade() {
                            app.update(cx, |app, cx| app.set_diff_scope(scope, cx));
                        }
                    }))
                    .into_any_element()
            })
            .collect();
        // 「本轮」还能逐块撤回：按钮直接打开审查页（原来只有改动卡片上的入口）
        if current == crate::app::right_panel::DiffScope::Turn
            && let Some(anchor) = self.review_anchor_of(cx)
        {
            let app = app.clone();
            items.push(
                div()
                    .id("diff-open-review")
                    .h_full()
                    .flex_shrink_0()
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_1()
                    .rounded_md()
                    .text_size(TEXT_META)
                    .text_color(rgb(INK_SOFT))
                    .hover(|d| d.bg(rgb(HOVER)).text_color(rgb(INK)))
                    .child(Icon::new(IconAll::Undo2).small())
                    .child("逐块撤回")
                    .on_click(cx.listener(move |_, _, _, cx| {
                        if let Some(app) = app.upgrade() {
                            app.update(cx, |app, cx| app.open_review(anchor, cx));
                        }
                    }))
                    .into_any_element(),
            );
        }
        // 「未提交 / 基线」两档要能手动重拉：用户手改文件、shell 里改了东西时
        // `diff_revision` 不会变，只有这个按钮能刷新
        if current != crate::app::right_panel::DiffScope::Turn
            && current != crate::app::right_panel::DiffScope::Session
        {
            let handle = app.clone();
            items.push(
                div()
                    .id("diff-refresh-git")
                    .h_full()
                    .flex_shrink_0()
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_1()
                    .rounded_md()
                    .text_size(TEXT_META)
                    .text_color(rgb(INK_SOFT))
                    .hover(|d| d.bg(rgb(HOVER)).text_color(rgb(INK)))
                    .child(Icon::new(IconAll::RefreshCw).small())
                    .child("刷新")
                    .on_click(cx.listener(move |_, _, _, cx| {
                        if let Some(app) = handle.upgrade() {
                            app.update(cx, |app, cx| app.refresh_git_diff(cx));
                        }
                    }))
                    .into_any_element(),
            );
        }
        vec![div().h_flex().items_center().gap_0p5().mr_1().children(items).into_any_element()]
    }
    /// 换窗口（弹出 / 吸回）：底栏的批注输入框绑的是旧 `Window`，重建并保留草稿。
    fn on_window_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(app) = self.app.upgrade() {
            app.update(cx, |app, cx| app.rebuild_comment_input(window, cx));
        }
    }
}

impl Panel for DiffPane {
    fn panel_name(&self) -> &'static str {
        PaneKind::Diff.panel_name()
    }

    fn dump(&self, _cx: &App) -> PanelState {
        super::pane_state(PaneKind::Diff, serde_json::json!({ "kind": "diff", "scope": "turn" }))
    }

    /// 切到后台：Diff 预览的虚拟列表缓存放掉（后台 Tab 不该继续占行高缓存）。
    /// 只放自己这一份：文件 Pane 可能正并排开着。
    fn set_active(&mut self, active: bool, _window: &mut Window, cx: &mut Context<Self>) {
        if active {
            return;
        }
        if let Some(app) = self.app.upgrade() {
            app.update(cx, |app, _| app.release_diff_list());
        }
    }
}

impl EventEmitter<PanelEvent> for DiffPane {}

impl Focusable for DiffPane {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for DiffPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(app) = self.app.upgrade() else {
            return div().size_full().into_any_element();
        };
        let width_rem = self.width.get().map(|w| f32::from(w) / f32::from(window.rem_size()));
        div()
            .relative()
            .size_full()
            .min_h_0()
            .child(self.width.probe())
            .child(app.update(cx, |app, cx| app.render_diff_tab(width_rem, window, cx)))
            .into_any_element()
    }
}

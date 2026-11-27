//! 文件 Pane：把右侧面板的「文件」工作区（文件树 + 源码预览）装进自由 Pane。
//!
//! 渲染仍然走 `right_panel.rs` 的实现（虚拟列表、`syntax.rs` 高亮、双栏阈值都在那边），
//! 这里只是换了个外壳：以前它挂在一个固定的右侧列上，现在可以拖到任意位置、
//! 和改动 / 对话并成 Tab，或者（Phase 2）弹成独立窗。

use super::{ClaudesktopPane, PaneKind};
use crate::app::ChatApp;
use crate::theme::rgb;
use crate::theme::*;
use gpui_base::dock::{Panel, PanelEvent, PanelState};
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::*;
use gpui_kit::*;

pub(crate) struct FilesPane {
    app: WeakEntity<ChatApp>,
    focus_handle: FocusHandle,
    /// 上一帧量到的面板宽度（决定要不要并排双栏；见 `render_file_workspace`）
    width: crate::app::widgets::MeasuredWidth,
}

impl FilesPane {
    pub(crate) fn new(app: WeakEntity<ChatApp>, cx: &mut Context<Self>) -> Self {
        super::follow_hub(app.clone(), cx);
        Self { app, focus_handle: cx.focus_handle(), width: Default::default() }
    }
}

impl ClaudesktopPane for FilesPane {
    fn pane_kind(&self) -> PaneKind {
        PaneKind::Files
    }

    fn icon(&self, _cx: &App) -> IconAll {
        IconAll::FolderTree
    }

    fn session_token(&self, cx: &App) -> Option<u64> {
        self.app.upgrade().map(|app| app.read(cx).session().token)
    }

    /// Tab 栏右侧的刷新按钮（只对文件树有意义）
    fn header_actions(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let app = self.app.clone();
        vec![
            div()
                .id("files-pane-refresh")
                .h_full()
                .flex_shrink_0()
                .px_1p5()
                .flex()
                .items_center()
                .text_color(rgb(INK_SOFT))
                .hover(|d| d.bg(rgb(HOVER)))
                .child(Icon::new(IconAll::RefreshCw).small())
                .on_click(cx.listener(move |_, _, _, cx| {
                    if let Some(app) = app.upgrade() {
                        app.update(cx, |app, cx| {
                            app.right_panel.refresh();
                            cx.notify();
                        });
                    }
                }))
                .into_any_element(),
        ]
    }
}

impl Panel for FilesPane {
    fn panel_name(&self) -> &'static str {
        PaneKind::Files.panel_name()
    }

    fn dump(&self, cx: &App) -> PanelState {
        let selected = self.app.upgrade().and_then(|app| {
            let app = app.read(cx);
            app.right_panel.selected_file.clone().map(|p| p.to_string_lossy().into_owned())
        });
        super::pane_state(
            PaneKind::Files,
            serde_json::json!({ "kind": "files", "selected": selected }),
        )
    }

    /// 切到后台（别的 Tab 顶上来）：把文件树的虚拟列表缓存放掉，
    /// 否则后台 Tab 会一直占着行高缓存。只放自己这一份（Diff Pane 可能并排开着）
    fn set_active(&mut self, active: bool, _window: &mut Window, cx: &mut Context<Self>) {
        if active {
            return;
        }
        if let Some(app) = self.app.upgrade() {
            app.update(cx, |app, _| app.release_file_list());
        }
    }
}

impl EventEmitter<PanelEvent> for FilesPane {}

impl Focusable for FilesPane {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for FilesPane {
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
            .child(app.update(cx, |app, cx| app.render_files_tab(width_rem, window, cx)))
            .into_any_element()
    }
}

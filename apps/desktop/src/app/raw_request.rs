//! 首页「请求」页签里点「原文」弹出的对话框：一次请求发给服务商的请求体和收到的响应。
//!
//! 原文由网关另存（`claudesktop_builtin::request_bodies`），这里读出来放进两个只读的代码编辑器
//! （gpui-kit `Editor`：按行虚拟滚动、可搜索，几 MB 的请求体也不卡），「请求 / 返回」切换；
//! 可以整段复制，或者存成文件用系统默认程序打开。对话框里自成一个 view，切栏只重画它自己。

use super::ChatApp;
use super::settings_kit::seg_control_in;
use crate::theme::rgb;
use crate::theme::*;
use claudesktop_builtin::gateway::RequestLog;
use claudesktop_builtin::request_bodies::{self, Saved};
use gpui_kit::component::input::{Editor, EditorState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::*;
use gpui_kit::*;

/// 对话框宽度
const RAW_W: Rems = Rems(56.);
/// 编辑器高度
const RAW_H: Rems = Rems(30.);

pub(super) struct RawRequest {
    request: Entity<EditorState>,
    response: Entity<EditorState>,
    saved: Saved,
    /// 存成文件时的名字（日志里的 ID 最后一段）
    name: String,
    /// 0 = 请求，1 = 返回
    tab: usize,
}

impl RawRequest {
    fn new(saved: Saved, name: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let editor =
            |text: &str, language: &'static str, window: &mut Window, cx: &mut Context<Self>| {
                let text = text.to_string();
                cx.new(|cx| {
                    let mut state = EditorState::new(window, cx)
                        .language(language)
                        .line_number(true)
                        .soft_wrap(true)
                        .searchable(true);
                    state.set_value(text, window, cx);
                    state
                })
            };
        let request = editor(&saved.request, "json", window, cx);
        let response = editor(&saved.response, "text", window, cx);
        Self { request, response, saved, name, tab: 0 }
    }

    fn text(&self) -> &str {
        if self.tab == 0 { &self.saved.request } else { &self.saved.response }
    }

    /// 存到临时目录，用系统默认程序打开（编辑器里看全文、换行、格式化）。
    fn open_in_app(&self, cx: &mut Context<Self>) -> Result<(), String> {
        let ext = if self.tab == 0 { "json" } else { "txt" };
        let kind = if self.tab == 0 { "request" } else { "response" };
        let path = std::env::temp_dir().join(format!("claudesktop-{kind}-{}.{ext}", self.name));
        std::fs::write(&path, self.text())
            .map_err(|e| format!("写不了 {}：{e}", path.display()))?;
        cx.open_with_system(&path);
        Ok(())
    }
}

impl Render for RawRequest {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = if self.tab == 0 { &self.request } else { &self.response };
        let empty = self.text().is_empty();
        let note = match self.tab {
            0 => format!("{} 字节，原样发出（这里排了版）", self.saved.request.len()),
            _ if empty => "没有存到返回".to_string(),
            _ if self.saved.truncated => "返回太长，只存了前 2 MB".to_string(),
            _ => "流式返回是原样的 SSE".to_string(),
        };
        div()
            .v_flex()
            .gap_3()
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_3()
                    .child(seg_control_in(
                        "raw-tab",
                        [(0, "请求"), (1, "返回")],
                        self.tab,
                        |this: &mut Self, tab, _, cx| {
                            this.tab = tab;
                            cx.notify();
                        },
                        cx,
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(TEXT_SMALL)
                            .text_color(rgb(INK_SOFT))
                            .child(note),
                    )
                    .child(
                        ChatApp::text_button("raw-copy", "复制", false)
                            .small()
                            .disabled(empty)
                            .on_click(cx.listener(|this, _, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(
                                    this.text().to_string(),
                                ));
                            })),
                    )
                    .child(
                        ChatApp::text_button("raw-open", "用编辑器打开", false)
                            .small()
                            .disabled(empty)
                            .on_click(cx.listener(|this, _, window, cx| {
                                if let Err(err) = this.open_in_app(cx) {
                                    window.push_notification(Notification::error(err), cx);
                                }
                            })),
                    ),
            )
            .child(
                div()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(LINE))
                    .bg(rgb(CARD))
                    .overflow_hidden()
                    .font_family(MONO)
                    .text_size(TEXT_SMALL)
                    .child(
                        Editor::new(state)
                            .readonly(true)
                            .appearance(false)
                            .bordered(false)
                            .h(RAW_H),
                    ),
            )
    }
}

impl ChatApp {
    /// 打开一次请求的原文；读不出来（清理掉了、文件坏了）用提示说明。
    pub(super) fn open_raw_request(
        &mut self,
        log: &RequestLog,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = log.body.clone() else {
            return;
        };
        let Some(dir) = self
            .settings
            .gateway()
            .map(|g| g.dir().to_path_buf())
            .or_else(claudesktop_builtin::gateway::default_dir)
        else {
            return;
        };
        // 解压、排版几 MB 的请求体放到后台，界面不卡
        let model = log.model.clone();
        let at_ms = log.at_ms;
        cx.spawn_in(window, async move |this, cx| {
            let loaded = cx
                .background_executor()
                .spawn({
                    let id = id.clone();
                    async move { request_bodies::load(&dir, &id) }
                })
                .await;
            let _ = this.update_in(cx, |app, window, cx| match loaded {
                Ok(saved) => app.show_raw_request(saved, &id, model, at_ms, window, cx),
                Err(err) => app.toast(Notification::error(err), cx),
            });
        })
        .detach();
    }

    fn show_raw_request(
        &mut self,
        saved: Saved,
        id: &str,
        model: Option<String>,
        at_ms: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if window.has_active_dialog(cx) {
            return;
        }
        let name = id.rsplit('/').next().unwrap_or("request").to_string();
        let view = cx.new(|cx| RawRequest::new(saved, name, window, cx));
        let mut title = vec!["请求原文".to_string()];
        title.extend(model);
        title.push(super::requests_view::time_label(at_ms));
        let title = title.join(" · ");
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .w(RAW_W.to_pixels(root_font_size()))
                .title(
                    div()
                        .text_size(TEXT_LEAD)
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(INK))
                        .child(title.clone()),
                )
                .child(view.clone())
        });
    }
}

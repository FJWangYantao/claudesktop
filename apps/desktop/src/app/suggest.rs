//! 输入框的 Tab 补全：停一下就让模型猜「接下来大概会写什么」，灰字画在光标后面，按 Tab 接上。
//!
//! 一次补全 = 一个很小的独立请求（[`claudesktop_builtin::suggest`]），不走内核、不进会话日志，
//! 也不占内核的「一轮」。流程：
//!
//! 1. 输入框内容变了（`InputEvent::Change`）→ [`ChatApp::schedule_suggestion`]：先清掉旧的那句
//!    灰字，再排一个 [`DEBOUNCE_MS`] 的计时器。连着打字时计时器不断被换掉，只有停顿了才真发。
//! 2. 计时器到点 → 记下这次请求的代号和输入快照，在后台线程里发请求（阻塞读 SSE）。
//! 3. 回来时代号还对不上（用户又打字 / 换了会话 / 关了开关）就丢掉，否则存进 `Suggestion`
//!    并只重画输入区。
//!
//! 几处刻意的收窄，都是「宁可不补，也不要补一句不相干的灰字」：
//!
//! - 请求要走网络，打字快的时候太费钱：停顿 [`DEBOUNCE_MS`] 才发，且只在光标在末尾、
//!   没有选区、输入不长（[`MAX_DRAFT_CHARS`]）的时候发；`/`、`@` 补全开着时不发。
//! - 一轮跑着时（等回复）也不发：那会儿用户多半在等，不是在写。
//! - 请求失败、没有可补的：静默，什么都不显示。
//! - 默认关；开了也优先用设置里另填的小模型（`suggest_model`），只带最近几轮对话，
//!   过期的请求当场中断——每次停顿都是一次计费的请求。

use super::ChatApp;
use crate::model::ChatItem;
use crate::settings::KernelChoice;
use crate::theme::rgb;
use crate::theme::*;
use gpui_kit::component::input::Position;
use gpui_kit::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// 停下多久才发请求（敲字时不停被换掉，只有真停了才发）
const DEBOUNCE_MS: u64 = 450;
/// 输入框里已经写了这么多字就不再猜（再猜也猜不准，还费一次请求）
const MAX_DRAFT_CHARS: usize = 400;
/// 给模型看的最近几轮（用户消息 + 那一轮最后一段回复）；猜下一句用不着更早的
const MAX_USER_TURNS: usize = 3;

/// 当前这一句灰字（还没被接上的）。
#[derive(Default)]
pub(super) struct SuggestionState {
    /// 这句灰字是为哪份输入猜的：输入变了就不再显示（见 [`SuggestionState::text`]）
    pub(super) text: Option<Suggestion>,
    /// 计时器：又敲字时换掉旧的，旧的自行取消
    timer: Option<Task<()>>,
    /// 每次排请求 +1：回来时对不上说明已经过期。后台线程也读它，过期的请求当场中断
    /// （不然用户接着打字时旧请求照样跑完、照样计费）
    generation: Arc<AtomicU64>,
    /// 刚接上一句：那一次 `Change` 不为它再发请求（接着连猜会一句接一句，又费钱又吵）
    just_accepted: bool,
}

pub(super) struct Suggestion {
    /// 灰字本身
    pub(super) text: String,
    /// 猜的时候输入框里是什么（和当前输入一致才显示）
    draft: String,
}

impl SuggestionState {
    /// 要显示的那句灰字：输入框还是猜它的那份内容时才算数。
    pub(super) fn text(&self, draft: &str) -> Option<&str> {
        self.text
            .as_ref()
            .filter(|s| s.draft == draft)
            .map(|s| s.text.as_str())
            .filter(|t| !t.trim().is_empty())
    }

    /// 换输入了（或者接上了、发出去了）：灰字立刻撤掉。
    pub(super) fn clear(&mut self) {
        self.text = None;
        self.generation.fetch_add(1, Ordering::Relaxed);
        self.timer = None;
    }
}

impl ChatApp {
    /// 输入框内容变了：撤掉旧的灰字，重新排一次补全。
    pub(super) fn schedule_suggestion(&mut self, cx: &mut Context<Self>) {
        // 刚才那次变化是「接上了一句」：不接着连猜，等用户再打字
        if std::mem::take(&mut self.suggestion.just_accepted) {
            self.suggestion.clear();
            return;
        }
        self.suggestion.clear();
        if !self.suggestion_enabled() {
            return;
        }
        let (draft, cursor) = {
            let input = self.input.read(cx);
            (input.value().to_string(), input.cursor())
        };
        // 只在光标在末尾、没选东西、内容不长的时候猜
        if draft.trim().is_empty()
            || draft.chars().count() > MAX_DRAFT_CHARS
            || cursor != draft.len()
            || !self.input.read(cx).selected_range().is_empty()
            || self.completion.open.is_some()
        {
            return;
        }
        let live = self.suggestion.generation.clone();
        let generation = live.fetch_add(1, Ordering::Relaxed) + 1;
        let history = self.suggestion_history();
        // 网关日志按会话归类；补全不占内核的一轮，所以轮次写 0
        let session_id = self.session().acp_id.clone().unwrap_or_default();
        let Some(config) = self.suggestion_config() else { return };
        let request_draft = draft.clone();
        self.suggestion.timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(DEBOUNCE_MS)).await;
            // 后台线程里发请求（阻塞读流，不能占 UI 线程）
            let asked = request_draft.clone();
            let text = cx
                .background_executor()
                .spawn(async move {
                    let stale = || live.load(Ordering::Relaxed) != generation;
                    claudesktop_builtin::suggest::complete(
                        &config,
                        &session_id,
                        0,
                        &asked,
                        &history,
                        &stale,
                    )
                })
                .await;
            // 回来时要放进来的灰字属于哪份输入（和请求用的那份是同一份）
            let draft = request_draft;
            this.update(cx, |app, cx| {
                // 用户又打字了 / 换了会话 / 开关关了：这次的结果已经过期
                if app.suggestion.generation.load(Ordering::Relaxed) != generation {
                    return;
                }
                if let Some(text) = text {
                    app.suggestion.text = Some(Suggestion { text, draft });
                    // 只重画输入区，不碰对话（见 content.rs）
                    app.refresh_composer(cx);
                }
            })
            .ok();
        }));
    }

    /// 这次要不要猜：开关开着、自研内核配好了、没有一轮在跑、不在首页配置引导。
    fn suggestion_enabled(&self) -> bool {
        self.settings.suggestions
            && !self.settings_open
            && !self.settings.needs_setup()
            && self.session().kernel == KernelChoice::Builtin
            && !self.session().busy
            && self.session().acp_id.is_some()
    }

    /// 补全用的端点与模型：设置里填了补全模型就用它，否则跟着当前会话的模型走（顶栏显示哪个就用哪个）。
    /// 带思考的模型不补：64 个 token 的上限多半被思考用光，灰字出不来，请求却照样计费。
    fn suggestion_config(&self) -> Option<claudesktop_builtin::ProviderConfig> {
        let profile = self.settings.builtin.current();
        if !profile.is_configured() {
            return None;
        }
        let mut config = profile.provider_config();
        let chosen = self.settings.suggest_model.trim();
        let session_model = self.session().model_label.to_string();
        if !chosen.is_empty() {
            config.model = chosen.to_string();
        } else if !session_model.trim().is_empty() && session_model != crate::session::UNKNOWN_MODEL
        {
            config.model = session_model;
        }
        if claudesktop_builtin::suggest::is_reasoning_model(&config.model) {
            return None;
        }
        // 补全的请求也经用量网关（用途 `suggest`），和「自动」模式分类器一致
        config.gateway = self.settings.gateway();
        Some(config)
    }

    /// 给补全看的对话：用户说过的话 + 助手的最后一段回复（工具调用、思考都不带）。
    fn suggestion_history(&self) -> Vec<(bool, String)> {
        let items = &self.session().transcript;
        let users: Vec<(usize, &str)> = items
            .iter()
            .enumerate()
            .filter_map(|(i, item)| match item {
                ChatItem::User { text, .. } => Some((i, text.as_ref())),
                _ => None,
            })
            .collect();
        let start = users.len().saturating_sub(MAX_USER_TURNS);
        let mut history = Vec::new();
        for (n, &(i, text)) in users.iter().enumerate().skip(start) {
            history.push((true, text.to_string()));
            // 这一轮之后、下一轮之前的那段回复（只取最后一段正文）
            let end = users.get(n + 1).map_or(items.len(), |&(j, _)| j);
            let reply = items[i + 1..end].iter().rev().find_map(|item| match item {
                ChatItem::Agent(text) if !text.trim().is_empty() => Some(text.clone()),
                _ => None,
            });
            if let Some(reply) = reply {
                history.push((false, reply));
            }
        }
        history
    }

    /// 接上这句灰字：插在光标处，光标留在新文字之后。
    ///
    /// 接上之后不再自动猜下一句（紧接着再猜又是一次请求，还会一句接一句停不下来）；
    /// 接着打字时自然会有新的一句。
    pub(super) fn accept_suggestion(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let draft = self.input.read(cx).value().to_string();
        let Some(text) = self.suggestion.text(&draft).map(str::to_string) else {
            return;
        };
        self.suggestion.clear();
        // `insert` 会发一次 `Change`，那一趟不要再排补全（见 `schedule_suggestion`）
        self.suggestion.just_accepted = true;
        self.input.update(cx, |state, cx| state.insert(text, window, cx));
        self.refresh_composer(cx);
    }

    /// 现在光标后面那句灰字（没有就是 None）。要在 UI 线程上读输入框，所以单独一个方法。
    ///
    /// 除了「输入框还是猜它的那份内容」，还要求光标仍在那份内容的末尾、没有选区：
    /// 用方向键挪过光标之后，补全就不该再跟过去（它是接着末尾那句话的）。
    pub(super) fn suggested_tail(&self, cx: &App) -> Option<String> {
        let input = self.input.read(cx);
        let draft = input.value().to_string();
        if input.cursor() != draft.len() || !input.selected_range().is_empty() {
            return None;
        }
        self.suggestion.text(&draft).map(str::to_string)
    }

    /// 调试钩子 `CLAUDESKTOP_SUGGEST=<灰字>`：不走模型，直接把这句话摆成灰字，
    /// 用来核对灰字画在哪儿（位置、裁剪、和已输入文字的对齐）。见 `main.rs`。
    /// 值是 `-` 时只填草稿、不摆灰字，用来和「有灰字」那张逐像素对比。
    pub(crate) fn debug_suggestion(
        &mut self,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let draft = "帮我看看这个";
        let column = draft.chars().count() as u32;
        self.input.update(cx, |state, cx| {
            state.set_value(draft, window, cx);
            // set_value 会把光标放回开头，补全只画在光标后面，这里挪到末尾
            state.set_cursor_position(Position::new(0, column), window, cx);
        });
        if text != "-" {
            self.suggestion.text = Some(Suggestion { text, draft: draft.to_string() });
        }
        self.refresh_composer(cx);
    }

    /// 灰字的绘制层，叠在输入框上（见 `composer.rs`）。
    ///
    /// 位置不能自己算：光标在第几行、被横向滚了多远只有输入框自己知道。所以包在 `deferred` 里，
    /// prepaint 排在输入框的 prepaint 之后，从它 stash 的当前帧光标几何（`prepaint_caret_geometry`）
    /// 取位置——和 gpui-kit 自己的补全菜单锚定光标是同一套做法。
    /// 文字裁到输入框的边界内：一句太长就截断，不会画到卡片外面去。
    pub(super) fn suggestion_ghost(&self, text: String) -> AnyElement {
        let input = self.input.clone();
        let color = rgb(INK_FAINT);
        deferred(
            canvas(
                move |_, window, cx| {
                    let geometry = input.read(cx).prepaint_caret_geometry()?;
                    // 字体跟输入框一样从祖先继承；字号取界面正文档（gpui-kit 输入框 Medium 档
                    // 就是 `text_sm`），行高和光标位置取输入框这一帧排出来的那份，两处文字才对得齐
                    let mut run = window.text_style().to_run(text.len());
                    run.color = color.into();
                    let font_size = TEXT_BODY.to_pixels(window.rem_size());
                    let line = window.text_system().shape_line(
                        text.clone().into(),
                        font_size,
                        &[run],
                        None,
                    );
                    // 光标框比行框矮（0.85 倍），向上补半个差值就是这一行的顶
                    let caret = geometry.cursor_bounds();
                    let line_height = geometry.line_height();
                    let origin =
                        point(caret.right(), caret.top() - (line_height - caret.size.height) / 2.);
                    Some((line, origin, line_height, geometry.input_bounds()))
                },
                |_, state, window, cx| {
                    let Some((line, origin, line_height, input_bounds)) = state else {
                        return;
                    };
                    let clip = ContentMask { bounds: input_bounds };
                    window.with_content_mask(Some(clip), |window| {
                        let _ = line.paint(origin, line_height, TextAlign::Left, None, window, cx);
                    });
                },
            )
            .absolute()
            .inset_0(),
        )
        .into_any_element()
    }
}

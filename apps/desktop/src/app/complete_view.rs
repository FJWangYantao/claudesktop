//! 输入框的 `/指令` 和 `@文件` 补全：输入时在输入框正上方弹出候选列表，
//! 上下键选、回车或 Tab 接受、Esc 收起；鼠标点也行。识别与排序的纯逻辑在 `crate::complete`。
//!
//! 键盘接管：输入框把上下键 / 回车 / Tab / Esc 绑成了自己的动作（`MoveUp` / `Enter` …），
//! 列表开着时在输入框卡片上用 `capture_action` 先截下来（动作分发先走 capture 阶段，
//! 从根到焦点），处理了就 `stop_propagation`，输入框收不到；列表没开时放行。
//!
//! 键位与谁管：
//! - ↑ / ↓：列表里两项以上才截走（选上 / 下一项）；只有一项时放行给输入框移光标，列表不动。
//!   其余情况按「设计说明-输入框方向键.md」的顺序判定（历史回溯 → 按显示行移动）。
//! - ← / →：一律放行（光标照常移），但 capture 里登记一次 `defer_in`，光标挪完再对一次触发词——
//!   移出 `/` `@` 的范围就收起列表，移回来再弹（`sync_completion`）。输入框不广播光标事件，
//!   所以只能这样补。
//! - Ctrl+回车：跳过「接受补全」，直接发送（输入框把 `secondary-enter` 绑成了 `Enter{secondary}`，
//!   我们别截它就是发送）。
//! - Alt+← / Alt+→：在光标处重新展开 / 收起列表（绑定在 `keys.rs`，处理在这里，只在输入框卡片上生效）。

use super::ChatApp;
use super::chrome::CHROME_H;
use super::keys::{COLLAPSE_COMPLETION_KEY, CollapseCompletion, ExpandCompletion};
use crate::complete::{self, Trigger, TriggerKind};
use crate::model::ViewMode;
use crate::settings::KernelChoice;
use crate::theme::rgb;
use crate::theme::*;
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::input::{
    Enter, Escape, IndentInline, MoveDown, MoveLeft, MoveRight, MoveUp, Position,
};
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::path::PathBuf;

/// 弹出列表最多几项（文件很多时只列最相关的）
const MAX_ITEMS: usize = 8;
/// 列工作区文件的上限（防止在巨大的目录里卡住）
const MAX_FILES: usize = 20_000;

/// 正开着的补全列表。
pub(super) struct Completion {
    trigger: Trigger,
    items: Vec<CompletionItem>,
    selected: usize,
    /// 条目滚动的位置：面板限高后条目多了要能滚，上下键选中项要滚回可见
    scroll: ScrollHandle,
}

#[derive(Clone)]
pub(super) struct CompletionItem {
    icon: IconAll,
    title: SharedString,
    detail: SharedString,
    /// 当前项（当前模式 / 模型 / 视图）打对勾
    current: bool,
    action: CompletionAction,
}

#[derive(Clone)]
enum CompletionAction {
    /// `@文件`：插入「@相对路径 」
    Mention(String),
    NewSession,
    Mode(usize),
    View(ViewMode),
    Compact,
    Export,
    Settings,
    /// 插件的斜杠命令：填进「/名字 」等用户写参数，发出时展开（见 `submit_to`）
    PluginCommand(String),
}

/// 补全的状态：列表、Esc 收起时的输入快照、工作区文件索引。
#[derive(Default)]
pub(super) struct CompletionState {
    pub(super) open: Option<Completion>,
    /// 按 Esc 收起时的输入内容：内容没变就不再弹出
    dismissed: Option<String>,
    /// 工作区文件（目录, 相对路径）：每次新敲一个 `@` 时重新列，之后筛选都用这份
    files: Option<(PathBuf, Vec<String>)>,
}

impl ChatApp {
    /// 输入内容变了：重新判断光标处是不是触发词、刷新候选。
    pub(super) fn update_completion(&mut self, cx: &mut Context<Self>) {
        // 提问面板挂在输入框上沿时不同时开补全（两叠一起会互相盖住）
        if self.session().waiting_question() {
            if self.completion.open.take().is_some() {
                self.refresh_composer(cx);
            }
            return;
        }
        let (text, cursor) = {
            let input = self.input.read(cx);
            (input.value().to_string(), input.cursor())
        };
        if self.completion.dismissed.as_deref() == Some(text.as_str()) {
            return;
        }
        self.completion.dismissed = None;
        let was = self.completion.open.as_ref().map(|c| c.trigger.kind);
        let Some(trigger) = complete::trigger_at(&text, cursor) else {
            self.completion.open = None;
            self.refresh_composer(cx);
            return;
        };
        // 聊天会话没有工作目录：`@` 文件补全不弹（附件是内联的，不走 `@路径`，见「设计说明-聊天与Code分离.md」5.5）
        if trigger.kind == TriggerKind::Mention && self.session().is_chat() {
            self.completion.open = None;
            self.refresh_composer(cx);
            return;
        }
        // 已经写完插件命令名、在写参数了：不再弹列表（否则回车会再选一次命令，参数被冲掉）
        if trigger.kind == TriggerKind::Slash
            && trigger.query.contains(char::is_whitespace)
            && claudesktop_core::plugin::find_command(&self.enabled_plugins(), &text).is_some()
        {
            self.completion.open = None;
            self.refresh_composer(cx);
            return;
        }
        let items: Vec<CompletionItem> = match trigger.kind {
            TriggerKind::Slash => self
                .slash_commands()
                .into_iter()
                .filter(|(keyword, item)| {
                    complete::command_matches(
                        keyword,
                        &format!("{} {}", item.title, item.detail),
                        &trigger.query,
                    )
                })
                .map(|(_, item)| item)
                .collect(),
            TriggerKind::Mention => {
                let cwd = self.session().cwd.clone();
                let stale = was != Some(TriggerKind::Mention)
                    || self.completion.files.as_ref().is_none_or(|(dir, _)| *dir != cwd);
                if stale {
                    self.completion.files =
                        Some((cwd.clone(), complete::list_files(&cwd, MAX_FILES)));
                }
                let files =
                    self.completion.files.as_ref().map(|(_, f)| f.as_slice()).unwrap_or_default();
                complete::rank_files(files, &trigger.query, MAX_ITEMS)
                    .into_iter()
                    .map(|path| {
                        let (dir, name) = match path.rsplit_once('/') {
                            Some((dir, name)) => (dir.to_string(), name.to_string()),
                            None => (String::new(), path.clone()),
                        };
                        CompletionItem {
                            icon: IconAll::FileText,
                            title: name.into(),
                            detail: dir.into(),
                            current: false,
                            action: CompletionAction::Mention(path),
                        }
                    })
                    .collect()
            }
        };
        self.completion.open = (!items.is_empty()).then(|| {
            // 继续输入同一类时尽量保住选中位置
            let selected = match &self.completion.open {
                Some(c) if c.trigger.kind == trigger.kind => c.selected.min(items.len() - 1),
                _ => 0,
            };
            // 同一类时接着用原来的滚动位置：每敲一个字都换一份的话，选中的那一项会跳回顶
            let scroll = match &self.completion.open {
                Some(c) if c.trigger.kind == trigger.kind => c.scroll.clone(),
                _ => ScrollHandle::new(),
            };
            Completion { trigger, items, selected, scroll }
        });
        self.refresh_composer(cx);
    }

    /// 可用的 `/指令`：(匹配用的英文关键字, 候选项)。模式来自当前 agent；模型不在这里，
    /// 走输入框右下角的模型菜单（列表长度随档案变，列进来会把补全面板撑得顶穿窗口）。
    fn slash_commands(&self) -> Vec<(String, CompletionItem)> {
        let item = |icon, title: String, detail: &str, current, action| CompletionItem {
            icon,
            title: title.into(),
            detail: detail.to_string().into(),
            current,
            action,
        };
        let s = self.session();
        let mut out = vec![(
            "new".to_string(),
            item(
                IconAll::MessageSquarePlus,
                "/new".into(),
                "新开会话",
                false,
                CompletionAction::NewSession,
            ),
        )];
        for (i, mode) in s.modes.iter().enumerate() {
            out.push((
                format!("mode {}", mode.id),
                item(
                    mode.icon,
                    format!("/mode {}", mode.name),
                    &mode.desc,
                    i == s.permission_mode,
                    CompletionAction::Mode(i),
                ),
            ));
        }
        // 模型不进补全列表：数量随档案变（本机 16 个），列在这里会把高度撑爆，
        // 而输入框右下角的模型菜单本来就是干这个的，这里只留稳定的常用指令
        for view in ViewMode::ALL {
            out.push((
                format!("view {:?}", view).to_lowercase(),
                item(
                    view.icon(),
                    format!("/view {}", view.label()),
                    view.desc(),
                    self.settings.view_mode == view,
                    CompletionAction::View(view),
                ),
            ));
        }
        if s.kernel == KernelChoice::Builtin {
            out.push((
                "compact".into(),
                item(
                    IconAll::ListCollapse,
                    "/compact".into(),
                    "把较早的对话压缩成摘要，腾出上下文",
                    false,
                    CompletionAction::Compact,
                ),
            ));
        }
        out.push((
            "export".into(),
            item(
                IconAll::Share,
                "/export".into(),
                "把对话导出成 Markdown",
                false,
                CompletionAction::Export,
            ),
        ));
        out.push((
            "settings".into(),
            item(
                IconAll::Settings,
                "/settings".into(),
                "打开设置",
                false,
                CompletionAction::Settings,
            ),
        ));
        // 启用的插件带来的命令：说明后面注上参数提示和来自哪个插件
        for p in self.enabled_plugins() {
            for c in &p.commands {
                let mut detail = c.description.clone();
                if let Some(hint) = &c.argument_hint {
                    detail = format!("{hint}  {detail}");
                }
                detail = format!("{}（插件 {}）", detail.trim(), p.name);
                out.push((
                    format!("{} {}", c.name, p.name),
                    item(
                        IconAll::Blocks,
                        format!("/{}", c.name),
                        &detail,
                        false,
                        CompletionAction::PluginCommand(c.name.clone()),
                    ),
                ));
            }
        }
        out
    }

    /// 接受第 `i` 项：`@文件` 把触发词换成路径；指令先把那一行指令删掉再执行。
    fn accept_completion(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(c) = self.completion.open.take() else {
            return;
        };
        let Some(item) = c.items.get(i).cloned() else {
            return;
        };
        let replacement = match &item.action {
            CompletionAction::Mention(path) => format!("@{path} "),
            CompletionAction::PluginCommand(name) => format!("/{name} "),
            _ => String::new(),
        };
        self.replace_input_range(c.trigger.range, &replacement, window, cx);
        match item.action {
            CompletionAction::Mention(_) | CompletionAction::PluginCommand(_) => {}
            CompletionAction::NewSession => {
                self.add_session(cx);
                self.focus_input(window, cx);
            }
            CompletionAction::Mode(i) => self.choose_mode(i, cx),
            CompletionAction::View(view) => self.set_view_mode(view, cx),
            CompletionAction::Compact => self.compact_session(cx),
            CompletionAction::Export => self.share_session(window, cx),
            CompletionAction::Settings => self.open_settings(window, cx),
        }
        cx.notify();
    }

    /// 把输入框里 `range`（字节）换成 `with`，光标放在换进去的文字后面。
    fn replace_input_range(
        &mut self,
        range: std::ops::Range<usize>,
        with: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.input.update(cx, |state, cx| {
            let text = state.value().to_string();
            let (start, end) = (range.start.min(text.len()), range.end.min(text.len()));
            let next = format!("{}{with}{}", &text[..start], &text[end..]);
            let position = position_of(&next, start + with.len());
            state.set_value(next, window, cx);
            state.set_cursor_position(position, window, cx);
        });
    }

    /// 光标挪到字节偏移 `offset`（换行 / 多字节都算对）。`text` 是输入框当前内容。
    fn set_input_cursor(
        &mut self,
        text: &str,
        offset: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let position = position_of(text, offset);
        self.input.update(cx, |state, cx| {
            state.set_cursor_position(position, window, cx);
        });
    }

    /// 输入框卡片上挂的键盘接管（见模块说明）。
    pub(super) fn with_completion_keys(
        &self,
        card: Stateful<Div>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        card.capture_action(
            cx.listener(|this, _: &MoveUp, window, cx| this.step_completion(-1, window, cx)),
        )
        .capture_action(
            cx.listener(|this, _: &MoveDown, window, cx| this.step_completion(1, window, cx)),
        )
        .capture_action(cx.listener(|this, _: &MoveLeft, window, cx| this.on_arrow(-1, window, cx)))
        .capture_action(cx.listener(|this, _: &MoveRight, window, cx| this.on_arrow(1, window, cx)))
        // Alt+← / Alt+→：在光标处重新展开 / 收起列表（键位绑在 keys.rs）
        .capture_action(cx.listener(|this, _: &ExpandCompletion, _, cx| this.expand_completion(cx)))
        .capture_action(
            cx.listener(|this, _: &CollapseCompletion, _, cx| this.collapse_completion(cx)),
        )
        .capture_action(cx.listener(|this, action: &Enter, window, cx| {
            // Shift+回车照常换行
            if action.shift {
                return;
            }
            // Ctrl+回车：不认补全，直接发送。输入框把 `secondary-enter` 绑成了
            // `Enter { secondary: true }`，它照常发 `PressEnter`（见 mod.rs 的事件订阅），
            // 所以这里只是不截它；开着的列表顺手收起，别留一张悬空的列表
            if action.secondary {
                if this.completion.open.take().is_some() {
                    this.completion.dismissed = Some(this.input.read(cx).value().to_string());
                    this.refresh_composer(cx);
                }
                return;
            }
            this.accept_selected(window, cx);
        }))
        .capture_action(cx.listener(|this, _: &IndentInline, window, cx| {
            // 首页空输入框里的建议提示词：Tab 填入（见 overview.rs）
            if this.completion.open.is_none()
                && this.input.read(cx).value().is_empty()
                && let Some(prompt) = this.ghost_prompt()
            {
                cx.stop_propagation();
                this.fill_prompt(prompt, window, cx);
                return;
            }
            // 一轮结束后猜的那句（淡色画在空输入框里）：Tab 填入（见 suggest.rs）
            if this.completion.open.is_none() && this.next_suggestion_text(cx).is_some() {
                cx.stop_propagation();
                this.accept_next_suggestion(window, cx);
                return;
            }
            // 候选列表没开、光标后面有一句灰字：Tab 接上（见 suggest.rs）
            if this.completion.open.is_none() && this.suggested_tail(cx).is_some() {
                cx.stop_propagation();
                this.accept_suggestion(window, cx);
                return;
            }
            this.accept_selected(window, cx)
        }))
        .capture_action(cx.listener(|this, _: &Escape, _, cx| {
            if this.completion.open.take().is_some() {
                this.completion.dismissed = Some(this.input.read(cx).value().to_string());
                cx.stop_propagation();
                this.refresh_composer(cx);
            } else if this.next_suggestion_text(cx).is_some() {
                // 空输入框里那句猜的下一句：Esc 先撤掉它（不打断打字，也不动光标）
                this.next_suggestion.clear();
                cx.stop_propagation();
                this.refresh_composer(cx);
            } else if this.suggested_tail(cx).is_some() {
                // 光标后面有灰字：Esc 先撤掉它（不打断打字，也不动光标）
                this.suggestion.clear();
                cx.stop_propagation();
                this.refresh_composer(cx);
            }
        }))
    }

    /// 上下键的统一入口，按下面的顺序判定，第一条命中就停（见「设计说明-输入框方向键.md」）：
    ///
    /// 1. 补全列表开着且有 **两项以上** → 选上 / 下一项。只有一项时不截键：
    ///    截了却没有可选项会变成「按了没反应」。
    /// 2. 光标在全文开头按 ↑ / 末尾按 ↓（且正翻着历史）→ 回溯输入历史（见 `history.rs`）；
    ///    没有上一条 / 下一条时**什么都不做**。有选区时不换历史，交给输入框收起选区。
    /// 3. 其余放行，由输入框自己按**显示行**上下移光标（自动折行的长段落也是一行一行走）；
    ///    移完光标没动（已经在第一 / 最后一个显示行）就补一步：↑ 到开头、↓ 到末尾。
    ///    走到开头 / 末尾之后**再按一次**才换历史。
    fn step_completion(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        // 1. 补全列表：两项以上才接管（只有一项时放行给输入框移光标，列表保持不动）
        let next = self
            .completion
            .open
            .as_ref()
            .and_then(|c| complete::step_index(c.items.len(), c.selected, delta));
        if let Some(next) = next {
            if let Some(c) = self.completion.open.as_mut() {
                c.selected = next;
                // 面板限高了，选中项可能在可视区外：滚回可见（差多少滚多少）
                c.scroll.scroll_to_item(next);
            }
            cx.stop_propagation();
            self.refresh_composer(cx);
            return;
        }
        let (text, cursor, has_selection) = {
            let input = self.input.read(cx);
            (input.value().to_string(), input.cursor(), !input.selected_range().is_empty())
        };
        // 输入框被别处改过（「编辑」、插入文件路径都走 `set_value`，不发 `Change`）：退出回溯，
        // 否则下面按 ↓ 会拿下一条历史把改好的内容冲掉
        self.history.sync(&text);

        if !has_selection {
            // 2. 开头：回溯历史；已经是最早一条 → 什么都不做
            if cursor == 0 && delta < 0 {
                if let Some(prev) = self.history.prev(&text).map(str::to_string) {
                    self.fill_history(&prev, false, window, cx);
                    cx.stop_propagation();
                }
                return;
            }
            // 2b. 末尾且正翻着历史：下一条（走到最新一条之后再按就拿回草稿）
            if cursor == text.len() && delta > 0 && self.history.browsing() {
                if let Some(next) = self.history.next(&text) {
                    self.fill_history(&next, true, window, cx);
                    cx.stop_propagation();
                }
                return;
            }
        }

        // 3. 放行给输入框按显示行移动；在第一 / 最后一个显示行上它不动光标，这时补到开头 / 末尾。
        //    显示行（含自动折行）只有输入框知道，所以不预判，等它移完再看
        if has_selection {
            return;
        }
        cx.defer_in(window, move |this, window, cx| {
            let (now_text, now_cursor) = {
                let input = this.input.read(cx);
                (input.value().to_string(), input.cursor())
            };
            if now_text != text || now_cursor != cursor {
                return;
            }
            let edge = if delta < 0 { 0 } else { text.len() };
            if cursor != edge {
                this.move_caret(edge, window, cx);
            }
        });
    }

    /// 把光标挪到 `offset`（字节）；多行按行列换算。
    fn move_caret(&mut self, offset: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |state, cx| {
            let text = state.value().to_string();
            state.set_cursor_position(position_of(&text, offset), window, cx);
        });
        self.refresh_composer(cx);
    }

    /// 把历史里的一条填进输入框（`to_end` 决定光标放开头还是末尾）。
    ///
    /// `set_value` 不发 `InputEvent::Change`（见 gpui-base 的注释），所以这里要自己
    /// 收掉补全列表、重画输入区；不能让 `update_completion` 跑，否则会拿旧草稿重新开列表。
    fn fill_history(
        &mut self,
        text: &str,
        to_end: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.input.update(cx, |state, cx| state.set_value(text, window, cx));
        self.completion.open = None;
        self.move_caret(if to_end { text.len() } else { 0 }, window, cx);
        self.refresh_composer(cx);
    }

    /// ← / →：光标在触发词的开头（←）/ 末尾（→）时按词跳，其余照常放行给输入框移一格。
    /// 无论哪种，光标挪完都要再对一次触发词（移出 `/` `@` 的范围就把列表收起，移回来再弹）。
    fn on_arrow(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        if self.jump_by_word(delta, window, cx) {
            cx.stop_propagation();
            return;
        }
        // 输入框不广播光标事件（`InputEvent` 只有 Change / PressEnter / Focus / Blur）：
        // 键放行给输入框，光标挪完再比对一次。按键是人手打发，一次内容拷贝不心疼
        cx.defer_in(window, |this, _, cx| this.sync_completion(cx));
    }

    /// 光标正好在触发词的开头（`delta < 0`）/ 末尾（`delta > 0`）时跳到上 / 下一个词，
    /// 返回是否跳了。跳出去之后列表就跟着收起（`sync_completion`）。
    fn jump_by_word(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let (text, cursor, selection) = {
            let input = self.input.read(cx);
            (input.value().to_string(), input.cursor(), input.selected_range())
        };
        if !selection.is_empty() {
            return false;
        }
        let Some((start, end)) = complete::token_bounds(&text, cursor) else {
            return false;
        };
        let target = if delta < 0 {
            if cursor != start {
                return false;
            }
            complete::prev_word_end(&text, start)
        } else {
            if cursor != end {
                return false;
            }
            complete::next_word_start(&text, end)
        };
        if target == cursor {
            return false;
        }
        self.set_input_cursor(&text, target, window, cx);
        self.sync_completion(cx);
        true
    }

    /// 光标挪过之后对一次触发词：触发词没变（种类、范围、查询都一样）就什么都不做，
    /// 换了 / 没了就重算。别的地方（打字）走 `update_completion`，这里只补「只动了光标」这一类
    /// （Esc / Alt+→ 收起过的那份内容由 `update_completion` 开头的 `dismissed` 判断挡住，不会自己弹回来）。
    fn sync_completion(&mut self, cx: &mut Context<Self>) {
        let (text, cursor) = {
            let input = self.input.read(cx);
            (input.value().to_string(), input.cursor())
        };
        let open = self.completion.open.as_ref().map(|c| c.trigger.clone());
        let same = match (&open, complete::trigger_at(&text, cursor)) {
            (Some(open), Some(now)) => {
                now.kind == open.kind && now.range == open.range && now.query == open.query
            }
            (None, None) => true,
            _ => false,
        };
        if !same {
            self.update_completion(cx);
        }
    }

    /// Alt+←：光标处有触发词时重新展开列表（按 Esc / Alt+→ 收起过的那份内容也照弹）。
    fn expand_completion(&mut self, cx: &mut Context<Self>) {
        let (text, cursor) = {
            let input = self.input.read(cx);
            (input.value().to_string(), input.cursor())
        };
        if complete::trigger_at(&text, cursor).is_none() {
            cx.propagate();
            return;
        }
        cx.stop_propagation();
        // 已经开着就不用再建一遍（免得选中项被重置）
        if self.completion.open.is_some() {
            return;
        }
        // 明确要它弹出来：先把 Esc 收起时的快照清掉，不然 update_completion 会当场返回
        self.completion.dismissed = None;
        self.update_completion(cx);
    }

    /// Alt+→：收起列表（只收起，不填入）。和 Esc 一样留下快照，光标来回挪不会再弹回来。
    fn collapse_completion(&mut self, cx: &mut Context<Self>) {
        if self.completion.open.take().is_none() {
            cx.propagate();
            return;
        }
        cx.stop_propagation();
        self.completion.dismissed = Some(self.input.read(cx).value().to_string());
        self.refresh_composer(cx);
    }

    fn accept_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(selected) = self.completion.open.as_ref().map(|c| c.selected) else {
            return;
        };
        cx.stop_propagation();
        self.accept_completion(selected, window, cx);
    }

    /// 输入框正上方的候选列表（浮在对话上面，不挤布局）；没开时为 None。
    ///
    /// `window` 用来量「面板最多能有多高」：面板锚在输入框上沿往上长，窗口矮 / 字号大时
    /// 不限高就会顶出窗口，最上面几项既看不见也点不到。高度按 `composer_top`（输入框卡片
    /// 上沿在窗口里的位置）减去顶栏留一点边距算，超出的部分在面板里滚动。
    pub(super) fn render_completion(&self, window: &Window, cx: &mut Context<Self>) -> Option<Div> {
        let c = self.completion.open.as_ref()?;
        let heading = match c.trigger.kind {
            TriggerKind::Slash => "指令",
            TriggerKind::Mention => "引用文件",
        };
        let rem = window.rem_size();
        // 输入框上沿还没量过（第一帧）就不限高，下一帧补上
        let max_h = self.composer_top.get().map(|top| {
            let gap = rems(1.).to_pixels(rem); // 和顶栏之间留一点
            let chrome = CHROME_H.to_pixels(rem);
            // 面板自己离输入框还有 mb_1p5
            let above = rems(0.375).to_pixels(rem);
            (top - chrome - gap - above).max(rems(6.).to_pixels(rem))
        });
        // 标题行和面板内边距占掉的部分（`rows` 的限高从这里扣）
        let chrome_h = rems(2.75).to_pixels(rem);
        let header = div()
            .h_flex()
            .flex_shrink_0()
            .justify_between()
            .px_2p5()
            .pt_1()
            .pb_0p5()
            .text_size(TEXT_META)
            .text_color(rgb(INK_SOFT))
            .child(heading)
            .child(format!(
                "{} · Esc / {} 收起",
                if c.items.len() >= 2 {
                    "↑↓ 选择 · 回车 / Tab 确认"
                } else {
                    "Tab 填入"
                },
                Self::key_text(COLLAPSE_COMPLETION_KEY),
            ));
        let mut rows = div().id("completion-rows").v_flex().gap_0p5().min_h_0();
        // 条目多了（文件很多、插件命令很多）要能滚：只在限高时才开滚动区
        if let Some(h) = max_h {
            rows = rows
                .max_h((h - chrome_h).max(rems(3.).to_pixels(rem)))
                .overflow_y_scroll()
                .track_scroll(&c.scroll);
        }
        for (i, item) in c.items.iter().enumerate() {
            let selected = i == c.selected;
            rows = rows.child(
                div()
                    .id(("completion", i))
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .px_2p5()
                    .py_1p5()
                    .rounded_md()
                    .cursor_pointer()
                    .when(selected, |d| d.bg(rgb(SELECTED)))
                    .when(!selected, |d| d.hover(|d| d.bg(rgb(HOVER))))
                    .on_click(
                        cx.listener(move |this, _, window, cx| {
                            this.accept_completion(i, window, cx)
                        }),
                    )
                    .child(Icon::new(item.icon).size_4().text_color(rgb(INK_SOFT)))
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(TEXT_BODY)
                            .text_color(rgb(INK))
                            .when(c.trigger.kind == TriggerKind::Mention, |d| d.font_family(MONO))
                            .child(item.title.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(TEXT_SMALL)
                            .text_color(rgb(INK_SOFT))
                            .child(item.detail.clone()),
                    )
                    .when(item.current, |d| {
                        d.child(Icon::new(IconAll::Check).size_4().text_color(rgb(CORAL)))
                    }),
            );
        }
        Some(
            div()
                .v_flex()
                .min_h_0()
                .gap_0p5()
                .p_1()
                .rounded_xl()
                .border_1()
                .border_color(rgb(LINE))
                .bg(rgb(CARD))
                .shadow_md()
                .child(header)
                .child(rows),
        )
    }
}

/// 把字节偏移换算成输入框用的行列（0 起）：行按 `\n` 数，列按字符数（不是字节）。
/// 偏移落在多字节字符中间时向前取整（`cursor` 理论上总在边界上，这里兜个底）。
fn cursor_line_column(text: &str, offset: usize) -> (u32, u32) {
    let mut offset = offset.min(text.len());
    while offset > 0 && !text.is_char_boundary(offset) {
        offset -= 1;
    }
    let before = &text[..offset];
    let line = before.matches('\n').count() as u32;
    let column = before.rsplit('\n').next().unwrap_or(before).chars().count() as u32;
    (line, column)
}

/// 字节偏移 → 输入框的光标位置（第几行、第几个字）。
fn position_of(text: &str, offset: usize) -> Position {
    let (line, column) = cursor_line_column(text, offset);
    Position::new(line, column)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[core::prelude::v1::test]
    fn line_column_is_zero_based_and_counts_chars() {
        // 列按字符数：`"ab\ncd"` 里 `c` 的字节偏移是 3，列 0；`d` 是 4，列 1
        assert_eq!(cursor_line_column("", 0), (0, 0));
        assert_eq!(cursor_line_column("abc", 0), (0, 0));
        assert_eq!(cursor_line_column("abc", 3), (0, 3));
        assert_eq!(cursor_line_column("ab\ncd", 0), (0, 0));
        assert_eq!(cursor_line_column("ab\ncd", 3), (1, 0));
        assert_eq!(cursor_line_column("ab\ncd", 4), (1, 1));
        // 多字节：按字符数，不按字节（"中文" 各 3 字节，`\n` 在偏移 6，`好` 在 7..10）
        assert_eq!(cursor_line_column("中文\n好", 6), (0, 2));
        assert_eq!(cursor_line_column("中文\n好", 7), (1, 0));
        assert_eq!(cursor_line_column("中文\n好", "中文\n好".len()), (1, 1));
        // 偏移落在多字节字符中间：向前取整，不 panic
        assert_eq!(cursor_line_column("中文\n好", 1), (0, 0));
    }

    /// 光标位置：换行算行、多字节按字算列，超出末尾夹到末尾
    #[core::prelude::v1::test]
    fn position_counts_lines_and_chars() {
        assert_eq!(position_of("/mode", 0), Position::new(0, 0));
        assert_eq!(position_of("a\nbc", 4), Position::new(1, 2));
        assert_eq!(position_of("@设置", 4), Position::new(0, 2));
        assert_eq!(position_of("ab", 99), Position::new(0, 2));
    }
}

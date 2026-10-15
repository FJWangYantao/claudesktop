//! 输入框的 `/指令` 和 `@文件` 补全：输入时在输入框正上方弹出候选列表，
//! 上下键选、回车或 Tab 接受、Esc 收起；鼠标点也行。识别与排序的纯逻辑在 `crate::complete`。
//!
//! 键盘接管：输入框把上下键 / 回车 / Tab / Esc 绑成了自己的动作（`MoveUp` / `Enter` …），
//! 列表开着时在输入框卡片上用 `capture_action` 先截下来（动作分发先走 capture 阶段，
//! 从根到焦点），处理了就 `stop_propagation`，输入框收不到；列表没开时放行。

use super::ChatApp;
use crate::complete::{self, Trigger, TriggerKind};
use crate::model::ViewMode;
use crate::settings::KernelChoice;
use crate::theme::rgb;
use crate::theme::*;
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::input::{Enter, Escape, IndentInline, MoveDown, MoveUp, Position};
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
            cx.notify();
            return;
        };
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
            Completion { trigger, items, selected }
        });
        cx.notify();
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
            _ => String::new(),
        };
        self.replace_input_range(c.trigger.range, &replacement, window, cx);
        match item.action {
            CompletionAction::Mention(_) => {}
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
            let before = &next[..start + with.len()];
            let line = before.matches('\n').count() as u32;
            let column = before.rsplit('\n').next().unwrap_or(before).chars().count() as u32;
            state.set_value(next, window, cx);
            state.set_cursor_position(Position::new(line, column), window, cx);
        });
    }

    /// 输入框卡片上挂的键盘接管（见模块说明）。
    pub(super) fn with_completion_keys(
        &self,
        card: Stateful<Div>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        card.capture_action(cx.listener(|this, _: &MoveUp, _, cx| this.step_completion(-1, cx)))
            .capture_action(cx.listener(|this, _: &MoveDown, _, cx| this.step_completion(1, cx)))
            .capture_action(cx.listener(|this, action: &Enter, window, cx| {
                // Shift+回车照常换行
                if action.shift {
                    return;
                }
                this.accept_selected(window, cx);
            }))
            .capture_action(
                cx.listener(|this, _: &IndentInline, window, cx| this.accept_selected(window, cx)),
            )
            .capture_action(cx.listener(|this, _: &Escape, _, cx| {
                if this.completion.open.take().is_some() {
                    this.completion.dismissed = Some(this.input.read(cx).value().to_string());
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
    }

    fn step_completion(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(c) = self.completion.open.as_mut() else {
            return;
        };
        let n = c.items.len() as isize;
        c.selected = (c.selected as isize + delta).rem_euclid(n) as usize;
        cx.stop_propagation();
        cx.notify();
    }

    fn accept_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(selected) = self.completion.open.as_ref().map(|c| c.selected) else {
            return;
        };
        cx.stop_propagation();
        self.accept_completion(selected, window, cx);
    }

    /// 输入框正上方的候选列表（浮在对话上面，不挤布局）；没开时为 None。
    pub(super) fn render_completion(&self, cx: &mut Context<Self>) -> Option<Div> {
        let c = self.completion.open.as_ref()?;
        let heading = match c.trigger.kind {
            TriggerKind::Slash => "指令",
            TriggerKind::Mention => "引用文件",
        };
        let mut list = div()
            .v_flex()
            .gap_0p5()
            .p_1()
            .rounded_xl()
            .border_1()
            .border_color(rgb(LINE))
            .bg(rgb(CARD))
            .shadow_md()
            .child(
                div()
                    .h_flex()
                    .justify_between()
                    .px_2p5()
                    .pt_1()
                    .pb_0p5()
                    .text_size(TEXT_META)
                    .text_color(rgb(INK_SOFT))
                    .child(heading)
                    .child("↑↓ 选择 · 回车 / Tab 确认 · Esc 关闭"),
            );
        for (i, item) in c.items.iter().enumerate() {
            let selected = i == c.selected;
            list = list.child(
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
        Some(list)
    }
}

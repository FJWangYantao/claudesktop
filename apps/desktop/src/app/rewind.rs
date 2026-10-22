//! 检查点回退：用户消息下方的「回退」，回到这条消息之前（撤回之后的对话、还原 agent 改过的文件）。
//!
//! - 浮层打开时算一次预览（[`RewindPreview`]：读日志、比对磁盘），关上时丢掉；点了哪种回退就照预览执行。
//! - 文件快照、还原计划、冲突判断在 `claudesktop_core::checkpoint`；日志只追加，回退记一条 `Record::Rewind`。
//! - 对话只在内核支持时回退（自研内核按日志重建上下文）；ACP agent 只能还原文件。
//!
//! 设计见仓库根目录「设计说明-检查点与回退.md」。

use super::ChatApp;
use crate::model::{ChatItem, PendingImage};
use crate::session::rewind_note;
use crate::settings::KernelChoice;
use crate::theme::rgb;
use crate::theme::*;
use claudesktop_core::Record;
use claudesktop_core::checkpoint::{self, FileRestore};
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::base::PopoverState;
use gpui_kit::component::button::{ButtonVariant, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::popover::Popover;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::path::Path;

/// 确认浮层的宽度
const REWIND_W: Rems = rems(27.);

/// 浮层里列出的文件名最多几个（其余写「等 N 个」）
const NAMES_SHOWN: usize = 3;

/// 「回退」浮层要显示的东西：打开时算一次。
pub(crate) struct RewindPreview {
    /// 会话令牌 + 用户消息在对话里的行号：浮层对应哪条消息
    key: (u64, usize),
    /// 第几条用户消息（从 0 数）
    turn: usize,
    prompt: String,
    /// 会撤回几轮对话（含这一条）
    rounds: usize,
    /// 要还原的文件，及磁盘上的现状和 agent 改完的样子对不上（冲突）
    files: Vec<(FileRestore, bool)>,
    /// 这几轮跑过几条命令（它们改的文件还原不了）
    commands: usize,
    /// 内核能回退对话
    conversation: bool,
    /// 聊天会话（没有工作目录，也就没有文件可还原）
    chat: bool,
    /// 回退不了的原因（没有保存记录、对话和记录对不上）
    error: Option<&'static str>,
}

impl RewindPreview {
    /// 对应对话里的哪一行
    pub(crate) fn item(&self) -> usize {
        self.key.1
    }

    fn conflicts(&self) -> impl Iterator<Item = &FileRestore> {
        self.files.iter().filter(|(_, c)| *c).map(|(r, _)| r)
    }

    /// 勾不勾「也覆盖」时实际会还原几个文件
    fn restorable(&self, overwrite: bool) -> usize {
        self.files.iter().filter(|(_, c)| overwrite || !c).count()
    }
}

impl ChatApp {
    /// 第 `idx` 个会话的有效记录（回退折算过）；先把合并中的文本落盘。没有保存记录时为 None。
    pub(super) fn effective_records(&mut self, idx: usize) -> Option<Vec<Record>> {
        let store = self.store.as_ref()?;
        let s = &mut self.sessions[idx];
        let meta = s.meta.as_ref()?;
        if let Some(log) = &mut s.log {
            let _ = log.flush();
        }
        store.load(&meta.id).ok().map(|records| claudesktop_core::effective(&records))
    }

    /// 当前会话第 `item` 行（一条用户消息）的回退预览。
    fn rewind_preview(&mut self, item: usize) -> RewindPreview {
        let active = self.active;
        let s = &self.sessions[active];
        let turn =
            s.transcript[..item].iter().filter(|i| matches!(i, ChatItem::User { .. })).count();
        let prompt = match s.transcript.get(item) {
            Some(ChatItem::User { text, .. }) => text.to_string(),
            _ => String::new(),
        };
        // 历史会话的内核可能还没起来：自研内核续接时按日志重建，照样能回退
        let conversation = match self.kernel_of(&s.kernel) {
            Some(kernel) => kernel.supports_rewind(),
            None => s.kernel == KernelChoice::Builtin,
        };
        let mut preview = RewindPreview {
            key: (s.token, item),
            turn,
            prompt,
            rounds: 0,
            files: Vec::new(),
            commands: 0,
            conversation,
            chat: s.is_chat(),
            error: None,
        };
        let Some(records) = self.effective_records(active) else {
            preview.error = Some("这个会话没有保存记录（演示回放不落盘），回退不了");
            return preview;
        };
        // 对话里的第 turn 条用户消息要和日志里的对得上，否则宁可不做
        let logged = checkpoint::user_index(&records, turn).and_then(|i| match &records[i] {
            Record::User { text, .. } => Some(text.as_str()),
            _ => None,
        });
        if logged != Some(preview.prompt.as_str()) {
            preview.error = Some("对话和保存的记录对不上，回退不了");
            return preview;
        }
        let users = records.iter().filter(|r| matches!(r, Record::User { .. })).count();
        preview.rounds = users - turn;
        preview.files = checkpoint::restore_plan(&records, turn)
            .into_iter()
            .map(|r| {
                let conflict = checkpoint::is_conflict(&r);
                (r, conflict)
            })
            .collect();
        preview.commands = checkpoint::commands_after(&records, turn);
        preview
    }

    /// 照浮层里的预览回退：`conversation` 撤回对话，`files` 还原文件（冲突的看「也覆盖」勾没勾）。
    fn rewind_to(
        &mut self,
        item: usize,
        conversation: bool,
        files: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let active = self.active;
        let Some(p) = self.rewind.take() else {
            return;
        };
        let s = &self.sessions[active];
        if p.key != (s.token, item) || p.error.is_some() || s.busy {
            return;
        }
        let conversation = conversation && p.conversation;
        let overwrite = std::mem::take(&mut self.rewind_overwrite);
        let (Some(store), Some(id)) = (self.store.clone(), s.meta.as_ref().map(|m| m.id.clone()))
        else {
            return;
        };

        // 先写文件：(路径, 还原前的现状, 还原成的内容)
        let mut restored = Vec::new();
        let mut failed = Vec::new();
        for (r, conflict) in p.files.iter().filter(|_| files) {
            if *conflict && !overwrite {
                continue;
            }
            // 现状先存一份：覆盖掉的内容以后还能再回退找回来
            let current = match checkpoint::read_snapshot(Path::new(&r.path)) {
                Some(Some(text)) => store.save_blob(&id, &text).ok(),
                _ => None,
            };
            match checkpoint::restore(&store, &id, r) {
                Ok(()) => restored.push((r.path.clone(), current, r.target.clone())),
                Err(err) => failed.push(format!("{}（{err}）", file_name(&r.path))),
            }
        }
        // 只还原文件时，还原本身也记成一次改动：之后再往更早回退，「预期现状」才对得上。
        // 回退对话时那几轮的记录整个作废，文件也回到了作废前的样子，不用记
        if !conversation {
            for (path, before, after) in &restored {
                let record = Record::FileChanged {
                    path: path.clone(),
                    before: before.clone(),
                    after: after.clone(),
                    reverted: None,
                };
                self.persist(active, record);
            }
        }
        let rewind = Record::Rewind {
            turn: p.turn,
            conversation,
            files: restored.len(),
            prompt: p.prompt.clone(),
        };
        self.persist(active, rewind);

        if conversation {
            // 这条消息（连同图片）放回输入框，对话截到它之前，内核的上下文跟着换
            let images = match &self.sessions[active].transcript[item] {
                ChatItem::User { images, .. } => images.clone(),
                _ => Vec::new(),
            };
            self.edit_message(item, window, cx);
            self.pending.extend(images.iter().filter_map(|p| PendingImage::from_path(p)));
            self.sessions[active].truncate_to(item);
            // 还没续接的历史会话不用管：续接时按日志重建，回退过的轮次自然不在
            let kernel = self.session_kernel(active);
            if let (Some(kernel), Some(sid)) = (kernel, self.sessions[active].acp_id.clone())
                && let Some(records) = self.effective_records(active)
                && let Err(err) = kernel.rewind(&sid, &records)
            {
                self.sessions[active].note(format!("内核的上下文没能跟着回退：{err}"));
            }
        }
        let s = &mut self.sessions[active];
        s.note(rewind_note(conversation, restored.len(), &p.prompt));
        if !failed.is_empty() {
            s.note(format!("有 {} 个文件没能还原：{}", failed.len(), failed.join("、")));
        }
        if let Some(log) = &mut s.log {
            let _ = log.flush();
        }
        s.follow_tail();
        self.focus_input(window, cx);
        cx.notify();
    }

    /// 用户消息下方的「回退」：点开是确认浮层（行内确认，不弹模态框）。这一轮在跑时置灰。
    pub(super) fn rewind_action(&self, item: usize, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.session().busy;
        let button = Self::message_action(("rewind-user", item), IconAll::Undo2, "回退", cx);
        if busy {
            return button.disabled(true).tooltip("这一轮还在跑，先停下再回退").into_any_element();
        }
        let handle = cx.entity();
        let on_open = handle.clone();
        Popover::new(("rewind-menu", item))
            .anchor(Anchor::TopRight)
            // rem：三个按钮跟着界面缩放也排得下一行
            .w(REWIND_W)
            .trigger(button.tooltip("回到这条消息之前：撤回之后的对话，还原 agent 改过的文件"))
            .bg(rgb(CARD))
            .border_1()
            .border_color(rgb(LINE))
            .rounded_lg()
            .p_3()
            // 打开时算预览（读日志、比对磁盘），关上时丢掉，下次打开重算
            .on_open_change(move |open, _, cx| {
                on_open.update(cx, |this, cx| {
                    this.rewind = open.then(|| this.rewind_preview(item));
                    this.rewind_overwrite = false;
                    cx.notify();
                });
            })
            .content(move |_, _, cx| {
                let popover = cx.entity();
                let app = handle.read(cx);
                match app.rewind.as_ref().filter(|p| p.key.1 == item) {
                    Some(p) => rewind_panel(p, app.rewind_overwrite, &handle, &popover, item),
                    None => div(),
                }
            })
            .into_any_element()
    }
}

/// 确认浮层：标题、会发生什么（撤回几轮 / 还原哪些文件）、还原不了什么、三种回退。
fn rewind_panel(
    p: &RewindPreview,
    overwrite: bool,
    handle: &Entity<ChatApp>,
    popover: &Entity<PopoverState>,
    item: usize,
) -> Div {
    let title = format!("回到「{}」之前", crate::model::title_from_prompt(&p.prompt));
    let mut panel = div().v_flex().gap_2().text_size(TEXT_SMALL).text_color(rgb(INK_SOFT)).child(
        div()
            .text_size(TEXT_BODY)
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(rgb(INK))
            .child(title),
    );
    if let Some(error) = p.error {
        return panel.child(div().child(error));
    }
    let line = |text: String| {
        div()
            .h_flex()
            .items_start()
            .gap_1p5()
            .child("·")
            .child(div().flex_1().min_w_0().child(text))
    };
    let warn = |text: String| {
        div()
            .h_flex()
            .items_start()
            .gap_1p5()
            .child(Icon::new(IconAll::TriangleAlert).size_3p5().mt_0p5().text_color(rgb(RED)))
            .child(div().flex_1().min_w_0().child(text))
    };

    panel = panel.child(line(if p.conversation {
        format!("撤回这条消息及之后的 {} 轮对话，消息放回输入框", p.rounds)
    } else {
        "这个内核不支持回退对话（上下文在 agent 进程里），只能还原文件".into()
    }));
    let all: Vec<&FileRestore> = p.files.iter().map(|(r, _)| r).collect();
    // 聊天会话没有工作目录，也就不可能有文件改动：只列对话那一项
    let chat = p.chat;
    if !chat {
        panel = panel.child(line(if all.is_empty() {
            "agent 在这之后没有改过文件".into()
        } else {
            format!("还原 {} 个文件：{}", all.len(), names(&all))
        }));
        if p.commands > 0 {
            panel = panel
                .child(warn(format!("这几轮跑过 {} 条命令，命令改动的文件不会还原", p.commands)));
        }
    }
    let conflicts: Vec<&FileRestore> = p.conflicts().collect();
    if !conflicts.is_empty() {
        let handle = handle.clone();
        panel = panel
            .child(warn(format!(
                "{} 个文件在 agent 改过之后又被改过，默认不动：{}",
                conflicts.len(),
                names(&conflicts)
            )))
            .child(
                div().pl_5().child(
                    Checkbox::new("rewind-overwrite")
                        .text_size(TEXT_SMALL)
                        .checked(overwrite)
                        .label("也覆盖这些文件（现在的内容会先存一份）")
                        .on_click(move |checked, _, cx| {
                            let checked = *checked;
                            handle.update(cx, |this, cx| {
                                this.rewind_overwrite = checked;
                                cx.notify();
                            });
                        }),
                ),
            );
    }

    // 按钮：能做的才出现；主按钮是最完整的那种
    let files = !chat && p.restorable(overwrite) > 0;
    let mut choices: Vec<(&'static str, bool, bool)> = Vec::new();
    if p.conversation && files {
        choices.push(("回退对话和文件", true, true));
        choices.push(("只回退对话", true, false));
    } else if p.conversation {
        choices.push(("回退对话", true, false));
    }
    if files {
        choices.push(("只还原文件", false, true));
    }
    let mut row = div().h_flex().flex_wrap().gap_2().pt_1();
    for (n, (label, conversation, restore)) in choices.into_iter().enumerate() {
        let (handle, popover) = (handle.clone(), popover.clone());
        row = row.child(
            ChatApp::text_button(("rewind-choice", n), label, n == 0)
                .when(n > 0, |b| b.with_variant(ButtonVariant::Secondary))
                .on_click(move |_, window, cx| {
                    // 先执行再收起：收起会清掉预览
                    handle.update(cx, |this, cx| {
                        this.rewind_to(item, conversation, restore, window, cx)
                    });
                    popover.update(cx, |s, cx| s.dismiss(window, cx));
                }),
        );
    }
    panel.child(row)
}

/// 「a.rs、b.rs、c.rs 等 5 个」：只写文件名，悬停整行看不到全路径也够认。
fn names(files: &[&FileRestore]) -> String {
    let mut text =
        files.iter().take(NAMES_SHOWN).map(|r| file_name(&r.path)).collect::<Vec<_>>().join("、");
    if files.len() > NAMES_SHOWN {
        text += &format!(" 等 {} 个", files.len());
    }
    text
}

pub(super) fn file_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map_or_else(|| path.to_string(), |n| n.to_string_lossy().into_owned())
}

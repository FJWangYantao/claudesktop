//! 单个对话会话的状态，以及把内核事件 / 演示步骤应用到对话记录上。

use crate::demo::*;
use crate::model::*;
use crate::settings::KernelChoice;
use claudesktop_core::{DiffView, Kernel, KernelEvent, Record, SessionLog, SessionMeta};
use gpui_kit::*;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

/// 内核没上报模型时显示的模型名（统计时当作「未知模型」）
pub const UNKNOWN_MODEL: &str = "默认模型";

/// 一个对话会话：自己的对话记录、忙闲、模式、模型。一条内核连接上可以同时有多个。
pub struct Session {
    pub title: SharedString,
    /// 这个会话跑在哪种内核上（空会话可以换，有对话后固定）
    pub kernel: KernelChoice,
    /// 创建时的令牌，用来把内核回的 Started / SessionFailed 对回这个会话
    pub token: u64,
    /// ACP sessionId（Started 之后才有；演示回放模式为 None）
    pub acp_id: Option<String>,
    /// 已向内核发出 session/new
    pub requested: bool,
    /// 可以发提示词：Started 已到，或演示回放模式
    pub ready: bool,
    pub transcript: Vec<ChatItem>,
    pub busy: bool,
    /// 演示回放用：递增即取消正在播放的脚本
    pub cancel_token: u64,
    /// 对话记录的虚拟列表状态（只渲染可见的行）；默认跟随到底，用户往上翻就停，翻回底部再跟
    pub list: ListState,
    /// 每行上次测量时的 [`ChatItem::fingerprint`]
    pub measured: Vec<u64>,
    /// 当前可选模式（默认静态表，Started 后换成 agent 上报的）
    pub modes: Vec<ModeEntry>,
    pub permission_mode: usize, // modes 下标
    pub model_label: SharedString,
    /// 自动演示：会话就绪后发送的提示词
    pub autoprompt: Option<String>,
    /// 会话工作目录（侧栏按它分组；session/new 用它）
    pub cwd: PathBuf,
    /// 持久化元信息：首条消息发出后才建档（空会话不落盘）
    pub meta: Option<SessionMeta>,
    /// 追加式日志；恢复的会话在第一次写入时才打开
    pub log: Option<SessionLog>,
    /// 从磁盘恢复、还没接上内核会话的历史会话（不自动 session/new）
    pub restored: bool,
    /// 历史会话正在续接：Started 到达时告诉用户上下文是否延续
    pub resuming: bool,
    /// 内核会话建立前选的模型：Started 之后再告诉内核
    pub pending_model: Option<String>,
    /// 本轮累计的 token，一轮结束时写进日志后清零
    pub turn_usage: TurnUsage,
    /// 本轮开始的时刻（状态条计时用；只在 busy 时有意义）
    pub turn_started: Option<Instant>,
    /// 用户手动改过标题（首条消息不再覆盖）
    pub renamed: bool,
}

/// 一轮里累计的 token 用量（内核的 `UsageUpdated` 逐次累加）。
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct TurnUsage {
    pub input: u64,
    pub output: u64,
    /// 其中思考消耗的部分（包含在 `output` 里；内核不报告时为 0）
    pub reasoning: u64,
}

/// 进行中的一轮此刻在干什么（状态条的左侧文字）。
#[derive(Debug, Clone, PartialEq)]
pub enum Activity {
    /// 有权限请求等用户应答
    WaitingPermission,
    /// 有工具正在执行（取本轮最近一个）
    Tool { name: SharedString, arg: SharedString },
    Thinking,
    Replying,
    /// 已发出，还没收到任何东西
    Waiting,
}

fn new_list_state() -> ListState {
    // overdraw：视口外多渲染的高度，滚动时少一点空白
    let state = ListState::new(0, ListAlignment::Top, px(600.));
    state.set_follow_mode(FollowMode::Tail);
    state
}

impl Session {
    /// 新会话；演示回放不需要内核会话，直接可用。
    pub fn new(token: u64, kernel: KernelChoice, cwd: PathBuf) -> Self {
        Self {
            title: "新会话".into(),
            kernel,
            token,
            acp_id: None,
            requested: false,
            ready: kernel == KernelChoice::Demo,
            transcript: Vec::new(),
            busy: false,
            cancel_token: 0,
            list: new_list_state(),
            measured: Vec::new(),
            modes: default_modes(),
            permission_mode: 0,
            // 演示回放用假模型名；真内核等 Started 上报，没上报就一直显示「默认模型」
            model_label: if kernel == KernelChoice::Demo { "GLM-5.3" } else { UNKNOWN_MODEL }.into(),
            autoprompt: None,
            cwd,
            meta: None,
            log: None,
            restored: false,
            resuming: false,
            pending_model: None,
            turn_usage: TurnUsage::default(),
            turn_started: None,
            renamed: false,
        }
    }

    /// 从持久化记录重放出一个历史会话：事件走与实时完全相同的映射。
    pub fn from_records(token: u64, meta: SessionMeta, records: Vec<Record>) -> Self {
        let mut s = Self::new(token, KernelChoice::of_agent(&meta.agent), meta.cwd.clone());
        s.ready = false;
        s.title = meta.title.clone().into();
        s.restored = true;
        for record in records {
            match record {
                Record::User { text } => s.transcript.push(ChatItem::User(text.into())),
                Record::Event { event } => {
                    s.apply_event(event, None);
                }
                Record::Turn { .. } => {}
                Record::PermissionResolved { id, label } => {
                    let item = s.transcript.iter_mut().rev().find(|i| {
                        matches!(i, ChatItem::Permission { id: pid, resolved: None, .. } if *pid == id)
                    });
                    if let Some(ChatItem::Permission { resolved, .. }) = item {
                        *resolved = Some(label.into());
                    }
                }
            }
        }
        // 上次退出时还在进行的东西一律收尾
        s.finish_thinking();
        for item in &mut s.transcript {
            match item {
                ChatItem::Permission { resolved: r @ None, .. } => *r = Some("已取消".into()),
                ChatItem::Tool(card) if card.phase == ToolPhase::Running => {
                    card.phase = ToolPhase::Failed;
                    if card.result.is_empty() {
                        card.result = "已中断".into();
                    }
                }
                _ => {}
            }
        }
        s.busy = false;
        s.meta = Some(meta);
        s
    }

    /// 把对话记录的变化同步给虚拟列表：新增的行追加，高度可能变了的行重新测量。
    /// 每次渲染前调用；只比对指纹，不做布局，1 万行也很便宜。
    pub fn sync_list(&mut self) {
        let n = self.transcript.len();
        if n < self.measured.len() {
            // 记录只会变长；万一变短就整体重来
            self.list.reset(0);
            self.measured.clear();
        }
        let old = self.measured.len();
        for i in 0..old {
            let f = self.transcript[i].fingerprint();
            if f != self.measured[i] {
                self.measured[i] = f;
                self.list.remeasure_items(i..i + 1);
            }
        }
        if n > old {
            self.measured
                .extend(self.transcript[old..].iter().map(ChatItem::fingerprint));
            self.list.splice(old..old, n - old);
        }
    }

    /// 发消息时重新跟到底部（哪怕之前往上翻过）
    pub fn follow_tail(&self) {
        self.list.set_follow_mode(FollowMode::Tail);
    }

    pub fn has_user_message(&self) -> bool {
        self.transcript
            .iter()
            .any(|i| matches!(i, ChatItem::User(_)))
    }

    /// 有还没应答的权限请求
    pub fn waiting_permission(&self) -> bool {
        self.transcript
            .iter()
            .any(|i| matches!(i, ChatItem::Permission { resolved: None, .. }))
    }

    /// 一轮开始：置忙并开始计时。
    pub fn begin_turn(&mut self) {
        self.busy = true;
        self.turn_started = Some(Instant::now());
    }

    /// 本轮此刻的活动：等授权 > 执行中的工具 > 最后一条记录的类型。
    /// 只看本轮（最后一条用户消息之后）。
    pub fn activity(&self) -> Activity {
        if self.waiting_permission() {
            return Activity::WaitingPermission;
        }
        let turn_start = self
            .transcript
            .iter()
            .rposition(|i| matches!(i, ChatItem::User(_)))
            .map_or(0, |i| i + 1);
        let turn = &self.transcript[turn_start..];
        let running = turn.iter().rev().find_map(|item| match item {
            ChatItem::Tool(card) if card.phase == ToolPhase::Running => Some(card),
            _ => None,
        });
        if let Some(card) = running {
            return Activity::Tool {
                name: card.name.clone(),
                arg: card.arg.clone(),
            };
        }
        match turn.last() {
            Some(ChatItem::Thinking { streaming: true, .. }) => Activity::Thinking,
            Some(ChatItem::Agent(_)) => Activity::Replying,
            _ => Activity::Waiting,
        }
    }

    pub fn note(&mut self, text: impl Into<SharedString>) {
        self.transcript.push(ChatItem::Note(text.into()));
    }

    /// 整段对话导出为 Markdown（「分享」按钮用）。
    pub fn to_markdown(&self) -> String {
        let mut out = format!(
            "# {}\n\n> 内核：{} · 模型：{}\n\n",
            self.title,
            self.kernel.label(),
            self.model_label
        );
        for item in &self.transcript {
            match item {
                ChatItem::User(text) => out += &format!("## 我\n\n{text}\n\n"),
                ChatItem::Agent(text) => out += &format!("## Claude\n\n{text}\n\n"),
                ChatItem::Thinking { text, .. } => {
                    out += &format!("<details><summary>思考过程</summary>\n\n{text}\n\n</details>\n\n");
                }
                ChatItem::Info { text, .. } => {
                    out += &format!("<details><summary>启动信息</summary>\n\n```{text}```\n\n</details>\n\n");
                }
                ChatItem::Tool(card) => {
                    out += &format!("> **{}** `{}` → {}\n\n", card.name, card.arg, card.result);
                }
                ChatItem::Permission { title, resolved, .. } => {
                    let state = resolved.as_deref().unwrap_or("待处理");
                    out += &format!("> 权限请求 `{title}`（{state}）\n\n");
                }
                ChatItem::Plan(rows) => {
                    out += "**计划**\n\n";
                    for row in rows.iter() {
                        out += &format!(
                            "{} {}\n",
                            if row.done { "- [x]" } else { "- [ ]" },
                            row.text
                        );
                    }
                    out += "\n";
                }
                ChatItem::Note(text) => out += &format!("*{text}*\n\n"),
            }
        }
        out
    }

    pub fn finish_thinking(&mut self) {
        if let Some(ChatItem::Thinking { streaming, open, .. }) = self.transcript.last_mut() {
            *streaming = false;
            *open = false;
        }
    }

    /// 应答某条权限请求：`Some((选项 ID, 展示名))` 选择，`None` 取消。
    pub fn resolve_permission(
        &mut self,
        idx: usize,
        choice: Option<(String, String)>,
        kernel: Option<&Arc<dyn Kernel>>,
    ) {
        let Some(kernel) = kernel else {
            return;
        };
        if let Some(ChatItem::Permission { id, resolved, .. }) = self.transcript.get_mut(idx) {
            if resolved.is_some() {
                return;
            }
            let label: SharedString = match &choice {
                Some((_, name)) => name.clone().into(),
                None => "已取消".into(),
            };
            *resolved = Some(match kernel.resolve_permission(*id, choice.map(|(i, _)| i)) {
                Ok(()) => label,
                Err(err) => format!("应答失败：{err}").into(),
            });
        }
    }

    /// 本轮结束 / 停止 / 内核出错时，仍在等待的请求一律按「取消」收尾。
    pub fn cancel_pending_permissions(&mut self, kernel: Option<&Arc<dyn Kernel>>) {
        let Some(kernel) = kernel else {
            return;
        };
        for item in &mut self.transcript {
            if let ChatItem::Permission { id, resolved, .. } = item {
                if resolved.is_none() {
                    let _ = kernel.resolve_permission(*id, None);
                    *resolved = Some("已取消".into());
                }
            }
        }
    }

    /// 内核要换了：收尾进行中的一轮，断开与旧内核会话的关联。
    /// 有对话的会话变成「历史会话」，下次发消息时在新内核上续接。
    pub fn detach_kernel(&mut self, kernel: Option<&Arc<dyn Kernel>>) {
        self.cancel_pending_permissions(kernel);
        self.cancel_token += 1;
        if self.busy {
            self.busy = false;
            self.finish_thinking();
            for item in &mut self.transcript {
                if let ChatItem::Tool(card) = item {
                    if card.phase == ToolPhase::Running {
                        card.phase = ToolPhase::Failed;
                        if card.result.is_empty() {
                            card.result = "已中断".into();
                        }
                    }
                }
            }
            self.note("内核已切换，这一轮已中断");
        }
        self.acp_id = None;
        self.requested = false;
        self.resuming = false;
        self.ready = false;
        self.autoprompt = None;
        self.restored = self.has_user_message();
    }

    /// 内核致命错误：这个会话不再可用。
    pub fn on_fatal(&mut self, message: &str, kernel: Option<&Arc<dyn Kernel>>) {
        self.busy = false;
        self.ready = false;
        self.cancel_pending_permissions(kernel);
        self.note(format!("内核错误：{message}"));
    }

    /// 内核事件 → 本会话的界面内容（ACP `session/update` 的 UI 投影）。
    /// 返回值：会话刚就绪且有待发的自动提示词时，交给调用方发送。
    pub fn apply_event(
        &mut self,
        event: KernelEvent,
        kernel: Option<&Arc<dyn Kernel>>,
    ) -> Option<String> {
        match event {
            KernelEvent::Started {
                session_id,
                model,
                modes,
                current_mode,
                resumed,
                ..
            } => {
                self.acp_id = Some(session_id);
                self.ready = true;
                if std::mem::take(&mut self.resuming) {
                    self.note(if resumed {
                        "已续接之前的会话，上下文已恢复"
                    } else {
                        "agent 不支持续接：已新开会话，agent 看不到之前的上下文"
                    });
                }
                if let Some(model) = model {
                    // provider/family/name → 只留最后一段，顶栏空间有限
                    self.model_label =
                        model.rsplit('/').next().unwrap_or(&model).to_string().into();
                }
                if !modes.is_empty() {
                    self.modes = modes
                        .into_iter()
                        .map(|m| ModeEntry {
                            icon: mode_icon(&m.id),
                            desc: m.description.unwrap_or_default().into(),
                            name: m.name.into(),
                            id: m.id,
                        })
                        .collect();
                    self.permission_mode = current_mode
                        .and_then(|id| self.modes.iter().position(|m| m.id == id))
                        .unwrap_or(0);
                }
                // 会话建立前在下拉里选过模型：现在告诉内核
                if let (Some(model), Some(kernel), Some(id)) =
                    (self.pending_model.take(), kernel, self.acp_id.clone())
                {
                    match kernel.set_model(&id, &model) {
                        Ok(()) => self.model_label = model.into(),
                        Err(err) => self.note(format!("切换模型失败：{err}")),
                    }
                }
                return self.autoprompt.take();
            }
            KernelEvent::AgentMessageChunk { text } => {
                self.finish_thinking();
                // 首条提示之前到达的文本是 agent 的启动信息，折叠显示
                let before_first_prompt = !self.has_user_message();
                match self.transcript.last_mut() {
                    Some(ChatItem::Info { text: buf, .. }) if before_first_prompt => {
                        buf.push_str(&text)
                    }
                    Some(ChatItem::Agent(buf)) => buf.push_str(&text),
                    _ if before_first_prompt => self.transcript.push(ChatItem::Info {
                        text,
                        open: false,
                    }),
                    _ => self.transcript.push(ChatItem::Agent(text)),
                }
            }
            KernelEvent::AgentThoughtChunk { text } => match self.transcript.last_mut() {
                Some(ChatItem::Thinking {
                    text: buf,
                    streaming: true,
                    ..
                }) => buf.push_str(&text),
                _ => self.transcript.push(ChatItem::Thinking {
                    text,
                    streaming: true,
                    open: true,
                }),
            },
            KernelEvent::ToolCallStarted {
                id,
                title,
                status,
                kind,
            } => {
                self.finish_thinking();
                let (name, arg) = tool_label(&kind, &title);
                self.transcript.push(ChatItem::Tool(ToolCard {
                    id: Some(id),
                    kind,
                    name,
                    arg,
                    result: SharedString::default(),
                    phase: tool_phase(&status),
                    diffs: Vec::new(),
                    expanded: false,
                }));
            }
            KernelEvent::ToolCallUpdated {
                id,
                title,
                status,
                raw_output,
            } => {
                if let Some(card) = self.tool_card(&id) {
                    if let Some(title) = title {
                        let (name, arg) = tool_label(&card.kind, &title);
                        card.name = name;
                        card.arg = arg;
                    }
                    if let Some(status) = status {
                        card.phase = tool_phase(&status);
                    }
                    if let Some(raw) = raw_output {
                        card.result = summarize_output(&raw).into();
                    }
                }
            }
            KernelEvent::ToolCallDiff { id, diff } => {
                if let Some(card) = self.tool_card(&id) {
                    card.set_diff(diff);
                }
            }
            KernelEvent::PlanReplaced { entries } => {
                self.finish_thinking();
                let rows: Vec<PlanRow> = entries
                    .into_iter()
                    .map(|e| PlanRow {
                        done: e.status == "completed",
                        text: e.content.into(),
                    })
                    .collect();
                let turn_start = self
                    .transcript
                    .iter()
                    .rposition(|i| matches!(i, ChatItem::User(_)))
                    .map_or(0, |i| i + 1);
                let existing = self.transcript[turn_start..]
                    .iter()
                    .rposition(|i| matches!(i, ChatItem::Plan(_)))
                    .map(|i| i + turn_start);
                match existing {
                    Some(i) => self.transcript[i] = ChatItem::Plan(rows),
                    None => self.transcript.push(ChatItem::Plan(rows)),
                }
            }
            KernelEvent::PermissionAuto { title, chosen } => {
                self.note(format!("已自动批准：{title} · {chosen}"));
            }
            KernelEvent::PermissionRequest { id, title, options } => {
                self.finish_thinking();
                self.transcript.push(ChatItem::Permission {
                    id,
                    title: title.into(),
                    options,
                    resolved: None,
                });
            }
            KernelEvent::PromptFailed { message } => {
                self.finish_thinking();
                self.busy = false;
                self.cancel_pending_permissions(kernel);
                self.note(format!("本轮失败：{message}"));
            }
            KernelEvent::TurnEnded { stop_reason } => {
                self.finish_thinking();
                self.busy = false;
                self.cancel_pending_permissions(kernel);
                if stop_reason != "end_turn" {
                    self.note(format!("本轮结束：{stop_reason}"));
                }
            }
            // 连接级事件由 ChatApp 处理；其余（状态、fs、回显）界面不展示
            KernelEvent::Connected { .. }
            | KernelEvent::SessionFailed { .. }
            | KernelEvent::Fatal(_)
            | KernelEvent::Status(_)
            | KernelEvent::UserMessageChunk { .. }
            | KernelEvent::FsRead { .. }
            | KernelEvent::FsWrite { .. } => {}
            KernelEvent::UsageUpdated {
                input_tokens,
                output_tokens,
                reasoning_tokens,
            } => {
                self.turn_usage.input += input_tokens;
                self.turn_usage.output += output_tokens;
                self.turn_usage.reasoning += reasoning_tokens;
            }
        }
        None
    }

    /// 按 tool_call_id 找最近的工具行。
    fn tool_card(&mut self, id: &str) -> Option<&mut ToolCard> {
        self.transcript.iter_mut().rev().find_map(|item| match item {
            ChatItem::Tool(card) if card.id.as_deref() == Some(id) => Some(card),
            _ => None,
        })
    }

    pub fn apply_mock(&mut self, action: &MockAction) {
        match action {
            MockAction::BeginThinking => {
                self.transcript.push(ChatItem::Thinking {
                    text: String::new(),
                    streaming: true,
                    open: true,
                });
            }
            MockAction::ThinkChunk(chunk) => {
                if let Some(ChatItem::Thinking { text, .. }) = self.transcript.last_mut() {
                    text.push_str(chunk);
                }
            }
            MockAction::EndThinking => {
                if let Some(ChatItem::Thinking { streaming, open, .. }) =
                    self.transcript.last_mut()
                {
                    *streaming = false;
                    *open = false;
                }
            }
            MockAction::BeginTool(name, arg) => {
                self.transcript.push(ChatItem::Tool(ToolCard {
                    id: None,
                    kind: String::new(),
                    name: (*name).into(),
                    arg: (*arg).into(),
                    result: SharedString::default(),
                    phase: ToolPhase::Running,
                    diffs: Vec::new(),
                    expanded: false,
                }));
            }
            MockAction::ToolDone(result) => {
                if let Some(ChatItem::Tool(card)) = self.transcript.last_mut() {
                    card.result = (*result).into();
                    card.phase = ToolPhase::Done;
                }
            }
            MockAction::Diff(path, old, new) => {
                if let Some(ChatItem::Tool(card)) = self.transcript.last_mut() {
                    card.set_diff(DiffView::compute(*path, Some(old), new));
                }
            }
            MockAction::BeginPlan => {
                self.transcript.push(ChatItem::Plan(Vec::new()));
            }
            MockAction::PlanRowAdd(row) => {
                if let Some(ChatItem::Plan(rows)) = self.transcript.last_mut() {
                    rows.push(PlanRow {
                        text: (*row).into(),
                        done: false,
                    });
                }
            }
            MockAction::PlanRowDone(i) => {
                if let Some(ChatItem::Plan(rows)) = self.transcript.last_mut() {
                    if let Some(row) = rows.get_mut(*i) {
                        row.done = true;
                    }
                }
            }
            MockAction::BeginAgent => {
                self.transcript.push(ChatItem::Agent(String::new()));
            }
            MockAction::AgentChunk(chunk) => {
                if let Some(ChatItem::Agent(text)) = self.transcript.last_mut() {
                    text.push_str(chunk);
                }
            }
            MockAction::EndAgent => {}
            MockAction::Note(text) => {
                self.transcript.push(ChatItem::Note((*text).into()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use claudesktop_core::PermissionOptionView;

    // 注意：本文件顶层 `use gpui_kit::*` 会把 gpui 的 test 宏带进作用域，
    // 遮蔽内建 #[test]，必须用全路径（原文件其他测试同理）。
    #[core::prelude::v1::test]
    fn markdown_export_covers_item_kinds() {
        let mut s = Session::new(1, KernelChoice::Demo, PathBuf::from("C:/proj"));
        s.title = "测试会话".into();
        s.note("一条备注");
        let user = ChatItem::User("你好".into());
        let agent = ChatItem::Agent("**回复**".into());
        let card = ToolCard {
            id: None,
            kind: "read".into(),
            name: "Read".into(),
            arg: "README.md".into(),
            result: "12 行".into(),
            phase: ToolPhase::Done,
            diffs: Vec::new(),
            expanded: false,
        };
        let tool = ChatItem::Tool(card);
        let row = PlanRow { done: false, text: "第一步".into() };
        let plan = ChatItem::Plan(vec![row]);
        s.transcript.push(user);
        s.transcript.push(agent);
        s.transcript.push(tool);
        s.transcript.push(plan);
        let md = s.to_markdown();
        assert!(md.starts_with("# 测试会话"));
        assert!(md.contains("## 我"));
        assert!(md.contains("你好"));
        assert!(md.contains("**Read**"));
        assert!(md.contains("README.md"));
        assert!(md.contains("- [ ] 第一步"));
        assert!(md.contains("一条备注"));
    }

    fn meta() -> SessionMeta {
        SessionMeta {
            id: "s1".into(),
            title: "读 README".into(),
            cwd: "C:/proj".into(),
            agent: "mock".into(),
            kernel_session_id: Some("acp-1".into()),
            created_ms: 0,
            updated_ms: 0,
        }
    }

    fn ev(event: KernelEvent) -> Record {
        Record::Event { event }
    }

    fn permission(id: u64) -> Record {
        ev(KernelEvent::PermissionRequest {
            id,
            title: "Write a.txt".into(),
            options: vec![PermissionOptionView {
                id: "allow".into(),
                name: "Yes".into(),
                kind: "allow_once".into(),
            }],
        })
    }

    #[core::prelude::v1::test]
    fn replay_rebuilds_transcript() {
        let records = vec![
            Record::User { text: "读 README".into() },
            ev(KernelEvent::AgentThoughtChunk { text: "想一想".into() }),
            ev(KernelEvent::ToolCallStarted {
                id: "t1".into(),
                title: "Read README.md".into(),
                status: "in_progress".into(),
                kind: "read".into(),
            }),
            ev(KernelEvent::ToolCallUpdated {
                id: "t1".into(),
                title: None,
                status: Some("completed".into()),
                raw_output: Some("# claudesktop".into()),
            }),
            permission(1),
            Record::PermissionResolved { id: 1, label: "Yes".into() },
            ev(KernelEvent::AgentMessageChunk { text: "总结完毕".into() }),
            ev(KernelEvent::TurnEnded { stop_reason: "end_turn".into() }),
            // 上次退出时没应答的请求
            Record::User { text: "再写个文件".into() },
            permission(2),
        ];
        let s = Session::from_records(7, meta(), records);

        assert!(s.restored && !s.ready && !s.busy);
        assert_eq!(s.title.as_ref(), "读 README");
        assert_eq!(s.cwd, PathBuf::from("C:/proj"));
        let kinds: Vec<&str> = s
            .transcript
            .iter()
            .map(|i| match i {
                ChatItem::User(_) => "user",
                ChatItem::Thinking { .. } => "thinking",
                ChatItem::Tool(_) => "tool",
                ChatItem::Permission { .. } => "permission",
                ChatItem::Agent(_) => "agent",
                _ => "other",
            })
            .collect();
        assert_eq!(
            kinds,
            ["user", "thinking", "tool", "permission", "agent", "user", "permission"]
        );
        assert!(matches!(&s.transcript[1], ChatItem::Thinking { streaming: false, .. }));
        assert!(matches!(&s.transcript[2], ChatItem::Tool(c) if c.phase == ToolPhase::Done));
        let labels: Vec<_> = s
            .transcript
            .iter()
            .filter_map(|i| match i {
                ChatItem::Permission { resolved, .. } => resolved.as_ref().map(|r| r.to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(labels, ["Yes", "已取消"]);
    }

    #[core::prelude::v1::test]
    fn diffs_attach_to_tool_card_and_replace_by_path() {
        let started = ev(KernelEvent::ToolCallStarted {
            id: "e1".into(),
            title: "Edit a.txt".into(),
            status: "pending".into(),
            kind: "edit".into(),
        });
        let diff = |new: &str| {
            ev(KernelEvent::ToolCallDiff {
                id: "e1".into(),
                diff: DiffView::compute("a.txt", Some("x
"), new),
            })
        };
        let records = vec![
            Record::User { text: "改".into() },
            started,
            diff("y
"),
            diff("z
"),
            ev(KernelEvent::ToolCallDiff {
                id: "e1".into(),
                diff: DiffView::compute("b.txt", None, "b
"),
            }),
        ];
        let s = Session::from_records(1, meta(), records);
        let ChatItem::Tool(card) = &s.transcript[1] else { panic!("tool card") };
        let paths: Vec<_> = card.diffs.iter().map(|d| d.path.as_str()).collect();
        assert_eq!(paths, ["a.txt", "b.txt"]);
        assert_eq!(card.diffs[0].lines.last().unwrap().text, "z");
    }

    #[core::prelude::v1::test]
    fn list_tracks_new_and_changed_rows() {
        let mut s = Session::new(1, KernelChoice::Demo, PathBuf::from("."));
        s.transcript.push(ChatItem::User("hi".into()));
        s.transcript.push(ChatItem::Agent("a".into()));
        s.sync_list();
        assert_eq!((s.list.item_count(), s.measured.len()), (2, 2));
        let before = s.measured[1];
        // 流式追加：行数不变，指纹变了
        if let Some(ChatItem::Agent(t)) = s.transcript.last_mut() {
            t.push_str("bc");
        }
        s.transcript.push(ChatItem::Note("n".into()));
        s.sync_list();
        assert_eq!(s.list.item_count(), 3);
        assert_ne!(s.measured[1], before);
        assert_eq!(s.measured[1], s.transcript[1].fingerprint());
        // 折叠 / 展开也算高度变化
        let open = ChatItem::Thinking { text: "x".into(), streaming: false, open: true };
        let closed = ChatItem::Thinking { text: "x".into(), streaming: false, open: false };
        assert_ne!(open.fingerprint(), closed.fingerprint());
    }

    #[core::prelude::v1::test]
    fn detach_kernel_interrupts_turn_and_marks_for_resume() {
        let mut s = Session::new(1, KernelChoice::Acp, PathBuf::from("."));
        s.apply_event(
            KernelEvent::Started {
                token: 1,
                agent_name: "a".into(),
                session_id: "k1".into(),
                model: Some("m".into()),
                modes: Vec::new(),
                current_mode: None,
                resumed: false,
            },
            None,
        );
        assert!(s.ready);
        // 空会话：脱离后等新内核重新创建
        s.detach_kernel(None);
        assert!(!s.ready && !s.restored && s.acp_id.is_none() && !s.requested);

        s.acp_id = Some("k2".into());
        s.ready = true;
        s.transcript.push(ChatItem::User("hi".into()));
        s.apply_event(
            KernelEvent::ToolCallStarted {
                id: "t".into(),
                title: "Read a".into(),
                status: "in_progress".into(),
                kind: "read".into(),
            },
            None,
        );
        s.busy = true;
        s.detach_kernel(None);
        // 有对话：变成历史会话，进行中的工具收尾
        assert!(s.restored && !s.busy && !s.ready);
        assert!(s.transcript.iter().any(|i| matches!(i, ChatItem::Tool(c) if c.phase == ToolPhase::Failed)));
        assert!(matches!(s.transcript.last(), Some(ChatItem::Note(_))));
    }

    #[core::prelude::v1::test]
    fn activity_follows_turn_and_usage_accumulates() {
        let mut s = Session::new(1, KernelChoice::Acp, PathBuf::from("."));
        // 上一轮遗留的执行中工具不算进本轮
        s.transcript.push(ChatItem::User("上一轮".into()));
        s.apply_event(
            KernelEvent::ToolCallStarted {
                id: "old".into(),
                title: "Read a".into(),
                status: "in_progress".into(),
                kind: "read".into(),
            },
            None,
        );
        s.transcript.push(ChatItem::User("这一轮".into()));
        s.begin_turn();
        assert!(s.busy && s.turn_started.is_some());
        assert_eq!(s.activity(), Activity::Waiting);

        s.apply_event(KernelEvent::AgentThoughtChunk { text: "想".into() }, None);
        assert_eq!(s.activity(), Activity::Thinking);

        s.apply_event(
            KernelEvent::ToolCallStarted {
                id: "t".into(),
                title: "Bash cargo test".into(),
                status: "in_progress".into(),
                kind: "execute".into(),
            },
            None,
        );
        assert!(matches!(s.activity(), Activity::Tool { .. }));

        s.apply_event(
            KernelEvent::ToolCallUpdated {
                id: "t".into(),
                title: None,
                status: Some("completed".into()),
                raw_output: None,
            },
            None,
        );
        s.apply_event(KernelEvent::AgentMessageChunk { text: "好了".into() }, None);
        assert_eq!(s.activity(), Activity::Replying);

        s.apply_event(permission_event(7), None);
        assert_eq!(s.activity(), Activity::WaitingPermission);

        for _ in 0..2 {
            s.apply_event(
                KernelEvent::UsageUpdated {
                    input_tokens: 100,
                    output_tokens: 30,
                    reasoning_tokens: 10,
                },
                None,
            );
        }
        assert_eq!(
            s.turn_usage,
            TurnUsage { input: 200, output: 60, reasoning: 20 }
        );
    }

    fn permission_event(id: u64) -> KernelEvent {
        match permission(id) {
            Record::Event { event } => event,
            _ => unreachable!(),
        }
    }
}

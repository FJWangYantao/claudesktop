//! 历史记录重放与内核事件投影：落盘记录 / `KernelEvent` 怎样变成界面上的对话内容。

use super::*;

impl Session {
    /// 从持久化记录重放出一个历史会话：事件走与实时完全相同的映射。
    pub fn from_records(
        token: u64,
        meta: SessionMeta,
        records: Vec<Record>,
        settings: &crate::settings::Settings,
    ) -> Self {
        let mut s = Self::of_kind(
            token,
            KernelChoice::of_agent(&meta.agent, &settings.acp),
            meta.cwd.clone(),
            meta.kind,
        );
        s.ready = false;
        // 不套档案的 `last_model`：历史会话用的是它自己当时的模型，不该被别的会话最近选的改掉
        // 是不是「任务」跟着元信息走：建的时候没选过文件夹的就是任务
        s.task = meta.task;
        s.kind = meta.kind;
        // 聊天的联网开关跟着元信息走，并且总是交给新的内核会话（内核默认按设置来，
        // 不交的话界面上显示的和模型实际拿到的工具可能对不上）；老元信息没记就当开着
        if s.kind == SessionKind::Chat {
            s.web = meta.web.unwrap_or(true);
            s.pending_web = Some(s.web);
        }
        // 跑在 git worktree 里的会话：侧栏仍归到主项目，分支 chip 用它
        s.worktree = meta.worktree.clone();
        s.title = meta.title.clone().into();
        s.restored = true;
        s.replaying = true;
        // 最新一轮每个文件最后一次写盘时处于「已撤回」的块（agent 写的清空），恢复改动卡片的撤回状态
        let mut reverted_now: HashMap<String, bool> = HashMap::new();
        // 回退过的轮次不显示（日志只追加，读的时候折算）
        for record in claudesktop_core::into_effective(records) {
            match record {
                Record::User { text, images, .. } => {
                    s.review_pending = false;
                    reverted_now.clear();
                    s.transcript.push(ChatItem::User {
                        text: text.into(),
                        images: images.iter().map(PathBuf::from).collect(),
                        files: Default::default(),
                    })
                }
                Record::Event { event } => {
                    s.apply_event(event, None);
                }
                Record::Turn {
                    model,
                    input_tokens,
                    output_tokens,
                    elapsed_ms,
                    context_tokens,
                    ..
                } => {
                    // 模型名从日志恢复：内核连上（Started）之前顶栏就能显示真名，
                    // 上下文用量也才能按它查到窗口大小
                    if let Some(model) = model {
                        s.model_label = model.into();
                    }
                    if let Some(user) = s.last_user() {
                        // 日志里的用量是准数（写日志前端点报过）
                        let usage = TurnUsage {
                            input: input_tokens,
                            output: output_tokens,
                            reasoning: 0,
                            reported: true,
                        };
                        s.turn_summaries.insert(user, TurnSummary { elapsed_ms, usage });
                    }
                    if let Some(n) = context_tokens.filter(|n| *n > 0) {
                        s.context_tokens = n;
                    }
                }
                Record::Thought { elapsed_ms: ms } => {
                    let last = s.transcript.iter_mut().rev().find_map(|i| match i {
                        ChatItem::Thinking { elapsed_ms, .. } => Some(elapsed_ms),
                        _ => None,
                    });
                    if let Some(slot) = last {
                        *slot = Some(ms);
                    }
                }
                Record::PermissionResolved { id, label } => {
                    let at = s.transcript.iter().rposition(|i| {
                        matches!(i, ChatItem::Permission { id: pid, resolved: None, .. } if *pid == id)
                    });
                    if let Some(at) = at {
                        if let ChatItem::Permission { resolved, .. } = &mut s.transcript[at] {
                            *resolved = Some(label.into());
                        }
                        s.merge_permission(at);
                    }
                }
                Record::QuestionAnswered { id, answer } => {
                    let at = s.transcript.iter().rposition(
                        |i| matches!(i, ChatItem::Question { id: qid, answer: None, .. } if *qid == id),
                    );
                    if let Some(at) = at
                        && let ChatItem::Question { picks, answer: slot, questions, .. } =
                            &mut s.transcript[at]
                    {
                        *picks = match &answer {
                            // 第 i 项是第 i 题选的选项下标：按那题的选项数校验
                            claudesktop_core::QuestionAnswer::Choices { choices } => questions
                                .iter()
                                .enumerate()
                                .map(|(i, q)| {
                                    choices.get(i).copied().filter(|n| *n < q.options.len())
                                })
                                .collect(),
                            _ => vec![None; questions.len()],
                        };
                        *slot = Some(answer);
                    }
                }
                Record::Rewind { conversation, files, prompt, .. } => {
                    s.note(rewind_note(conversation, files, &prompt))
                }
                // 检查点只在回退 / 审查时用，界面上不显示
                Record::FileChanged { path, reverted, .. } => {
                    s.review_pending |= reverted.is_some();
                    reverted_now.insert(path, reverted.is_some_and(|r| !r.is_empty()));
                }
                // 工具加载状态是内核给模型的工具定义，界面上不显示（续接时内核从日志还回去）
                Record::ToolLoadout { .. } => {}
            }
        }
        // 撤回了多少要对着 diff 才数得清，这里只知道「撤回过」：先按撤回了一部分显示，
        // 点开审查页或再点一次「撤回」时按实际情况更新（`ChatApp::sync_undo`）
        if reverted_now.values().any(|r| *r)
            && let Some(turn) = s.latest_turn()
        {
            s.undo = Some(TurnUndo { turn, all: false, exact: false });
        }
        // 上次退出时还在进行的东西一律收尾
        s.finish_thinking();
        s.replaying = false;
        let mut unfinished = s.busy;
        for item in s.transcript.iter_mut() {
            match item {
                ChatItem::Permission { resolved: r @ None, .. } => {
                    *r = Some("已取消".into());
                    unfinished = true;
                }
                // 上次退出时还挂着的提问：内核那边早就没了这个请求，按取消收尾
                ChatItem::Question { answer: a @ None, .. } => {
                    *a = Some(claudesktop_core::QuestionAnswer::Cancelled);
                    unfinished = true;
                }
                ChatItem::Tool(card) if card.phase == ToolPhase::Running => {
                    card.phase = ToolPhase::Failed;
                    if card.result.is_empty() {
                        card.result = "已中断".into();
                    }
                    unfinished = true;
                }
                _ => {}
            }
        }
        s.merge_permissions();
        // 关应用时这一轮还在跑：补一张卡片说清楚，并给「继续」（不然只剩几行「已中断」，不知道下一步）
        if unfinished {
            s.transcript.push(ChatItem::Failed {
                message: SharedString::default(),
                kind: FailureKind::Unfinished,
            });
        }
        s.busy = false;
        // 重放出来的会话不带着「正在流式」的计数
        s.turn_streamed.clear();
        s.sync_builtin_modes(settings.auto_mode_default());
        if !s.is_chat() {
            s.pending_mode = meta.mode.clone();
            if let Some(mode) = &meta.mode
                && let Some(i) = s.modes.iter().position(|m| &m.id == mode)
            {
                s.permission_mode = i;
            }
        }
        // 上次退出时还没发出去的排队消息：接着排（内核就绪后由 `dispatch_queued` 发出）
        s.queued = meta.queued.iter().cloned().map(QueuedPrompt::from_meta).collect();
        s.meta = Some(meta);
        s
    }

    /// 应答过的权限请求并进它的工具行（工具行末尾出「已允许」之类的短标签），这一行不再画。
    /// 找不到工具行（旧日志没有工具 ID）、认不出选的是什么或应答失败时，照旧单独一行。
    pub(crate) fn merge_permission(&mut self, idx: usize) {
        let Some(ChatItem::Permission {
            tool: Some(tool),
            options,
            resolved: Some(label),
            merged: false,
            ..
        }) = self.transcript.get(idx)
        else {
            return;
        };
        // 按块同意的标签带着「（只写 N 块）」：匹配选项时去掉它
        let base = partial_base(label);
        let grant = if base.as_ref() == "已取消" {
            Grant::from_choice(None, label.clone())
        } else {
            match options.iter().find(|o| o.name == base.as_ref()) {
                Some(option) => Grant::from_choice(Some(&option.kind), label.clone()),
                None => return,
            }
        };
        let tool = tool.clone();
        let Some(card) = self.tool_card(&tool) else {
            return;
        };
        card.grant = Some(grant);
        if let Some(ChatItem::Permission { merged, .. }) = self.transcript.get_mut(idx) {
            *merged = true;
        }
    }

    /// 把所有应答过、还没并进工具行的权限请求并过去（重放收尾、整轮取消时用）。
    pub(crate) fn merge_permissions(&mut self) {
        for idx in 0..self.transcript.len() {
            if matches!(
                self.transcript[idx],
                ChatItem::Permission { resolved: Some(_), merged: false, .. }
            ) {
                self.merge_permission(idx);
            }
        }
    }

    /// 本轮结束 / 停止 / 内核出错时，仍在等待的请求一律按「取消」收尾。
    pub fn cancel_pending_permissions(&mut self, kernel: Option<&Arc<dyn Kernel>>) {
        let Some(kernel) = kernel else {
            return;
        };
        for item in self.transcript.iter_mut() {
            if let ChatItem::Permission { id, resolved, .. } = item
                && resolved.is_none()
            {
                let _ = kernel.resolve_permission(*id, None);
                *resolved = Some("已取消".into());
            }
        }
        self.merge_permissions();
    }

    /// 本轮结束 / 停止 / 内核出错时，还没回答的提问一律按「取消」收尾
    /// （内核那边的阻塞已经解开了，再点也送不进去）。
    pub fn cancel_pending_questions(&mut self) {
        for item in self.transcript.iter_mut() {
            if let ChatItem::Question { answer: a @ None, .. } = item {
                *a = Some(claudesktop_core::QuestionAnswer::Cancelled);
            }
        }
    }

    /// 回退对话：对话截到第 `idx` 行之前（那条用户消息起都不要了），挂在这些行上的状态一并清掉。
    pub fn truncate_to(&mut self, idx: usize) {
        self.stream_pending.clear();
        if idx < self.transcript.len() {
            self.transcript.truncate(idx);
            self.body_cache.truncate(idx);
            self.diff_revision += 1;
            self.diff_epoch += 1;
        }
        self.turn_summaries.retain(|&i, _| i < idx);
        self.open_groups.retain(|&i| i < idx);
        self.folded_live_groups.retain(|&i| i < idx);
        if self.animated_group.is_some_and(|i| i >= idx) {
            self.animated_group = None;
        }
        self.plan_folded = false;
        // 上下文变短了，用量等下一轮第一步请求完内核再报
        self.context_tokens = 0;
        self.cache_hit = None;
    }

    /// 这一轮停了 / 断了，还在转圈的工具行收成「已中断」（内核不会再报它们的结果）。
    pub fn interrupt_running_tools(&mut self) {
        let runs = self.subagents.values_mut().flat_map(|r| r.items.iter_mut());
        for item in self.transcript.iter_mut().chain(runs) {
            if let ChatItem::Tool(card) = item
                && card.phase == ToolPhase::Running
            {
                card.phase = ToolPhase::Failed;
                if card.result.is_empty() {
                    card.result = "已中断".into();
                }
            }
        }
    }

    /// 排着的「压缩上下文」（[`Self::compact_queued`]）不做了：撤掉并在对话里说一句（`why` 是开头的原因）。
    pub fn drop_queued_compaction(&mut self, why: &str) {
        if std::mem::take(&mut self.compact_queued) {
            self.note(format!("{why}：排着的「压缩上下文」也撤掉了，需要的话再发一次"));
        }
    }

    /// 内核要换了：收尾进行中的一轮，断开与旧内核会话的关联。
    /// 有对话的会话变成「历史会话」，下次发消息时在新内核上续接。
    pub fn detach_kernel(&mut self, kernel: Option<&Arc<dyn Kernel>>) {
        self.flush_stream();
        self.cancel_pending_permissions(kernel);
        // 排着的压缩是给旧内核会话的：撤掉，免得新会话接上时和第一轮撞在一起
        self.drop_queued_compaction("内核已切换");
        self.cancel_token += 1;
        if self.busy {
            self.busy = false;
            self.finish_thinking();
            self.interrupt_running_tools();
            self.note("内核已切换，这一轮已中断");
        }
        self.stopping = false;
        self.acp_id = None;
        self.requested = false;
        self.resuming = false;
        self.ready = false;
        self.autoprompt = None;
        self.restored = self.has_user_message();
        // 当前模式随新内核会话重新交回去：用户选的（或新对话默认的）模式不该因为重连丢掉
        self.pending_mode = self.modes.get(self.permission_mode).map(|m| m.id.clone());
        // 会话级思考档位同理：新内核会话里是「默认」，选过的要重新交回去
        if self.thinking.is_some() {
            self.pending_thinking = Some(self.thinking);
        }
        // 联网开关也是会话级的：新内核会话默认按设置来，用户在输入框里改过的要重新交回去
        if self.kind == SessionKind::Chat {
            self.pending_web = Some(self.web);
        }
    }

    /// 内核致命错误：这个会话不再可用。
    pub fn on_fatal(&mut self, message: &str, kernel: Option<&Arc<dyn Kernel>>) {
        self.flush_stream();
        self.busy = false;
        self.stopping = false;
        self.ready = false;
        self.cancel_pending_permissions(kernel);
        self.note(format!("内核错误：{message}"));
        self.drop_queued_compaction("内核出错");
    }

    /// 内核事件 → 本会话的界面内容（ACP `session/update` 的 UI 投影）。
    /// 返回值：会话刚就绪且有待发的自动提示词时，交给调用方发送。
    pub fn apply_event(
        &mut self,
        event: KernelEvent,
        kernel: Option<&Arc<dyn Kernel>>,
    ) -> Option<claudesktop_core::UserPrompt> {
        let rows_before = self.transcript.len();
        let turn_ended = matches!(event, KernelEvent::TurnEnded { .. });
        // 后台起标题不是「来了新内容」：不算进 `content_seq`，否则用户停在吸附带里会被
        // 平白滑回底部、未读红点也会误亮
        if Record::worth_keeping(&event) && !matches!(event, KernelEvent::TitleGenerated { .. }) {
            self.content_seq += 1;
        }
        // 实质事件（工具、轮末、用量……）动手前先让攒着的流式文本落进正文，状态始终一致
        if !matches!(
            event,
            KernelEvent::AgentMessageChunk { .. } | KernelEvent::AgentThoughtChunk { .. }
        ) {
            self.flush_stream();
        }
        // 重试的倒计时只撑到下一个实质事件（重发后出字、出错或结束）
        if !matches!(
            event,
            KernelEvent::Retrying { .. }
                | KernelEvent::Status(_)
                | KernelEvent::Notice(_)
                | KernelEvent::UsageUpdated { .. }
        ) {
            self.retry = None;
        }
        match event {
            KernelEvent::Retrying { status, attempt, delay_ms, .. } => {
                self.retry = Some(Retry {
                    status,
                    attempt,
                    at: Instant::now() + std::time::Duration::from_millis(delay_ms),
                });
            }
            KernelEvent::Started { session_id, model, modes, current_mode, resumed, .. } => {
                // 换了新的内核会话（如回退后重开）：旧的不再用，交还内核收尾
                if let (Some(old), Some(kernel)) = (&self.acp_id, kernel)
                    && *old != session_id
                {
                    kernel.close_session(old);
                }
                self.acp_id = Some(session_id);
                self.ready = true;
                // 续接成功是理所当然的，不往对话里写；接不上才要说（agent 看不到之前的内容了）
                if std::mem::take(&mut self.resuming) && !resumed {
                    self.note("agent 不支持续接：已新开会话，agent 看不到之前的上下文");
                }
                if let Some(model) = model {
                    // provider/family/name → 只留最后一段，顶栏空间有限
                    self.model_label =
                        model.rsplit('/').next().unwrap_or(&model).to_string().into();
                }
                if !modes.is_empty() {
                    self.replace_modes(modes, current_mode.as_deref());
                }
                // 新建对话的默认模式 / 会话建立前选的模式：内核会话建立了才告诉它。
                // 内核没报这个模式（如 ACP agent）就作罢，保留上面按 `current_mode` 定下的那个
                if let Some(mode) = self.pending_mode.take()
                    && let Some(i) = self.modes.iter().position(|m| m.id == mode)
                {
                    let result = match (kernel, self.acp_id.as_deref()) {
                        (Some(kernel), Some(id)) => kernel.set_mode(id, &mode),
                        _ => Ok(()),
                    };
                    match result {
                        Ok(()) => self.permission_mode = i,
                        Err(err) => self.note(format!("切换权限模式失败：{err}")),
                    }
                }
                self.remember_mode();
                // 会话建立前在下拉里选过模型：现在告诉内核
                if let (Some(model), Some(kernel), Some(id)) =
                    (self.pending_model.take(), kernel, self.acp_id.clone())
                {
                    match kernel.set_model(&id, &model) {
                        Ok(()) => self.model_label = model.into(),
                        Err(err) => self.note(format!("切换模型失败：{err}")),
                    }
                }
                // agent 档案记着上次用的模型：新会话接上来就切过去（agent 没报模型时才补）。
                // 切不过去（agent 那边已经没有这个模型）就算了，不往每个新对话里写一条失败
                else if let Some(model) = self.last_model.take()
                    && self.model_label == UNKNOWN_MODEL
                    && let Some(kernel) = kernel
                    && let Some(id) = self.acp_id.clone()
                    && kernel.set_model(&id, &model).is_ok()
                {
                    self.model_label = model.into();
                }
                return self.autoprompt.take();
            }
            KernelEvent::AgentMessageChunk { text } => {
                // 思考可能还攒在缓冲里：只把思考并进再收尾（用时挂在完整的思考上），
                // 正文的缓冲不动——flush_stream 不分种类，这里不能调它
                if self.stream_pending_thought {
                    self.flush_stream();
                }
                self.finish_thinking();
                self.turn_streamed.push(&text);
                self.buffer_chunk(text, false);
            }
            KernelEvent::AgentThoughtChunk { text } => {
                self.turn_streamed.push(&text);
                self.buffer_chunk(text, true);
            }
            KernelEvent::SubagentStarted { task, agent, prompt } => {
                *self.subagent(&task) =
                    SubagentRun { agent: agent.into(), prompt: prompt.into(), items: Vec::new() };
            }
            KernelEvent::SubagentChunk { task, text, thought } => {
                self.turn_streamed.push(&text);
                self.subagent(&task).push_text(&text, thought);
            }
            KernelEvent::ToolCallStarted { id, title, status, kind, parent } => {
                self.finish_thinking();
                let (name, arg) = tool_label(&kind, &title);
                let card = ToolCard {
                    id: Some(id),
                    kind,
                    name,
                    arg,
                    result: SharedString::default(),
                    output: SharedString::default(),
                    content: SharedString::default(),
                    phase: tool_phase(&status),
                    diffs: Vec::new(),
                    expanded: false,
                    task: false,
                    grant: None,
                    images: Vec::new(),
                };
                // 子 agent 的调用归到它那个子任务里，不进对话记录
                match parent {
                    Some(task) => self.subagent(&task).push_tool(card),
                    None => self.transcript.push(ChatItem::Tool(card)),
                }
            }
            KernelEvent::ToolCallUpdated { id, title, status, raw_output, output } => {
                let mut changed_diffs = false;
                if let Some(card) = self.tool_card(&id) {
                    if let Some(title) = title {
                        let (name, arg) = tool_label(&card.kind, &title);
                        card.name = name;
                        card.arg = arg;
                    }
                    if let Some(status) = status {
                        let phase = tool_phase(&status);
                        changed_diffs = !card.diffs.is_empty()
                            && (card.phase == ToolPhase::Done) != (phase == ToolPhase::Done);
                        card.phase = phase;
                    }
                    if let Some(raw) = raw_output {
                        card.result = summarize_output(&raw).into();
                    }
                    if let Some(output) = output {
                        card.output = output.into();
                    }
                }
                if changed_diffs {
                    self.diff_revision += 1;
                }
            }
            KernelEvent::ToolCallProgress { id, output } => {
                if let Some(card) = self.tool_card(&id)
                    && card.phase == ToolPhase::Running
                {
                    card.output = output.into();
                }
            }
            KernelEvent::ToolCallDiff { id, diff } => {
                if let Some(card) = self.tool_card(&id) {
                    card.set_diff(diff);
                    if card.phase == ToolPhase::Done {
                        self.diff_revision += 1;
                    }
                }
            }
            KernelEvent::ToolCallContent { id, output } => {
                if let Some(card) = self.tool_card(&id) {
                    card.content = output.into();
                }
            }
            // 工具带回来的图片（`read_file` 读到的图、MCP 的多模态结果）：工具行里画缩略图。
            // 图片太大不进日志，续接后这一行没有缩略图（图片文件还在磁盘上）
            KernelEvent::ToolCallImages { id, images } => {
                if let Some(card) = self.tool_card(&id) {
                    card.images = images
                        .iter()
                        .filter_map(thumb_from_prompt_image)
                        .collect();
                }
            }
            KernelEvent::PlanReplaced { entries } => {
                self.finish_thinking();
                let rows: Vec<PlanRow> = entries
                    .into_iter()
                    .map(|e| PlanRow {
                        status: PlanStatus::parse(&e.status),
                        text: e.content.into(),
                    })
                    .collect();
                let turn_start = self.turn_start();
                let existing = self.transcript[turn_start..]
                    .iter()
                    .rposition(|i| matches!(i, ChatItem::Plan(_)))
                    .map(|i| i + turn_start);
                match existing {
                    Some(i) => self.transcript[i] = ChatItem::Plan(rows),
                    None => self.transcript.push(ChatItem::Plan(rows)),
                }
            }
            KernelEvent::PermissionAuto { title, chosen, tool } => {
                // 知道是哪个工具调用的：标在那一行上，不另起一行
                let card = tool.and_then(|tool| {
                    self.transcript.iter_mut().rev().find_map(|i| match i {
                        ChatItem::Tool(card) if card.id.as_deref() == Some(tool.as_str()) => {
                            Some(card)
                        }
                        _ => None,
                    })
                });
                match card {
                    Some(card) => {
                        card.grant = Some(Grant { kind: GrantKind::Auto, label: chosen.into() })
                    }
                    None => self.transcript.push(ChatItem::AutoApproved {
                        title: title.into(),
                        chosen: chosen.into(),
                    }),
                }
            }
            KernelEvent::PermissionRequest { id, title, options, tool, hunks } => {
                self.finish_thinking();
                self.transcript.push(ChatItem::Permission {
                    id,
                    title: title.into(),
                    options,
                    resolved: None,
                    tool,
                    merged: false,
                    hunks,
                    // 默认全部放行：「允许一次」就是不勾也行
                    keep: None,
                });
            }
            // 事件里的 `tool`（触发它的调用 ID）界面不用：提问不占工具行
            KernelEvent::UserQuestion { id, questions, .. } => {
                self.finish_thinking();
                self.transcript.push(ChatItem::Question {
                    id,
                    picks: vec![None; questions.len()],
                    multi_picks: vec![Vec::new(); questions.len()],
                    text_answers: vec![None; questions.len()],
                    answered: vec![false; questions.len()],
                    questions,
                    answer: None,
                });
            }
            KernelEvent::Compacting => {
                self.finish_thinking();
                self.transcript
                    .push(ChatItem::Compaction { id: None, state: CompactState::Running, open: false });
            }
            KernelEvent::Compacted { summary, context_tokens } => {
                self.settle_compaction(CompactState::Done(summary.into()));
                if context_tokens > 0 {
                    self.context_tokens = context_tokens;
                }
            }
            KernelEvent::CompactFailed { message } => {
                self.settle_compaction(CompactState::Failed(message.into()));
            }
            KernelEvent::PromptFailed { message } => {
                self.settle_compaction(CompactState::Failed("已中断".into()));
                self.finish_thinking();
                self.busy = false;
                self.stopping = false;
                self.cancel_pending_permissions(kernel);
                self.cancel_pending_questions();
                self.interrupt_running_tools();
                self.fail(message);
            }
            KernelEvent::TurnEnded { stop_reason } => {
                self.settle_compaction(CompactState::Failed(
                    if stop_reason == "cancelled" { "已取消" } else { "已中断" }.into(),
                ));
                self.finish_thinking();
                self.busy = false;
                // 用户停下的：「已请求停止」已经说过了，不再补一行「本轮结束：cancelled」
                let stopped = std::mem::take(&mut self.stopping) && stop_reason == "cancelled";
                self.cancel_pending_permissions(kernel);
                self.cancel_pending_questions();
                self.interrupt_running_tools();
                if stop_reason != "end_turn" && !stopped {
                    self.note(format!("本轮结束：{stop_reason}"));
                }
            }
            // 运行中插话：内核已把它追加进上下文，界面上落成一行真实用户消息
            KernelEvent::SteerInjected { text } => {
                self.transcript.push(ChatItem::User {
                    text: text.into(),
                    images: Vec::new(),
                    files: Default::default(),
                });
            }
            // 插件钩子出错等：会话里提示一行
            KernelEvent::Notice(text) => self.note(text),
            // 连接级事件由 ChatApp 处理；其余（状态、fs、回显）界面不展示
            KernelEvent::Connected { .. }
            | KernelEvent::SessionFailed { .. }
            | KernelEvent::Fatal(_)
            | KernelEvent::Status(_)
            | KernelEvent::UserMessageChunk { .. }
            | KernelEvent::FsRead { .. }
            | KernelEvent::FsWrite { .. }
            | KernelEvent::FileChanged { .. }
            // 工具加载只影响内核发给模型的工具定义（内核从日志的 `Record::ToolLoadout` 拿回来）
            | KernelEvent::ToolsLoaded { .. }
            | KernelEvent::McpStatus { .. }
            | KernelEvent::BackgroundTasks { .. }
            | KernelEvent::MemoryChanged { .. }
            | KernelEvent::MemorySuggested { .. } => {}
            KernelEvent::AuthRequired { methods, message, .. } => {
                self.auth = Some(AuthRequestView { methods, message: message.into(), busy: false });
            }
            KernelEvent::AuthCompleted { success, message, .. } => {
                if success {
                    self.auth = None;
                } else if let Some(auth) = &mut self.auth {
                    auth.busy = false;
                    auth.message = message.into();
                }
            }
            KernelEvent::ConfigOptionsChanged { options } => {
                let current = options
                    .iter()
                    .find(|option| option.category == "model" || option.id == "model")
                    .map(|option| option.current_value.clone());
                self.config_options = options;
                if let Some(model) = current {
                    self.model_label = model.rsplit('/').next().unwrap_or(&model).to_string().into();
                }
            }
            KernelEvent::CurrentModeChanged { mode_id } => {
                if let Some(index) = self.modes.iter().position(|mode| mode.id == mode_id) {
                    self.permission_mode = index;
                    self.remember_mode();
                }
            }
            KernelEvent::AvailableCommandsChanged { commands } => {
                self.available_commands = commands;
            }
            KernelEvent::ContextUsageUpdated { usage } => {
                let ContextUsageView { used, size, cost } = usage;
                self.context_tokens = used;
                self.context_size = (size > 0).then_some(size);
                self.context_cost = cost;
            }
            KernelEvent::CompactionUpdated { compaction } => {
                self.apply_acp_compaction(compaction);
            }
            KernelEvent::TitleGenerated { title } => {
                // 模型起的标题替换截断版（用户手动改过的不覆盖）
                if !self.renamed {
                    self.title = title.clone().into();
                    self.title_generated = true;
                }
            }
            KernelEvent::UsageUpdated {
                input_tokens,
                output_tokens,
                reasoning_tokens,
                context_tokens,
                cache_read_tokens,
            } => {
                // 端点不报缓存时是 0，和「一点没命中」分不开，所以 0 也当不知道。
                // 只看主会话自己的请求（带上下文大小的那种）：压缩、子会话的请求是另一份上下文，
                // 它们的命中率不该顶掉主会话的
                if context_tokens > 0 {
                    self.cache_hit =
                        crate::usage::cache_hit_percent(cache_read_tokens, input_tokens);
                }
                // 到这里的输出已经被端点数过，估算的那部分清掉（之后新收到的再重新估）
                self.turn_streamed.clear();
                self.turn_usage.reported = true;
                self.turn_usage.input += input_tokens;
                self.turn_usage.output += output_tokens;
                self.turn_usage.reasoning += reasoning_tokens;
                if context_tokens > 0 {
                    self.context_tokens = context_tokens;
                }
            }
        }
        // 真的多了行（新步骤涌入）或这一轮结束，上一次手动点开的组才不再播弹簧；
        // 流式 chunk 只是把正文补长，不能把展开 / 收起的弹簧动画中途掐断
        if self.transcript.len() != rows_before || turn_ended {
            self.animated_group = None;
        }
        None
    }

    /// 第 `task` 个 task 调用的子任务（没有就建一个），并把那一行标成子任务卡片。
    fn subagent(&mut self, task: &str) -> &mut SubagentRun {
        if let Some(card) = self.tool_card(task) {
            card.task = true;
        }
        self.subagents.entry(task.to_string()).or_default()
    }

    /// 按 tool_call_id 找最近的工具行；子 agent 的调用（`task 调用 ID/调用 ID`）在它那个子任务里找。
    fn tool_card(&mut self, id: &str) -> Option<&mut ToolCard> {
        fn find<'a>(item: &'a mut ChatItem, id: &str) -> Option<&'a mut ToolCard> {
            match item {
                ChatItem::Tool(card) if card.id.as_deref() == Some(id) => Some(card),
                _ => None,
            }
        }
        if let Some((task, _)) = id.split_once('/')
            && let Some(run) = self.subagents.get_mut(task)
        {
            return run.items.iter_mut().rev().find_map(|i| find(i, id));
        }
        self.transcript.iter_mut().rev().find_map(|i| find(i, id))
    }

    pub fn apply_mock(&mut self, action: &MockAction) {
        match action {
            MockAction::BeginThinking => {
                self.transcript.push(ChatItem::thinking(String::new(), Some(Instant::now())));
            }
            MockAction::ThinkChunk(chunk) => {
                if let Some(ChatItem::Thinking { text, .. }) = self.transcript.last_mut() {
                    text.push_str(chunk);
                }
            }
            MockAction::EndThinking => self.finish_thinking(),
            MockAction::BeginTool(name, arg) => {
                self.transcript.push(ChatItem::Tool(ToolCard {
                    id: None,
                    kind: String::new(),
                    name: (*name).into(),
                    arg: (*arg).into(),
                    result: SharedString::default(),
                    output: SharedString::default(),
                    content: SharedString::default(),
                    phase: ToolPhase::Running,
                    diffs: Vec::new(),
                    expanded: false,
                    task: false,
                    grant: None,
                    images: Vec::new(),
                }));
            }
            MockAction::ToolDone(result) => {
                if let Some(ChatItem::Tool(card)) = self.transcript.last_mut() {
                    if card.phase != ToolPhase::Done && !card.diffs.is_empty() {
                        self.diff_revision += 1;
                    }
                    card.result = (*result).into();
                    card.phase = ToolPhase::Done;
                }
            }
            MockAction::Diff(path, old, new) => {
                if let Some(ChatItem::Tool(card)) = self.transcript.last_mut() {
                    card.set_diff(DiffView::compute(*path, Some(old), new));
                    if card.phase == ToolPhase::Done {
                        self.diff_revision += 1;
                    }
                }
            }
            MockAction::BeginPlan => {
                self.transcript.push(ChatItem::Plan(Vec::new()));
            }
            MockAction::PlanRowAdd(row) => {
                if let Some(ChatItem::Plan(rows)) = self.transcript.last_mut() {
                    // 第一项一加进来就算在做
                    let status =
                        if rows.is_empty() { PlanStatus::InProgress } else { PlanStatus::Pending };
                    rows.push(PlanRow { text: (*row).into(), status });
                }
            }
            MockAction::PlanRowDone(i) => {
                if let Some(ChatItem::Plan(rows)) = self.transcript.last_mut() {
                    if let Some(row) = rows.get_mut(*i) {
                        row.status = PlanStatus::Done;
                    }
                    // 下一项接着做
                    if let Some(next) = rows.get_mut(*i + 1)
                        && next.status == PlanStatus::Pending
                    {
                        next.status = PlanStatus::InProgress;
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

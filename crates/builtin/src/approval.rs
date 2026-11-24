//! 审批判定：规则命中、自动模式分类器、拒绝计数、向用户提问。

use super::*;

impl Inner {
    /// 有没有一条已保存的「总是拒绝」规则命中这个动作（聊天会话联网免问之前先看它）。
    pub(super) fn denied_rule_hits(&self, cwd: &Path, action: &Action) -> bool {
        let Some(rule) = action.rule(cwd) else {
            return false;
        };
        let command = match action {
            Action::Shell { command, .. } => Some(command.as_str()),
            _ => None,
        };
        self.rules.lock().unwrap().denies(cwd, &rule, command)
    }

    /// 读工具直接放行；其余按模式 → 已保存规则 → 询问用户。
    /// 抓网页是只读的，Plan 模式下也能用（照样要问）。
    pub(super) fn authorize(
        &self,
        session: &str,
        cwd: &Path,
        action: &Action,
        title: &str,
        tool: &str,
        stopped: &dyn Fn() -> bool,
    ) -> Verdict {
        let access = action.access();
        if access == Access::Read {
            return Verdict::Allow;
        }
        // 聊天会话：联网开着就是用户同意了（关着时这两个工具根本不在定义里）。
        // 已保存的「总是拒绝」规则照样生效，所以放在规则判断之前
        if access == Access::Fetch
            && self.chat_web_of(session) == Some(true)
            && !self.denied_rule_hits(cwd, action)
        {
            return Verdict::Allow;
        }
        let mode = self.mode(session);
        match mode.as_str() {
            MODE_PLAN if !matches!(access, Access::Fetch | Access::Mcp { read_only: true }) => {
                return Verdict::Deny(
                    "Plan mode is active: writing files and running commands are not allowed. \
                     Investigate with read-only tools and present a plan instead."
                        .into(),
                );
            }
            MODE_BYPASS => return Verdict::Allow,
            MODE_ACCEPT_EDITS if access == Access::Edit && action.inside(cwd) => {
                return Verdict::Allow;
            }
            // 自动：硬熔断 → 明显只读的命令 → 工作目录内的编辑（都免分类器）→ 已保存的规则
            // → 分类器。前三个都是省掉一次分类器往返的快路
            MODE_AUTO => {
                if let Some(reason) = tools::hard_block(action, cwd) {
                    return Verdict::AutoDeny {
                        reason, summary: "自动模式拦下（硬熔断）"
                    };
                }
                if access == Access::Edit && action.inside(cwd) {
                    return Verdict::Allow;
                }
                // 明显只读的命令（`ls`、`git status`、`rg` 这类白名单，见
                // `tools::reads_only`）直接放行：不调分类器、不计入拒绝计数，
                // 和 `Action::Read` 的待遇一样只是走的是命令那条路
                if let Action::Shell { command, .. } | Action::ShellBackground { command, .. } =
                    action
                    && tools::reads_only(command)
                {
                    return Verdict::Allow;
                }
                return self.auto_verdict(session, cwd, action, title, stopped);
            }
            _ => {}
        }
        let rule = action.rule(cwd);
        let command = match action {
            Action::Shell { command, .. } => Some(command.as_str()),
            _ => None,
        };
        {
            let mut rules = self.rules.lock().unwrap();
            // 拒绝规则优先：命中就不问，直接按「用户拒绝」回给模型
            if let Some(rule) = &rule
                && rules.denies(cwd, rule, command)
            {
                return Verdict::Deny(
                    "The user has a saved rule that always denies this action. \
                     Do not retry it; stop and ask the user how they would like to proceed \
                     (the rule can be removed in Settings)."
                        .into(),
                );
            }
            if let Some(rule) = &rule
                && rules.allows(cwd, rule, command)
            {
                return Verdict::Allow;
            }
        }

        let option =
            |id: &str, name: String| PermissionOptionView { id: id.into(), name, kind: id.into() };
        // 「总是」两个选项的文案：kind = allow_always / reject_always
        let always_name = |rule: &str, deny: bool| {
            let verb = if deny { "拒绝" } else { "允许" };
            if let Some(prefix) = rule.strip_prefix("shell:") {
                format!("本项目内总是{verb}「{prefix}」开头的命令")
            } else if let Some(host) = rule.strip_prefix("fetch:") {
                format!("本项目内总是{verb}访问 {host}")
            } else if rule == "search" {
                format!("本项目内总是{verb}搜索网页")
            } else if let Some(tool) = rule.strip_prefix("mcp:") {
                format!("本项目内总是{verb} {}", tool.replacen(':', " 的 ", 1))
            } else {
                format!("本项目内总是{verb}编辑")
            }
        };
        let mut options = vec![option("allow_once", "允许一次".into())];
        if let Some(rule) = &rule {
            options.push(option("allow_always", always_name(rule, false)));
        }
        options.push(option("reject_once", "拒绝".into()));
        if let Some(rule) = &rule {
            options.push(option("reject_always", always_name(rule, true)));
        }

        // 写 / 改文件：把这次的块一并交给权限卡，用户可以在上面逐块勾选
        let hunks = action.permission_hunks(cwd);
        let answer = self.ask(session, title, tool, options, hunks.clone(), stopped);
        let choice = answer.as_ref().map(|a| a.option.as_str());
        match choice {
            Some("allow_once") => match (answer.and_then(|a| a.hunks), &hunks) {
                // 只勾了一部分：只写这几块（写之前核对文件还是问的时候那样）
                (Some(keep), Some(asked)) => Verdict::Partial(tools::KeepHunks::new(keep, asked)),
                _ => Verdict::Allow,
            },
            Some("allow_always") => {
                if let Some(rule) = rule
                    && let Err(e) = self.rules.lock().unwrap().add(cwd, rule)
                {
                    self.emit(None, KernelEvent::Status(format!("保存权限规则失败: {e}")));
                }
                Verdict::Allow
            }
            Some("reject_always") => {
                if let Some(rule) = rule
                    && let Err(e) = self
                        .rules
                        .lock()
                        .unwrap()
                        .add(cwd, format!("{}{}", permissions::DENY_PREFIX, rule))
                {
                    self.emit(None, KernelEvent::Status(format!("保存权限规则失败: {e}")));
                }
                Verdict::Deny(
                    "The user denied this tool call and saved a rule that always denies it. \
                     Do not retry it in this session; ask the user how they would like to proceed."
                        .into(),
                )
            }
            _ => Verdict::Deny(
                "The user denied this tool call. Do not retry it; \
                 stop and ask the user how they would like to proceed."
                    .into(),
            ),
        }
    }

    /// 自动模式：已保存的规则先判，剩下的交给分类器。判定不出来一律按拒绝处理。
    pub(super) fn auto_verdict(
        &self,
        session: &str,
        cwd: &Path,
        action: &Action,
        title: &str,
        stopped: &dyn Fn() -> bool,
    ) -> Verdict {
        let Some(auto) = &self.auto_mode else {
            // 模式列表里不会出现自动模式（没配置），走到这里只能是手改了日志：按默认模式问
            return Verdict::Deny(
                "Automatic mode is not configured in this session. Ask the user to choose a \
                 permission mode."
                    .into(),
            );
        };
        // 规则（「总是允许 / 总是拒绝」）优先于分类器：用户明确表达过意图
        let rule = action.rule(cwd);
        let command = match action {
            Action::Shell { command, .. } => Some(command.as_str()),
            _ => None,
        };
        {
            let mut rules = self.rules.lock().unwrap();
            if let Some(rule) = &rule
                && rules.denies(cwd, rule, command)
            {
                return Verdict::Deny(
                    "The user has a saved rule that always denies this action. \
                     Do not retry it; ask the user how they would like to proceed \
                     (the rule can be removed in Settings)."
                        .into(),
                );
            }
            if let Some(rule) = &rule
                && rules.allows(cwd, rule, command)
            {
                return Verdict::Allow;
            }
        }
        // 连着被拦得太多次：分类器要么缺上下文、要么模型在硬顶，停下来问人
        if let Some(reason) = self.denials_give_up(session) {
            return Verdict::AutoDeny { reason, summary: "自动模式已暂停" };
        }
        let inputs = self.user_inputs(session);
        let verdict = self.classify(session, auto, cwd, action, title, &inputs, stopped);
        // 被取消：不算一次拒绝，也不再发事件（取消后不发任何事件）
        if matches!(&verdict, Verdict::Deny(reason) if reason == "cancelled") {
            return verdict;
        }
        self.note_verdict(session, &verdict);
        // 拒绝的理由连同「别换个马甲重试」的说明一起回给模型：它自己决定换路子还是问用户
        match verdict {
            Verdict::Allow => Verdict::Allow,
            // 分类器不按块同意（它只给放行 / 拒绝），按块只有用户在卡片上勾选这一条路
            Verdict::Partial(keep) => Verdict::Partial(keep),
            Verdict::Deny(reason) => Verdict::Deny(reason),
            Verdict::AutoDeny { reason, summary } => {
                Verdict::AutoDeny { reason: auto::denial_text(&reason), summary }
            }
        }
    }

    /// 会话里被拒绝的次数到了阈值：返回要回给模型的说明（否则 None）。
    pub(super) fn denials_give_up(&self, session: &str) -> Option<String> {
        let counters = self.auto_mode.as_ref()?.denials.lock().unwrap();
        let d = counters.get(session)?;
        (d.streak >= MAX_DENIAL_STREAK || d.total >= MAX_DENIALS).then(|| {
            format!(
                "Automatic mode paused: {} actions were blocked in a row ({} in total) in this \
                 session. Stop and wait for the user: tell them what kept being blocked and ask \
                 how they would like to proceed. Do not try another variation.",
                d.streak, d.total
            )
        })
    }

    /// 记一次判定结果：放行清零连续计数，拒绝两个计数都加。
    /// 放行不发事件（自动模式本来就该安静）；拒绝的结果挂在失败的工具行上。
    pub(super) fn note_verdict(&self, session: &str, verdict: &Verdict) {
        let Some(auto) = &self.auto_mode else { return };
        let mut counters = auto.denials.lock().unwrap();
        let d = counters.entry(session.to_string()).or_default();
        match verdict {
            Verdict::Allow | Verdict::Partial(_) => d.streak = 0,
            Verdict::Deny(_) | Verdict::AutoDeny { .. } => {
                d.streak += 1;
                d.total += 1;
            }
        }
    }

    /// 用户发了消息或提交了答案：连续拒绝清零；累计到了上限时也重新计数。
    /// 否则一旦暂停，分类器再也不会被调用（放行才清零，而暂停时根本不判）。
    pub(super) fn reset_denials(&self, session: &str) {
        let Some(auto) = &self.auto_mode else { return };
        if let Some(d) = auto.denials.lock().unwrap().get_mut(session) {
            d.streak = 0;
            if d.total >= MAX_DENIALS {
                d.total = 0;
            }
        }
    }

    /// 会话里用户发的消息和提交的答案（分类器的输入之一）。
    pub(super) fn user_inputs(&self, session: &str) -> Vec<auto::UserInput> {
        self.sessions
            .lock()
            .unwrap()
            .get(session)
            .map(|s| s.user_inputs.clone())
            .unwrap_or_default()
    }

    /// 调一次分类器。端点出错、回复解析不出来、被取消：都按拒绝处理。
    #[allow(clippy::too_many_arguments)] // 一次判定要的东西本来就多，拆结构体反而绕
    pub(super) fn classify(
        &self,
        session: &str,
        auto: &AutoMode,
        cwd: &Path,
        action: &Action,
        title: &str,
        user_inputs: &[auto::UserInput],
        stopped: &dyn Fn() -> bool,
    ) -> Verdict {
        let system = auto::system_prompt(&auto.config.environment);
        let user = format!(
            "{}\n\n{}",
            auto::user_section(user_inputs),
            auto::action_section(title, action, cwd)
        );
        let messages = vec![ChatMessage::new("system", system), ChatMessage::new("user", user)];
        // 分类器要么用自己的模型和端点，要么跟着会话走
        let (mut config, model) = match &auto.config.model {
            Some(config) => (config.clone(), config.model.clone()),
            None => (
                self.provider_config.clone(),
                self.session_model(session)
                    .unwrap_or_else(|| self.default_model.lock().unwrap().clone()),
            ),
        };
        // 分类器只要一句短判断：档位按模型覆盖成「能关就关、关不掉取最低一档」，
        // 别继承档案里给正式对话配的深度思考（又慢又贵，还容易被思考带偏）
        config.params.thinking = suggest::short_thinking(config.protocol, &model);
        let provider = auto.provider_for(&config);
        let mut reply = String::new();
        // 分类器的请求也记进用量网关（用途 `auto`）
        let _tag = gateway::tag(session, "auto", self.turn_number(session));
        let outcome = provider.stream_chat(&model, &messages, &[], stopped, &mut |event| {
            if let StreamEvent::Text(text) = event {
                reply.push_str(&text);
            }
        });
        if stopped() {
            return Verdict::Deny("cancelled".into());
        }
        match outcome {
            Ok(_) => match auto::parse(&reply) {
                Some(auto::Decision::Allow(_)) => Verdict::Allow,
                Some(auto::Decision::Deny(reason)) => {
                    Verdict::AutoDeny { reason, summary: "自动模式拦下" }
                }
                None => Verdict::AutoDeny {
                    reason: auto::FAILED.into(),
                    summary: "自动模式拦下（分类器失败）",
                },
            },
            Err(err) => {
                self.emit(None, KernelEvent::Status(format!("分类器请求失败：{err}")));
                Verdict::AutoDeny {
                    reason: auto::FAILED.into(),
                    summary: "自动模式拦下（分类器失败）",
                }
            }
        }
    }

    /// 会话当前用的模型（子会话跟着主会话）。
    pub(super) fn session_model(&self, session: &str) -> Option<String> {
        self.sessions.lock().unwrap().get(session).map(|s| s.model.clone())
    }

    /// 发出权限请求并阻塞等待应答；被取消时返回 None。`hunks` 给了就表示这次改动可以
    /// 按块部分同意（界面据此在卡片里列块、允许勾选）。
    pub(super) fn ask(
        &self,
        session: &str,
        title: &str,
        tool: &str,
        options: Vec<PermissionOptionView>,
        hunks: Option<PermissionHunks>,
        stopped: &dyn Fn() -> bool,
    ) -> Option<Answer> {
        let id = self.next_request.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        self.pending.lock().unwrap().insert(id, tx);
        self.emit(
            Some(session),
            KernelEvent::PermissionRequest {
                id,
                title: title.to_string(),
                options,
                tool: Some(tool.to_string()),
                hunks,
            },
        );
        loop {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(Reply::Permission(choice)) => return choice,
                // 通道串了（不可能）：当成取消，别把提问的答案当成权限选项
                Ok(Reply::Question(_)) => return None,
                Err(mpsc::RecvTimeoutError::Timeout) if !stopped() => {}
                Err(_) => {
                    self.pending.lock().unwrap().remove(&id);
                    return None;
                }
            }
        }
    }

    /// `ask_user`：把问题发给界面并阻塞等待回答。用户停下这一轮时返回「已取消」。
    /// 和权限请求一样没有超时：窗口切走 / 最小化都不影响。
    pub(super) fn ask_user(
        &self,
        session: &str,
        arguments: &str,
        stopped: &dyn Fn() -> bool,
    ) -> ToolOutput {
        let questions = match tools::parse_questions(arguments) {
            Ok(questions) => questions,
            Err(message) => return ToolOutput::fail(message, "参数无效"),
        };
        let id = self.next_request.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        self.pending.lock().unwrap().insert(id, tx);
        self.emit(
            Some(session),
            KernelEvent::UserQuestion { id, tool: None, questions: questions.clone() },
        );
        let answer = loop {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(Reply::Question(answer)) => break answer,
                // 串了（不可能）：当取消
                Ok(Reply::Permission(_)) => break QuestionAnswer::Cancelled,
                Err(mpsc::RecvTimeoutError::Timeout) if !stopped() => {}
                Err(_) => {
                    self.pending.lock().unwrap().remove(&id);
                    break QuestionAnswer::Cancelled;
                }
            }
        };
        if let Some(input) = auto::UserInput::question_answer(&questions, &answer) {
            if let Some(state) = self.sessions.lock().unwrap().get_mut(session) {
                state.user_inputs.push(input);
                trim_user_inputs(&mut state.user_inputs);
            }
            self.reset_denials(session);
        }
        let (content, summary) = tools::question_result(&questions, &answer);
        match answer {
            QuestionAnswer::Choices { .. } | QuestionAnswer::Form { .. } => {
                ToolOutput::ok(content, summary)
            }
            // 「你来定」：模型自己挑一个继续，结果照常回给它
            QuestionAnswer::ModelDecides => ToolOutput::ok(content, summary),
            // 「自己说」/ 取消：这一轮停在这里，等用户下一条消息
            QuestionAnswer::SelfAnswer | QuestionAnswer::Cancelled => {
                ToolOutput::fail_stop(content, summary)
            }
        }
    }
}

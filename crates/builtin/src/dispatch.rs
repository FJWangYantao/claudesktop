//! 工具调用的分发：按名字路由到具体工具、并行执行、权限与检查点、子任务。

use super::*;

impl Inner {
    /// 执行一次工具调用（含权限判定），并把进度报给 UI。
    pub(super) fn run_tool(
        &self,
        session: &str,
        cwd: &Path,
        call: &ToolCall,
        stopped: &dyn Fn() -> bool,
    ) -> ToolOutput {
        // 聊天会话：不在名单里的工具一律回错误结果（模型照着续接前的文字瞎编、
        // 或者别家模型幻觉都会走到这里），不进 authorize
        if tools::exposure(&call.function.name) == tools::Exposure::Deferred {
            // 记下这一步用到了它：下一轮的工具预算排序把它排最前
            self.touch_tool(session, &call.function.name);
        }
        if let Some(web) = self.chat_web_of(session)
            && !tools::chat_allows(web, &call.function.name)
        {
            return ToolOutput::fail(
                "This tool is not available in a chat conversation.",
                "聊天里没有这个工具",
            );
        }
        // 待办清单不碰文件、不用授权，也不占一行工具：直接换掉界面上的计划（子会话没有这个工具）
        if call.function.name == tools::TODO_WRITE && self.tool_allowed(session, tools::TODO_WRITE)
        {
            return match tools::parse_todos(&call.function.arguments) {
                Ok(entries) => {
                    let summary = tools::todo_summary(&entries);
                    if let Some(state) = self.sessions.lock().unwrap().get_mut(session) {
                        state.todos = entries.clone();
                    }
                    self.emit(Some(session), KernelEvent::PlanReplaced { entries });
                    ToolOutput::ok(format!("Todo list updated: {summary}"), summary)
                }
                Err(message) => ToolOutput::fail(message, "参数无效"),
            };
        }
        // 向用户提问：不碰文件、不用授权，也不占一行工具（卡片自己就是那一行）。
        // 子会话没有这个工具（没人看得到它的问题），`tool_allowed` 会拦住
        if call.function.name == tools::ASK_USER && self.tool_allowed(session, tools::ASK_USER) {
            return self.ask_user(session, &call.function.arguments, stopped);
        }
        // 找工具：不碰文件、不用授权，也不占一行工具行（它只换下一轮声明给模型的清单）。
        // 子会话不参与工具分层（工具按角色白名单给），直接回一句
        if call.function.name == tools::TOOL_SEARCH
            && self.tool_allowed(session, tools::TOOL_SEARCH)
        {
            if self.is_subagent(session) {
                return ToolOutput::fail(
                    "A subagent has no tool_search: its tools are fixed by the role it was \
                     started with. Work with what you have.",
                    "子任务里没有这个工具",
                );
            }
            return self.search_tools(session, &call.function.arguments);
        }
        // 记忆不出网、不用授权：直接写 memory.json（见 memory 模块）。但它是写文件，
        // Plan 模式（只读）不给（工具列表里也没有，这里挡住模型硬调的）
        if call.function.name == tools::MEMORY_WRITE
            && self.tool_allowed(session, tools::MEMORY_WRITE)
        {
            if self.mode(session) == MODE_PLAN {
                return ToolOutput::fail(
                    "Plan mode is active: memory_write is not available. Present a plan instead.",
                    "计划模式不能记",
                );
            }
            return self.remember(session, cwd, &call.function.arguments);
        }
        // 查记忆只读（哪种模式都能用）；删记忆是写文件，Plan 模式下不给
        if call.function.name == tools::MEMORY_SEARCH
            && self.tool_allowed(session, tools::MEMORY_SEARCH)
        {
            return self.search_memory(cwd, &call.function.arguments);
        }
        if call.function.name == tools::MEMORY_FORGET
            && self.tool_allowed(session, tools::MEMORY_FORGET)
        {
            if self.mode(session) == MODE_PLAN {
                return ToolOutput::fail(
                    "Plan mode is active: memory_forget is not available. Present a plan instead.",
                    "计划模式不能删记忆",
                );
            }
            return self.forget_memory(session, cwd, &call.function.arguments);
        }
        let (action, title) = self.start_tool(session, cwd, call);
        self.finish_tool(session, cwd, call, action, &title, stopped)
    }

    /// `tool_search`：把没直接声明的工具找出来，并加载进本会话（下一次请求起生效）。
    ///
    /// 返回里必须说清楚「这一步已经发完了，下一轮才能调」：模型拿到工具结果时这一步的
    /// 请求已经发出去了，它在同一步里再调刚搜到的工具只会报「不在名单里」。
    pub(super) fn search_tools(&self, session: &str, arguments: &str) -> ToolOutput {
        let query = match tools::parse_tool_search(arguments) {
            Ok(query) => query,
            Err(message) => return ToolOutput::fail(message, "参数无效"),
        };
        let candidates = self.tool_candidates(session);
        let found = tools::search(&query, &candidates);
        if found.is_empty() {
            return ToolOutput::ok(
                format!(
                    "No tool matches \"{query}\". Try different words (a tool name or what you want \
                     to do), or do it with the tools you already have. Tools already loaded in this \
                     session are not listed again."
                ),
                format!("没找到「{query}」"),
            );
        }
        // 并进本会话的名单（去重），再把完整名单落进日志：续接 / 回退时内核从这里把名单还回去
        let names = {
            let mut sessions = self.sessions.lock().unwrap();
            let Some(state) = sessions.get_mut(session) else {
                return ToolOutput::fail("This session is gone.", "会话已结束");
            };
            state.step_seq += 1;
            let step = state.step_seq;
            for name in &found {
                if !state.loaded_tools.contains(name) {
                    state.loaded_tools.push(name.clone());
                }
                // 刚搜出来的排最前：预算不够时先保它们
                state.tool_last_step.insert(name.clone(), step);
            }
            state.loaded_tools.clone()
        };
        self.emit(Some(session), KernelEvent::ToolsLoaded { names });
        let lines: Vec<String> = found
            .iter()
            .map(|name| match candidates.iter().find(|(candidate, _)| candidate == name) {
                Some((_, description)) => format!("- {name}: {description}"),
                None => format!("- {name}"),
            })
            .collect();
        ToolOutput::ok(
            format!(
                "Found {} tool(s):\n{}\n\nThey are available from your NEXT request onwards. This \
                 request is already sent, so do not call them in this same step — say what you are \
                 going to do with them, and call them in the next round.",
                found.len(),
                lines.join("\n")
            ),
            format!("找到 {} 个工具：{}", found.len(), found.join("、")),
        )
    }

    /// `tool_search` 的候选语料：这个会话「能拿但还没声明」的工具（名字, 说明）。
    ///
    /// 语料就是工具定义本身（和 `run_steps` 里那份一致），不另维护一份描述；已加载的不再给。
    pub(super) fn tool_candidates(&self, session: &str) -> Vec<(String, String)> {
        let (disabled, plugin_scope) = {
            let sessions = self.sessions.lock().unwrap();
            sessions
                .get(session)
                .map(|s| (s.mcp_disabled.clone(), s.plugins_enabled.clone()))
                .unwrap_or_default()
        };
        let writable = self.mode(session) != MODE_PLAN;
        let mut candidates: Vec<_> = tools::specs(writable)
            .iter()
            .map(|spec| {
                (
                    subagent::spec_name(spec).to_string(),
                    spec["function"]["description"].as_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        candidates.extend(self.mcp.descriptions(writable, &disabled));
        let task = subagent::spec(&self.agents(plugin_scope.as_deref()));
        candidates.push((
            subagent::TASK.to_string(),
            task["function"]["description"].as_str().unwrap_or_default().to_string(),
        ));
        let loaded = self.loaded_tools(session);
        candidates
            .into_iter()
            .filter(|(name, _)| tools::exposure(name) == tools::Exposure::Deferred)
            .filter(|(name, _)| !loaded.contains(name))
            .collect()
    }

    /// `memory_write`：写进全局或项目的 `memory.json`，报 `MemoryChanged`。
    /// 读写文件失败不让工具失败——把原因回给模型，免得它反复重试。
    pub(super) fn remember(&self, session: &str, cwd: &Path, arguments: &str) -> ToolOutput {
        use claudesktop_core::memory;
        let (scope, entry) = match tools::parse_memory(arguments) {
            Ok(parsed) => parsed,
            Err(message) => return ToolOutput::fail(message, "参数无效"),
        };
        // 聊天会话没有工作目录：一律记到全局，不会往 `<数据目录>/chat/.claudesktop/` 里写
        let scope = if self.is_chat(session) { memory::Scope::Global } else { scope };
        let (path, scope_name) = match scope {
            memory::Scope::Global => (self.memory_path.clone(), "global"),
            memory::Scope::Project => (Some(memory::project_file_or(cwd)), "project"),
        };
        let Some(path) = path else {
            return ToolOutput::fail(
                "Could not find the data directory, so the note was not saved.",
                "没能保存",
            );
        };
        let mut entry = entry;
        entry.source =
            Some(memory::Source { session: session.to_string(), turn: self.turn_number(session) });
        // 项目记忆在项目目录里（进 git）：和别的写文件工具一样，前后各读一次交给检查点，回退时还原
        let before =
            (scope == memory::Scope::Project).then(|| checkpoint::read_snapshot(&path)).flatten();
        // 读 → 改 → 写全程持锁（几个会话、设置页同时改不会互相覆盖）；读不了就不动文件，
        // 格式坏了挪到旁边再记（不覆盖之前挪开的）
        let used_ids = self.used_ids(session);
        let result = memory::update(&path, memory::OnBroken::MoveAside, |entries| {
            let updated = memory::upsert(entries, entry.clone());
            memory::enforce_limits(entries, scope.max_entries());
            // 顺手把这一轮带进过上下文的那些标成刚用过：淘汰时先挑最久没用的
            memory::mark_used(entries, &used_ids, memory::now_ms());
            (updated, entries.len())
        });
        if let Some(before) = before
            && let Some(after) = checkpoint::read_snapshot(&path)
            && before != after
        {
            let file = path.to_string_lossy().into_owned();
            self.emit(Some(session), KernelEvent::FileChanged { path: file, before, after });
        }
        match result {
            Ok(((updated, count), notice)) => {
                if let Some(notice) = notice {
                    self.emit(Some(session), KernelEvent::Notice(notice));
                }
                self.emit(
                    Some(session),
                    KernelEvent::MemoryChanged { scope: scope_name.into(), count },
                );
                let verb = if updated { "Updated" } else { "Saved" };
                let scope = match scope_name {
                    "global" => "every project",
                    _ => "this project",
                };
                ToolOutput::ok(
                    format!("{verb} for {scope}: {}", entry.text),
                    format!("记下了：{}", entry.text),
                )
            }
            Err(text) => {
                self.emit(Some(session), KernelEvent::Notice(text.clone()));
                ToolOutput::fail(text, "没能保存")
            }
        }
    }

    /// 这一轮带进过上下文的条目 id（`used_ms` 据此刷新）。
    pub(super) fn used_ids(&self, session: &str) -> Vec<String> {
        self.sessions
            .lock()
            .unwrap()
            .get(session)
            .map(|s| s.memory_used.clone())
            .unwrap_or_default()
    }

    /// `memory_search`：在两份记忆里按词找，返回命中的条目（只读，不用授权）。
    pub(super) fn search_memory(&self, cwd: &Path, arguments: &str) -> ToolOutput {
        use claudesktop_core::memory;
        let (query, where_) = match tools::parse_memory_query(tools::MEMORY_SEARCH, arguments) {
            Ok(parsed) => parsed,
            Err(message) => return ToolOutput::fail(message, "参数无效"),
        };
        // (属于哪个范围, 条目)；两份文件都是现读（界面里的增删立刻生效）
        let mut found: Vec<(memory::Scope, memory::Entry)> = Vec::new();
        if where_.includes(memory::Scope::Project) {
            let (entries, _) = memory::load(&memory::project_file_or(cwd));
            found.extend(entries.into_iter().map(|e| (memory::Scope::Project, e)));
        }
        if where_.includes(memory::Scope::Global) {
            let (entries, _) = self.memory_path.as_deref().map(memory::load).unwrap_or_default();
            found.extend(entries.into_iter().map(|e| (memory::Scope::Global, e)));
        }
        let by_entry: std::collections::HashMap<&str, memory::Scope> =
            found.iter().map(|(scope, e)| (e.id.as_str(), *scope)).collect();
        let ordered: Vec<&memory::Entry> = found.iter().map(|(_, e)| e).collect();
        let hits: Vec<(&memory::Entry, usize)> = memory::ranked(ordered, &query, memory::MIN_SCORE);
        if hits.is_empty() {
            return ToolOutput::ok(
                format!("No saved note matches \"{query}\"."),
                format!("没有匹配「{query}」的记忆"),
            );
        }
        let lines: Vec<String> = hits
            .iter()
            .take(tools::MEMORY_SEARCH_MAX)
            .map(|(entry, _)| {
                let scope = by_entry.get(entry.id.as_str()).copied().unwrap_or_default();
                let where_ = match scope {
                    memory::Scope::Global => "global",
                    memory::Scope::Project => "project",
                };
                let source = entry
                    .source
                    .as_ref()
                    .map(|s| format!(" — saved in session {} (turn {})", s.session, s.turn))
                    .unwrap_or_default();
                format!("- [{}] {}{source}", where_, entry.text)
            })
            .collect();
        ToolOutput::ok(
            format!("Saved notes matching \"{query}\":\n{}", lines.join("\n")),
            format!("找到 {} 条记忆", lines.len()),
        )
    }

    /// `memory_forget`：删掉本会话记下的、和 `query` 对得上的条目（别的会话记下的删不掉，
    /// 免得误删别人的；界面里可以删任意一条）。
    pub(super) fn forget_memory(&self, session: &str, cwd: &Path, arguments: &str) -> ToolOutput {
        use claudesktop_core::memory;
        let (query, where_) = match tools::parse_memory_query(tools::MEMORY_FORGET, arguments) {
            Ok(parsed) => parsed,
            Err(message) => return ToolOutput::fail(message, "参数无效"),
        };
        // 主会话 ID（子会话记不下东西，也就没有能删的）
        let owner = subagent::split_id(session).map_or(session, |(parent, _)| parent);
        let mut removed: Vec<String> = Vec::new();
        let mut failed: Option<String> = None;
        let mut scopes: Vec<(memory::Scope, Option<PathBuf>)> = Vec::new();
        if where_.includes(memory::Scope::Project) {
            scopes.push((memory::Scope::Project, Some(memory::project_file_or(cwd))));
        }
        if where_.includes(memory::Scope::Global) {
            scopes.push((memory::Scope::Global, self.memory_path.clone()));
        }
        for (scope, path) in scopes {
            // 文件还没建过 = 没有可删的；别为一次空删除在项目里新建 `.claudesktop/memory.json`
            let Some(path) = path.filter(|p| p.is_file()) else {
                continue;
            };
            // 项目记忆进 git：和别的写文件工具一样交给检查点，回退时还原
            let before = (scope == memory::Scope::Project)
                .then(|| checkpoint::read_snapshot(&path))
                .flatten();
            let result = memory::update(&path, memory::OnBroken::Refuse, |entries| {
                // 删掉的是哪些（正文）：删之前记下，删之后对比，和 `forget` 的判据永远一致
                let texts: Vec<(String, String)> =
                    entries.iter().map(|e| (e.id.clone(), e.text.clone())).collect();
                let count = memory::forget(entries, &query, Some(owner));
                let gone: Vec<String> = texts
                    .into_iter()
                    .filter(|(id, _)| !entries.iter().any(|e| &e.id == id))
                    .map(|(_, text)| text)
                    .collect();
                (count, entries.len(), gone)
            });
            if let Some(before) = before
                && let Some(after) = checkpoint::read_snapshot(&path)
                && before != after
            {
                let file = path.to_string_lossy().into_owned();
                self.emit(Some(session), KernelEvent::FileChanged { path: file, before, after });
            }
            match result {
                Ok(((count, total, texts), _)) => {
                    if count > 0 {
                        self.emit(
                            Some(session),
                            KernelEvent::MemoryChanged { scope: scope.name().into(), count: total },
                        );
                    }
                    removed.extend(texts);
                }
                Err(text) => failed = Some(text),
            }
        }
        if !removed.is_empty() {
            return ToolOutput::ok(
                format!("Deleted {} note(s): {}", removed.len(), removed.join("; ")),
                format!("删掉了：{}", removed.join("；")),
            );
        }
        if let Some(text) = failed {
            self.emit(Some(session), KernelEvent::Notice(text.clone()));
            return ToolOutput::fail(text, "没能删除");
        }
        ToolOutput::ok(
            "No note saved by this session matches that, so nothing was deleted. Only notes \
             saved in this same session can be deleted here; notes from earlier sessions can be \
             deleted on the settings page (Memory)."
                .to_string(),
            "没有可删的记忆（只能删本会话记下的）".to_string(),
        )
    }

    /// 一步里的全部调用：连续的只读调用和子任务（`task`）一起并行跑，其余依次执行；结果按调用顺序返回。
    /// 中途被取消时返回 None。
    pub(super) fn run_tools(
        &self,
        session: &str,
        cwd: &Path,
        calls: &[ToolCall],
        stopped: &(dyn Fn() -> bool + Sync),
    ) -> Option<Vec<ToolOutput>> {
        let mut outputs = Vec::with_capacity(calls.len());
        let mut rest = calls;
        while !rest.is_empty() {
            let n = rest
                .iter()
                .take_while(|c| {
                    let name = &c.function.name;
                    tools::parallel_safe(name) || name == subagent::TASK
                })
                .count()
                .min(PARALLEL_TOOLS);
            if n < 2 {
                let output = self.run_tool(session, cwd, &rest[0], stopped);
                let halt = output.stop_turn;
                outputs.push(output);
                rest = &rest[1..];
                // 用户在提问卡片上选了「自己说」/ 取消：同一步后面的调用不再执行，
                // 只补上结果（tool_calls 后面必须跟着对应的 tool 消息）
                if halt {
                    outputs.extend(rest.iter().map(|call| self.skip_tool(session, call)));
                    break;
                }
            } else {
                let (group, tail) = rest.split_at(n);
                // 先按顺序报出全部工具行，界面上的顺序和模型的调用顺序一致
                let started: Vec<_> =
                    group.iter().map(|c| self.start_tool(session, cwd, c)).collect();
                outputs.extend(std::thread::scope(|s| {
                    let handles: Vec<_> = group
                        .iter()
                        .zip(started)
                        .map(|(call, (action, title))| {
                            s.spawn(move || {
                                self.finish_tool(session, cwd, call, action, &title, stopped)
                            })
                        })
                        .collect();
                    handles
                        .into_iter()
                        .map(|h| {
                            h.join().unwrap_or_else(|_| ToolOutput::fail("tool crashed", "出错"))
                        })
                        .collect::<Vec<_>>()
                }));
                rest = tail;
            }
            if stopped() {
                return None;
            }
        }
        Some(outputs)
    }

    /// 用户叫停后没执行的调用：报一行失败的工具（续接时历史里也有它的结果），回给模型说明原因。
    pub(super) fn skip_tool(&self, session: &str, call: &ToolCall) -> ToolOutput {
        let content = "Not executed: the user stopped this turn at your question.";
        self.emit(
            Some(session),
            KernelEvent::ToolCallStarted {
                id: call.id.clone(),
                title: call.function.name.clone(),
                status: "failed".into(),
                kind: "other".into(),
                parent: None,
            },
        );
        self.emit(
            Some(session),
            KernelEvent::ToolCallUpdated {
                id: call.id.clone(),
                title: None,
                status: Some("failed".into()),
                raw_output: Some("用户停在了提问，未执行".into()),
                output: Some(claudesktop_core::output_preview(content)),
            },
        );
        ToolOutput::fail(content, "用户停在了提问，未执行")
    }

    /// 解析调用并报出工具行（改动类先带上 diff 预览）。
    pub(super) fn start_tool(
        &self,
        session: &str,
        cwd: &Path,
        call: &ToolCall,
    ) -> (Result<Action, String>, String) {
        let name = &call.function.name;
        let action = if !self.tool_allowed(session, name) {
            Err(format!("Tool `{name}` is not available to this subagent"))
        } else if mcp::is_mcp_tool(name) {
            self.mcp_action(name, &call.function.arguments)
        } else {
            Action::parse(name, &call.function.arguments, cwd)
        };
        // 聊天会话里抓网页不问用户（见 `authorize`），所以只许访问公网
        let action = action.map(|mut a| {
            if let Action::Fetch { public_only, .. } = &mut a {
                *public_only = self.is_chat(session);
            }
            a
        });
        let (title, kind) = match &action {
            Ok(a) => (a.title(cwd), a.kind()),
            Err(_) => (call.function.name.clone(), "other"),
        };
        self.emit(
            Some(session),
            KernelEvent::ToolCallStarted {
                id: call.id.clone(),
                title: title.clone(),
                status: "pending".into(),
                kind: kind.into(),
                parent: None,
            },
        );
        // 改动预览：先于权限请求发出，用户能看着 diff 决定是否放行
        if let Some(diff) = action.as_ref().ok().and_then(|a| a.preview(cwd)) {
            self.emit(Some(session), KernelEvent::ToolCallDiff { id: call.id.clone(), diff });
        }
        (action, title)
    }

    /// 执行内置工具；写文件 / 改文件前后各读一次，交给 UI 存检查点（回退时还原）。
    pub(super) fn run_checkpointed(
        &self,
        session: &str,
        call_id: &str,
        action: &Action,
        stopped: &dyn Fn() -> bool,
        keep: Option<&tools::KeepHunks>,
    ) -> ToolOutput {
        let target = action.written_path().map(Path::to_path_buf);
        let before = target.as_deref().and_then(checkpoint::read_snapshot);
        // 后台任务记在这个会话名下（事件里按会话分组）
        bgtasks::set_current(self.kernel_id, session);
        let output = action.run(
            stopped,
            &|output| {
                let id = call_id.to_string();
                self.emit(Some(session), KernelEvent::ToolCallProgress { id, output });
            },
            keep,
        );
        if let (Some(path), Some(before)) = (target, before)
            && let Some(after) = checkpoint::read_snapshot(&path)
            && before != after
        {
            let path = path.to_string_lossy().into_owned();
            self.emit(Some(session), KernelEvent::FileChanged { path, before, after });
        }
        output
    }

    /// 模型调的 MCP 工具 → 动作；服务器没连上或没有这个工具时的错误回给模型。
    pub(super) fn mcp_action(&self, name: &str, arguments: &str) -> Result<Action, String> {
        let (server, tool, read_only) = self.mcp.lookup(name).ok_or_else(|| {
            format!("Unknown tool `{name}` (its MCP server may have disconnected)")
        })?;
        let args = if arguments.trim().is_empty() { "{}" } else { arguments };
        let arguments: serde_json::Value =
            serde_json::from_str(args).map_err(|e| format!("Invalid arguments for {name}: {e}"))?;
        Ok(Action::Mcp { server, tool, arguments, read_only })
    }

    /// 授权、执行，并报告结果。
    pub(super) fn finish_tool(
        &self,
        session: &str,
        cwd: &Path,
        call: &ToolCall,
        action: Result<Action, String>,
        title: &str,
        stopped: &dyn Fn() -> bool,
    ) -> ToolOutput {
        let tool_input: Value =
            serde_json::from_str(&call.function.arguments).unwrap_or(Value::Null);
        let hook_input = |extra: Option<(&str, Value)>| {
            let mut input = json!({"tool_name": call.function.name, "tool_input": tool_input});
            if let Some((key, value)) = extra {
                input[key] = value;
            }
            input
        };
        // 插件的「执行工具前」钩子在授权之前：被拦下就不问用户了
        let blocked = match &action {
            Ok(_) => {
                self.run_hooks(
                    session,
                    cwd,
                    HookEvent::PreToolUse,
                    Some(&call.function.name),
                    hook_input(None),
                )
                .1
            }
            Err(_) => None,
        };
        let mut output = match action {
            Err(message) => ToolOutput::fail(message, "参数无效"),
            Ok(_) if blocked.is_some() => ToolOutput::fail(
                format!("Blocked by a plugin hook: {}", blocked.unwrap_or_default()),
                "插件钩子拦下",
            ),
            Ok(action) => {
                let verdict = self.authorize(session, cwd, &action, title, &call.id, stopped);
                match verdict {
                    Verdict::Allow | Verdict::Partial(_) => {
                        if stopped() {
                            return ToolOutput::fail("cancelled", "已中断");
                        }
                        self.emit(
                            Some(session),
                            KernelEvent::ToolCallUpdated {
                                id: call.id.clone(),
                                title: None,
                                status: Some("in_progress".into()),
                                raw_output: None,
                                output: None,
                            },
                        );
                        // 按块同意：只写这几块（其余块留着旧内容）
                        let keep = match verdict {
                            Verdict::Partial(keep) => Some(keep),
                            _ => None,
                        };
                        match &action {
                            Action::Mcp { server, tool, arguments, .. } => {
                                self.mcp.call(server, tool, arguments.clone(), stopped)
                            }
                            Action::Task { prompt, agent, .. } => {
                                self.run_task(session, cwd, &call.id, prompt, agent.as_deref())
                            }
                            _ => self.run_checkpointed(
                                session,
                                &call.id,
                                &action,
                                stopped,
                                keep.as_ref(),
                            ),
                        }
                    }
                    Verdict::Deny(reason) => ToolOutput::fail(reason, "已拒绝"),
                    Verdict::AutoDeny { reason, summary } => ToolOutput::fail(reason, summary),
                }
            }
        };
        // 「执行工具后」钩子：退出码 2 的原因追加到结果里给模型看
        if output.ok && !stopped() {
            let response = json!({"content": output.content});
            if let (_, Some(feedback)) = self.run_hooks(
                session,
                cwd,
                HookEvent::PostToolUse,
                Some(&call.function.name),
                hook_input(Some(("tool_response", response))),
            ) {
                output.content.push_str(&format!("\n\nPlugin hook feedback: {feedback}"));
            }
        }
        if !stopped() {
            self.emit(
                Some(session),
                KernelEvent::ToolCallUpdated {
                    id: call.id.clone(),
                    title: None,
                    status: Some(if output.ok { "completed" } else { "failed" }.into()),
                    raw_output: Some(output.summary.clone()),
                    output: Some(claudesktop_core::output_preview(&output.content)),
                },
            );
            // 工具带回来的图片：界面上在工具行里画缩略图。不进日志（base64 太大）。
            // 发的是**未处理**的那份：要不要真的发给模型由 `run_steps` 按模型 / 协议决定，
            // 界面上总能看到（模型看不了图也能看）
            if !output.images.is_empty() {
                let images = output
                    .images
                    .iter()
                    .map(|image| PromptImage {
                        media_type: image.media_type.clone(),
                        data: image.data.clone(),
                    })
                    .collect();
                self.emit(
                    Some(session),
                    KernelEvent::ToolCallImages { id: call.id.clone(), images },
                );
            }
        }
        output
    }

    /// 子任务：开子会话跑完，最后一段回复当结果（见 `subagent` 模块）。在主会话这一轮的线程里阻塞着跑，
    /// 取消标志用主会话这一轮的。
    pub(super) fn run_task(
        &self,
        session: &str,
        cwd: &Path,
        call_id: &str,
        prompt: &str,
        agent: Option<&str>,
    ) -> ToolOutput {
        let name = agent.unwrap_or(subagent::GENERAL);
        let scope = {
            let sessions = self.sessions.lock().unwrap();
            sessions.get(session).and_then(|s| s.plugins_enabled.clone())
        };
        let agents = self.agents(scope.as_deref());
        let Some(agent) = agents.iter().find(|a| a.name == name).cloned() else {
            let names: Vec<&str> = agents.iter().map(|a| a.name.as_str()).collect();
            return ToolOutput::fail(
                format!("Unknown subagent_type `{name}`. Available: {}", names.join(", ")),
                "没有这个角色",
            );
        };
        let child = format!("{session}/{call_id}");
        let flag = {
            let mut sessions = self.sessions.lock().unwrap();
            let Some((flag, state)) = sessions.get(session).and_then(|parent| {
                let flag = parent.turn.clone()?;
                let state = SessionState {
                    cwd: parent.cwd.clone(),
                    // 跟着主会话走：`chat_web_of` / `is_chat` 按子会话自己的 ID 查这两项
                    // （聊天里没有 task 工具，目前开不出聊天的子会话，免得以后漏掉）
                    kind: parent.kind,
                    web: parent.web,
                    // 角色写了模型 → 设置里的子任务模型 → 跟主会话
                    model: agent
                        .model
                        .clone()
                        .or_else(|| self.subagent_model.clone())
                        .unwrap_or_else(|| parent.model.clone()),
                    // 子任务跟主会话的档位走（用户中途换档也跟上）
                    thinking: parent.thinking,
                    history: vec![ChatMessage::new("user", prompt)],
                    history_storage: HistoryStorage::Resident,
                    history_generation: 0,
                    history_io: Arc::new(Mutex::new(())),
                    history_activity: 0,
                    mode: parent.mode.clone(),
                    restored: false,
                    instructions: parent.instructions.clone(),
                    skills: parent.skills.clone(),
                    started: true,
                    summary: None,
                    measured: None,
                    turn: Some(flag.clone()),
                    turns: parent.turns,
                    agent: Some(agent.clone()),
                    // 项目级停用的 MCP 服务器子会话照样停用
                    mcp_disabled: parent.mcp_disabled.clone(),
                    plugins_enabled: parent.plugins_enabled.clone(),
                    // 子会话和主会话看同一份记忆（记不进去：工具不给它）
                    memory_project: parent.memory_project.clone(),
                    memory_global: parent.memory_global.clone(),
                    memory_shown: parent.memory_shown.clone(),
                    memory_used: Vec::new(),
                    // 子会话的分类器判定同样要看用户说过的话
                    user_inputs: parent.user_inputs.clone(),
                    // 子会话没有待办清单
                    todos: Vec::new(),
                    steps_since_todo: 0,
                    // 子会话的工具按角色清单筛（`subagent::allows`），不走 exposure 那一套
                    loaded_tools: Vec::new(),
                    step_seq: parent.step_seq,
                    tool_last_step: parent.tool_last_step.clone(),
                };
                Some((flag, state))
            }) else {
                return ToolOutput::fail("cancelled", "已中断");
            };
            sessions.insert(child.clone(), state);
            flag
        };
        self.emit(
            Some(session),
            KernelEvent::SubagentStarted {
                task: call_id.to_string(),
                agent: agent.name.clone(),
                prompt: prompt.to_string(),
            },
        );
        let result = run_steps(self, &child, &flag);
        self.sessions.lock().unwrap().remove(&child);
        match result {
            Steps::Stopped => ToolOutput::fail("cancelled", "已中断"),
            Steps::Failed(message) => {
                ToolOutput::fail(format!("The subagent failed: {message}"), "子任务出错")
            }
            Steps::Done { stop, reply } => {
                // 量已经按步报进主会话的累计了（见 `run_steps` 里的 `report_usage`），这里不再报
                self.run_hooks(
                    session,
                    cwd,
                    HookEvent::SubagentStop,
                    None,
                    json!({"subagent_type": agent.name, "stop_reason": stop}),
                );
                if reply.trim().is_empty() {
                    return ToolOutput::fail(
                        format!("The subagent stopped ({stop}) without a final report."),
                        "子任务没有给出结果",
                    );
                }
                let summary = subagent::summary(&reply);
                ToolOutput::ok(reply, summary)
            }
        }
    }
}

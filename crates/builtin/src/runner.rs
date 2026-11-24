//! 一次运行的工具循环：提示词进场、并行工具、steer 注入点、压缩与收尾。

use super::*;

pub(crate) fn run_pending_prompt(
    inner: Arc<Inner>,
    session: String,
    prompt: claudesktop_core::UserPrompt,
    flag: Arc<AtomicBool>,
    submitted_at: std::time::Instant,
) {
    let worker_at = std::time::Instant::now();
    if let Err(err) = inner.wake_history(&session) {
        if !flag.load(Ordering::SeqCst) {
            inner.finish(&session, &flag);
            inner.emit(Some(&session), KernelEvent::PromptFailed { message: format!("{err:#}") });
        }
        return;
    }
    let restored_at = std::time::Instant::now();
    let Some(warnings) = inner.prepare_prompt(&session, prompt, &flag) else {
        return;
    };
    if std::env::var_os("CLAUDESKTOP_HIBERNATE_BENCH").is_some() {
        eprintln!(
            "[hibernate_send] {}",
            json!({
                "session": session,
                "queue_ms": worker_at.duration_since(submitted_at).as_secs_f64() * 1000.,
                "restore_ms": restored_at.duration_since(worker_at).as_secs_f64() * 1000.,
                "prepare_ms": restored_at.elapsed().as_secs_f64() * 1000.,
                "preflight_ms": submitted_at.elapsed().as_secs_f64() * 1000.,
            })
        );
    }
    inner.reset_denials(&session);
    for warning in warnings {
        inner.emit(Some(&session), KernelEvent::Notice(warning));
    }
    run_turn(inner, session, flag);
}

pub(crate) fn run_compact(
    inner: Arc<Inner>,
    session: String,
    model: String,
    flag: Arc<AtomicBool>,
) {
    let restored = inner.wake_history(&session);
    if flag.load(Ordering::SeqCst) {
        return;
    }
    if let Err(err) = restored {
        inner.finish(&session, &flag);
        inner.emit(Some(&session), KernelEvent::CompactFailed { message: format!("{err:#}") });
        inner.emit(Some(&session), KernelEvent::TurnEnded { stop_reason: "end_turn".into() });
        return;
    }
    let empty = inner.sessions.lock().unwrap().get(&session).is_none_or(|s| s.history.is_empty());
    if empty {
        inner.finish(&session, &flag);
        inner.emit(
            Some(&session),
            KernelEvent::CompactFailed { message: "还没有可以压缩的对话".into() },
        );
        inner.emit(Some(&session), KernelEvent::TurnEnded { stop_reason: "end_turn".into() });
        return;
    }
    let budget = transcript_budget(inner.window(&model));
    let result = inner.compact(&session, &flag, &model, true, 0, budget);
    if flag.load(Ordering::SeqCst) {
        return;
    }
    inner.finish(&session, &flag);
    if let Ok(Some(u)) = result {
        inner.report_usage(&session, u, 0);
    }
    inner.emit(Some(&session), KernelEvent::TurnEnded { stop_reason: "end_turn".into() });
}

/// 轮末仍读历史的工作用活动计数保护（`turn` 已经先放掉，界面才能接着发下一轮）。
struct HistoryActivityGuard {
    inner: Arc<Inner>,
    session: String,
}

impl Drop for HistoryActivityGuard {
    fn drop(&mut self) {
        self.inner.end_history_activity(&self.session);
    }
}

/// 一轮对话（在独立线程里运行）：先跑提交消息的钩子，再走工具循环（用量逐步当场报），最后结束。
pub(crate) fn run_turn(inner: Arc<Inner>, session: String, flag: Arc<AtomicBool>) {
    if !inner.prompt_hooks(&session, &flag) {
        return;
    }
    let stop = match run_steps(&inner, &session, &flag) {
        Steps::Stopped => return, // cancel() 已经报告过 TurnEnded
        Steps::Failed(message) => {
            // 和从日志重建的历史一致（见 history::rebuild）：失败也占一条 assistant 消息，
            // 用户点「重试」重发时不会出现两条连着的用户消息（Anthropic 要求交替）
            inner.commit(
                &session,
                &flag,
                vec![ChatMessage::new("assistant", format!("[turn failed: {message}]"))],
            );
            inner.finish(&session, &flag);
            inner.emit(Some(&session), KernelEvent::PromptFailed { message });
            return;
        }
        Steps::Done { stop, .. } => stop,
    };

    if !inner.begin_history_activity(&session) {
        return;
    }
    let _activity = HistoryActivityGuard { inner: inner.clone(), session: session.clone() };
    inner.finish(&session, &flag);
    let cwd = inner.sessions.lock().unwrap().get(&session).map(|s| s.cwd.clone());
    inner.emit(Some(&session), KernelEvent::TurnEnded { stop_reason: stop.clone() });
    // 「一轮结束」钩子只是通知：这一轮已经结束了，结果不影响什么
    if let Some(cwd) = cwd {
        inner.run_hooks(&session, &cwd, HookEvent::Stop, None, json!({"stop_reason": stop}));
    }
    // 第一轮正常结束后让模型给会话起个标题（失败静默，保留截断版）；子会话不用
    if stop == "end_turn" && inner.session_titles && inner.is_first_turn(&session) {
        name_session(&inner, &session);
    }
    // 一轮正常结束：提炼「候选」记忆（默认关；只进待确认区，见 `remember` 模块）
    if stop == "end_turn" && inner.memory_suggest && subagent::split_id(&session).is_none() {
        suggest_memories(&inner, &session);
    }
}

/// 一轮结束后提炼候选记忆：一次小请求，读这一轮的用户消息和最后的回复。
/// 结果作为 `MemorySuggested` 发给界面（只进「待确认」，不写文件）。
pub(crate) fn suggest_memories(inner: &Inner, session: &str) {
    let Some((model, user, reply, known)) = inner.turn_inputs(session) else {
        return;
    };
    let known: Vec<&str> = known.iter().map(String::as_str).collect();
    let no_stop = || false;
    let candidates = remember::suggest(
        &inner.provider_config,
        &model,
        session,
        inner.turn_number(session),
        &known,
        &user,
        &reply,
        &no_stop,
    );
    if !candidates.is_empty() {
        inner.emit(Some(session), KernelEvent::MemorySuggested { candidates });
    }
}

/// 用模型给会话起标题：单独发一个 max_tokens 很小的请求，只带第一条用户消息和第一条回复。
/// 成功发 `TitleGenerated`；失败 / 取消 / 内容为空都不吭声（界面保留截断版标题）。
pub(crate) fn name_session(inner: &Inner, session: &str) {
    let Some((model, first_user, first_reply)) = inner.title_inputs(session) else {
        return;
    };
    let prompt = format!(
        "为下面的对话起一个简短的标题（不超过 12 个字，概括用户想要什么，用对话的语言，\
         不加引号和句号）。只输出标题本身。\n\n用户：{}\n\n助手：{}",
        first_user, first_reply
    );
    let messages = vec![ChatMessage::new("user", prompt)];
    let no_stop = || false;
    let mut text = String::new();
    let _tag = gateway::tag(session, "title", 1);
    let outcome = inner.provider.stream_chat(&model, &messages, &[], &no_stop, &mut |event| {
        if let StreamEvent::Text(t) = event {
            text.push_str(&t);
        }
    });
    let title = text.trim().trim_matches(['"', '\'', '「', '」', '『', '』']).trim();
    let title: String = title.chars().take(24).collect();
    if outcome.is_ok() && !title.is_empty() {
        inner.emit(Some(session), KernelEvent::TitleGenerated { title });
    }
}

/// 工具循环：请求模型 → 执行工具 → 再请求，直到不再调用工具。主会话的一轮和子任务（子会话）都走这里。
/// 每一步的用量当场报给界面（`Inner::report_usage`），不攒到一轮结束。
pub(crate) fn run_steps(inner: &Inner, session: &str, flag: &Arc<AtomicBool>) -> Steps {
    let stopped = || flag.load(Ordering::SeqCst);
    // 用量网关的日志按会话、用途归类（子任务的 ID 是 `主会话/调用 ID`）
    let purpose = if subagent::split_id(session).is_some() { "subagent" } else { "turn" };
    let _tag = gateway::tag(session, purpose, inner.turn_number(session));
    let mut stop = "max_turn_requests".to_string();
    let mut reply = String::new();
    // 上一步刚压缩过（不连着压两次）/ 这一轮已经因为超限报错压缩过（只压一次）
    let mut just_compacted = false;
    let mut overflow_compacted = false;
    // 连着几步的回复被输出上限截断（中间只要有一整步正常就清零）
    let mut truncated_steps: u32 = 0;
    // 连着几步没对用户说话 / 连着几步工具调用全失败（只给主会话的 Code 会话提醒，见下）
    let mut silent_steps: u32 = 0;
    let mut failed_steps: u32 = 0;
    // 会话级档位（输入框下拉选的）按轮取：一轮中途换了档，后面的步骤仍用开头那一档
    //（工具循环中途开 / 关思考，Anthropic 会因上一条助手消息没有 thinking 块报 400）
    let Some(thinking) = inner.sessions.lock().unwrap().get(session).map(|s| s.thinking) else {
        return Steps::Stopped;
    };
    // 覆盖了档位时造的 provider，按模型缓存（一轮里一般只造一次，连接接着用）
    let mut turn_provider: Option<(String, Arc<dyn ChatProvider>)> = None;

    for _ in 0..inner.max_steps {
        // 用量快到窗口：先压缩再请求
        if !just_compacted {
            if let Some((model, window)) =
                inner.model_of(session).and_then(|m| inner.window(&m).map(|w| (m, w)))
                && inner.context_estimate(session) * 100 >= window * compact::AUTO_COMPACT_PERCENT
            {
                match inner.compact(
                    session,
                    flag,
                    &model,
                    false,
                    window * compact::KEEP_PERCENT / 100,
                    transcript_budget(Some(window)),
                ) {
                    // 压缩本身的用量也并进这一轮（上下文大小由 `Compacted` 报，这里报 0）
                    Ok(Some(u)) => inner.report_usage(session, u, 0),
                    Ok(None) => {}
                    // 压缩失败（已报给界面）就带着原样的上下文接着请求
                    Err(_) if !stopped() => {}
                    Err(_) => return Steps::Stopped,
                }
                just_compacted = true;
            }
        } else {
            just_compacted = false;
        }
        let Some(Snapshot { mut messages, mode, cwd, model, agent }) = inner.snapshot(session)
        else {
            return Steps::Stopped;
        };
        // 提醒只挂在这次请求的最后一条（用户消息或工具结果）上，不进历史
        if let Some(reminder) = inner.todo_nudge(session)
            && let Some(last) = messages.last_mut().filter(|m| m.role == "user" || m.role == "tool")
        {
            last.append_text(&reminder);
        }
        // 子会话的话用户看不到、聊天会话没有长工具链：只提醒主会话的 Code 会话。
        // 先看计数（每步都查会话状态要加锁，没必要）
        let blocked = failed_steps >= tools::FAILURE_REMIND_STEPS;
        if (blocked || silent_steps >= tools::PROGRESS_REMIND_STEPS)
            && agent.is_none()
            && inner.chat_web_of(session).is_none()
            && let Some(last) = messages.last_mut().filter(|m| m.role == "user" || m.role == "tool")
        {
            if blocked {
                last.append_text(&tools::blocker_reminder(failed_steps));
                failed_steps = 0;
                silent_steps = 0;
            } else {
                last.append_text(&tools::progress_reminder(silent_steps));
                silent_steps = 0;
            }
        }
        // 当前模型看不了图（能力表知道，或被 400 记过）：历史里的图片换成文字占位，
        // 不让整轮报错。只影响发出去的请求，日志不动（换回能看图的模型又是原图）
        if inner.model_blind_to_images(&model) {
            for m in &mut messages {
                m.replace_images_with_placeholder();
            }
        }
        let (disabled, plugin_scope) = {
            let sessions = inner.sessions.lock().unwrap();
            sessions
                .get(session)
                .map(|s| (s.mcp_disabled.clone(), s.plugins_enabled.clone()))
                .unwrap_or_default()
        };
        // 聊天会话：只有联网（开着时）和记忆，不带 MCP 工具、不带子任务
        let specs = if let Some(web) = inner.chat_web_of(session) {
            tools::chat_specs(web)
        } else {
            let loaded = inner.loaded_tools(session);
            // 延迟工具按预算挑：最近搜到 / 用过的优先，装载太多时旧工具本轮先不声明
            let last_step = inner.tool_last_step(session);
            let active = select_within_budget(&loaded, &last_step, |name| {
                inner.tool_spec_token_cost(
                    name,
                    mode != MODE_PLAN,
                    &disabled,
                    plugin_scope.as_deref(),
                )
            });
            let include = |name: &str| match &agent {
                Some(agent) => subagent::allows(agent, name),
                None => {
                    tools::exposure(name) == tools::Exposure::Direct
                        || active.iter().any(|tool| tool == name)
                }
            };
            let mut specs = tools::specs(mode != MODE_PLAN);
            if agent.is_none() {
                specs = tools::for_model_within(specs, &loaded, &active);
            } else {
                specs.retain(|spec| include(subagent::spec_name(spec)));
            }
            specs.extend(inner.mcp.specs_for(mode != MODE_PLAN, &disabled, include));
            if agent.is_none() && include(subagent::TASK) {
                specs.push(subagent::spec(&inner.agents(plugin_scope.as_deref())).into());
            }
            specs
        };
        let mut text = String::new();
        // 会话级档位覆盖档案里的那份；没覆盖时直接用构造好的 provider
        let provider: &dyn ChatProvider = match thinking {
            Some(level) => {
                if turn_provider.as_ref().is_none_or(|(m, _)| *m != model) {
                    let mut config = inner.provider_config.clone();
                    config.model = model.clone();
                    config.params.thinking = Some(level);
                    turn_provider = Some((model.clone(), make_provider(&config)));
                }
                turn_provider.as_ref().map(|(_, p)| p.as_ref()).expect("刚造好")
            }
            None => inner.provider.as_ref(),
        };
        let result = provider.stream_chat(&model, &messages, &specs, &stopped, &mut |event| {
            if stopped() {
                return;
            }
            match event {
                StreamEvent::Text(t) => {
                    text.push_str(&t);
                    inner.emit(Some(session), KernelEvent::AgentMessageChunk { text: t });
                }
                StreamEvent::Reasoning(t) => {
                    inner.emit(Some(session), KernelEvent::AgentThoughtChunk { text: t })
                }
                StreamEvent::Retrying { status, attempt, delay_ms, reason } => inner.emit(
                    Some(session),
                    KernelEvent::Retrying { status, attempt, delay_ms, reason },
                ),
            }
        });
        if stopped() {
            return Steps::Stopped;
        }
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(err) => {
                let mut message = format!("{err:#}");
                // 超出上下文：压缩一次再重发这一步
                if !overflow_compacted && claudesktop_core::is_context_overflow(&message) {
                    overflow_compacted = true;
                    let window = inner.window(&model);
                    let keep = window.unwrap_or_else(|| compact::estimate_tokens(&messages))
                        * compact::KEEP_PERCENT
                        / 100;
                    match inner.compact(
                        session,
                        flag,
                        &model,
                        false,
                        keep,
                        transcript_budget(window),
                    ) {
                        Ok(Some(u)) => {
                            inner.report_usage(session, u, 0);
                            just_compacted = true;
                            continue;
                        }
                        Ok(None) => {}
                        Err(_) if stopped() => return Steps::Stopped,
                        Err(e) => message.push_str(&format!("\n（自动压缩上下文也失败了：{e:#}）")),
                    }
                }
                return Steps::Failed(message);
            }
        };
        // 这一步的用量当场报：输入 + 输出就是此刻上下文的大小（之前各步都含在这次的输入里），
        // 界面跑着的时候就能看到上下文涨到多少，不用等整轮结束
        let mut step_tokens = 0;
        if let Some(u) = outcome.usage {
            step_tokens = u.input_tokens + u.output_tokens;
            inner.report_usage(session, u, step_tokens);
        }
        if !text.trim().is_empty() {
            reply = text.clone();
        }

        // 回复被输出上限截断（`finish_reason == "length"`）：流式解析器会把半截 JSON 尽量补全成
        // 能过 schema 的参数，`edit_file` 的 `old_string` / `write_file` 的 `content` 可能就是残缺的前半段。
        // 所以这一批调用一个都不执行（不看参数是否合法），全部回错误让模型重发——pi 的
        // `failToolCallsFromTruncatedMessage` 也是这么做的
        if outcome.finish_reason.as_deref() == Some("length") && !outcome.tool_calls.is_empty() {
            truncated_steps += 1;
            if truncated_steps >= MAX_TRUNCATED_STEPS {
                return Steps::Failed(format!(
                    "连续 {truncated_steps} 次回复被输出上限截断（finish_reason = length），已停止。"
                ));
            }
            let contents: Vec<String> = outcome
                .tool_calls
                .iter()
                .map(|c| truncated_tool_message(&c.function.name))
                .collect();
            let results: Vec<_> = outcome
                .tool_calls
                .iter()
                .zip(&contents)
                .map(|(call, content)| ChatMessage::tool_result(call.id.clone(), content.clone()))
                .collect();
            // 界面：每个调用都报一行工具，状态直接是 failed。不走 `start_tool`：截断的参数不该被
            // 解析（`Action::parse` 可能成功），也不该发 diff 预览
            for (call, content) in outcome.tool_calls.iter().zip(&contents) {
                inner.emit(
                    Some(session),
                    KernelEvent::ToolCallStarted {
                        id: call.id.clone(),
                        title: call.function.name.clone(),
                        status: "failed".into(),
                        kind: "other".into(),
                        parent: None,
                    },
                );
                inner.emit(
                    Some(session),
                    KernelEvent::ToolCallUpdated {
                        id: call.id.clone(),
                        title: None,
                        status: Some("failed".into()),
                        raw_output: Some("回复被截断，未执行".into()),
                        output: Some(claudesktop_core::output_preview(content)),
                    },
                );
            }
            // assistant 消息 + 这批错误结果照常写进历史：下一步请求里的 tool_calls 必须有对应的
            // tool 消息，否则端点直接报错
            let unmeasured = results.len();
            let mut call = ChatMessage::assistant_tool_calls(text, outcome.tool_calls);
            call.thinking = outcome.thinking;
            let mut step = Vec::with_capacity(unmeasured + 1);
            step.push(call);
            step.extend(results);
            if !inner.commit(session, flag, step) {
                return Steps::Stopped;
            }
            inner.measure(session, step_tokens, unmeasured);
            inner.count_step(session, false);
            // 继续循环，让模型下一步重发完整的调用
            continue;
        }
        // 这一步没被截断：连续计数清零
        truncated_steps = 0;
        // 这一步对用户说了话就清零；只调工具不说话就加一
        silent_steps = if text.trim().is_empty() { silent_steps + 1 } else { 0 };

        if outcome.tool_calls.is_empty() {
            if !text.is_empty() {
                inner.commit(session, flag, vec![ChatMessage::new("assistant", text)]);
            }
            inner.measure(session, step_tokens, 0);
            stop = stop_reason(outcome.finish_reason.as_deref());
            break;
        }

        // 同一步的多个调用执行完（连续的只读调用并行）；assistant 消息和全部结果一起写进历史，
        // 保证 tool_calls 后面总跟着对应的 tool 消息
        let Some(outputs) = inner.run_tools(session, &cwd, &outcome.tool_calls, &stopped) else {
            return Steps::Stopped;
        };
        // 用户在提问卡片上选了「自己说」/ 取消：结果写进历史后这一轮就结束
        let halt = outcome
            .tool_calls
            .iter()
            .zip(&outputs)
            .any(|(c, o)| c.function.name == tools::ASK_USER && o.stop_turn);
        // 「连续失败」按步数：同一步里只要有一个调用成功就清零，全部失败（用户叫停的不算）才加一。
        // 按单个调用数会把「一步里并行试探几条路径、都不存在」误判成卡住
        if outputs.iter().any(|o| !o.stop_turn) {
            failed_steps = if outputs.iter().any(|o| o.ok) { 0 } else { failed_steps + 1 };
        }
        let results: Vec<_> = outcome
            .tool_calls
            .iter()
            .zip(outputs)
            .map(|(call, output)| {
                if output.images.is_empty() {
                    ChatMessage::tool_result(call.id.clone(), output.content)
                } else {
                    ChatMessage::tool_result_with_images(
                        call.id.clone(),
                        output.content,
                        output.images,
                    )
                }
            })
            .collect();
        // 工具带回来的图片（`read_file` 读到的图、MCP 的多模态结果）能不能真跟在这一步里发出去，
        // 看模型和协议：
        // - 模型看不了图：换成文字说明（图已经在前面的 `ToolCallImages` 里给界面看过了）；
        // - OpenAI 兼容协议：`role: tool` 的 content 只能是字符串，图片另起一条 user 消息；
        // - Anthropic / Responses：本来就能带，留在工具结果里（Responses 会自己拆成 input_image）。
        let mut results = results;
        let mut image_message = images_as_user_message(
            &mut results,
            inner.provider_config.protocol,
            &model,
            inner.model_blind_to_images(&model),
        );
        let wrote_todo = outcome.tool_calls.iter().any(|c| c.function.name == tools::TODO_WRITE);
        let mut step = Vec::with_capacity(results.len() + 2);
        let unmeasured = results.len();
        let mut call = ChatMessage::assistant_tool_calls(text, outcome.tool_calls);
        // 思考块跟着工具调用回传（Anthropic 开了思考时缺了它，下一步请求会被拒）
        call.thinking = outcome.thinking;
        step.push(call);
        step.extend(results);
        step.append(&mut image_message);
        if !inner.commit(session, flag, step) {
            return Steps::Stopped;
        }
        // 工具结果还没被端点数过
        inner.measure(session, step_tokens, unmeasured);
        inner.count_step(session, wrote_todo);
        if halt {
            stop = "end_turn".into();
            break;
        }
    }
    Steps::Done { stop, reply }
}

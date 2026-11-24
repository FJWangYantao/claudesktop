//! 系统提示词的组装：分节、稳定前缀 / 易变尾巴，以及聊天与代码两种会话的正文。

use super::*;

impl SessionState {
    pub(crate) fn system_prompt(&self) -> String {
        if self.kind == SessionKind::Chat {
            return chat_system_prompt(
                self.web,
                self.restored,
                self.summary.as_deref(),
                &self.instructions,
                &self.memory_global,
            );
        }
        self.code_sections().into_iter().map(|(_, text)| text).collect::<Vec<_>>().join("\n\n")
    }

    pub(crate) fn code_sections(&self) -> Vec<(&'static str, String)> {
        let ctx = Context {
            instructions: &self.instructions,
            skills: &self.skills,
            memory_project: &self.memory_project,
            memory_global: &self.memory_global,
        };
        prompt_sections(
            &self.cwd,
            &self.mode,
            self.restored,
            self.summary.as_deref(),
            self.agent.as_ref(),
            ctx,
        )
    }

    /// 发给模型的 system 消息：稳定的那几节单独一条，模式 / 摘要 / 记忆这些易变的节另起一条。
    ///
    /// 两条消息按 `\n\n` 拼起来和 [`Self::system_prompt`] 逐字节相同；`anthropic.rs` 把每条
    /// 都当独立 block 并各打一个 `cache_control` 断点，于是压缩、切模式、记忆变化只让后一条
    /// 失效，前面的字节仍然命中提示缓存（见「方案-内核提速.md」阶段 2 B）。
    pub(crate) fn system_messages(&self) -> Vec<ChatMessage> {
        if self.kind == SessionKind::Chat {
            return vec![ChatMessage::new("system", self.system_prompt())];
        }
        prompt_messages(&self.code_sections())
            .into_iter()
            .map(|text| ChatMessage::new("system", text))
            .collect()
    }
}

/// 进系统提示词的「背景材料」：项目说明文件、技能、记忆。
///
/// 打包传（而不是一个个参数）是为了别让 `system_prompt` 的参数越加越长，
/// 以后再加一类背景材料只往这里添字段。
pub(crate) struct Context<'a> {
    pub(crate) instructions: &'a [instructions::Instruction],
    pub(crate) skills: &'a [Skill],
    pub(crate) memory_project: &'a [claudesktop_core::memory::Entry],
    pub(crate) memory_global: &'a [claudesktop_core::memory::Entry],
}

/// 系统提示词。节之间用空行连起来（每节的内容里不带前导空行）。
#[cfg(test)]
pub(crate) fn system_prompt(
    cwd: &Path,
    mode: &str,
    restored: bool,
    summary: Option<&str>,
    agent: Option<&AgentDef>,
    ctx: Context<'_>,
) -> String {
    prompt_sections(cwd, mode, restored, summary, agent, ctx)
        .into_iter()
        .map(|(_, text)| text)
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// 几乎不变的节（项目说明 / 技能）：`SessionState::system_messages` 把它们切成第一条
/// system 消息，缓存断点打在它上面；模式、摘要、记忆变化只影响后面那条。
const STABLE_SECTIONS: &[&str] = &["preamble", "agent", "restored", "project", "skills"];

/// 把分好的节切成要发的 system 消息正文：稳定的前缀一组、易变的尾部一组。
///
/// 两条按 `\n\n` 拼起来和整段提示词逐字节相同；只有「头部不变、尾部变了」这种常见情形
/// （压缩、切模式、记忆变化）才拆开，其余（没有任何易变节 / 一条节都没有）照旧一条。
pub(crate) fn prompt_messages(sections: &[(&'static str, String)]) -> Vec<String> {
    let join = |parts: &[(&'static str, String)]| {
        parts.iter().map(|(_, text)| text.as_str()).collect::<Vec<_>>().join("\n\n")
    };
    let stable_end = sections.iter().take_while(|(name, _)| STABLE_SECTIONS.contains(name)).count();
    if stable_end == 0 || stable_end == sections.len() {
        return vec![join(sections)];
    }
    vec![join(&sections[..stable_end]), join(&sections[stable_end..])]
}

/// 系统提示词按「多久变一次」分节（见「方案-内核提速.md」阶段 2 B）。
///
/// 稳定的节拼成第一条 system 消息、易变的节拼成第二条（`system_messages`）：只变了尾巴时，
/// 前面的提示缓存前缀不动。名字只用来比对，不进提示词；越靠前的节越稳定
/// （记忆 / 技能 / 摘要一变就全废，所以放后面）。
pub(crate) fn prompt_sections(
    cwd: &Path,
    mode: &str,
    restored: bool,
    summary: Option<&str>,
    agent: Option<&AgentDef>,
    ctx: Context<'_>,
) -> Vec<(&'static str, String)> {
    let Context { instructions: project, skills, memory_project, memory_global } = ctx;
    let mut sections: Vec<(&'static str, String)> = Vec::new();
    // 子会话没有待办清单（计划是主会话的），也不能再派子任务
    let todo = if agent.is_none() {
        "For tasks with several steps, track progress with todo_write and keep it current: it is what \
         the user watches while you work, so mark each item completed as soon as it is done. "
    } else {
        ""
    };
    let delegation =
        if agent.is_none() { format!("{}\n", subagent::DELEGATION) } else { String::new() };
    // 中途汇报只给主会话：子会话的话用户看不到，也不能提问（`subagent::prompt_section`），
    // 两边的指令会打架
    let progress = if agent.is_none() {
        "On longer work, keep the user in the loop with one or two plain sentences alongside your \
         next tool call when you finish a phase, when you find something that changes the plan, or \
         when you are blocked (the same kind of call keeps failing, a tool or dependency is missing, \
         the environment misbehaves). If getting unblocked would take you away from what the user \
         asked for (repairing a toolchain, installing things, long workarounds), stop and report \
         what is blocking you and the options instead of pressing on.\n"
    } else {
        ""
    };
    sections.push((
        "preamble",
        format!(
            "You are a coding agent running inside claudesktop, a desktop agent harness, \
             working on the user's machine.\n\
             Working directory: {}\n\
             Platform: {}\n\n\
             Verify a change only when you judge it risky or the user asks for it — one check, \
             not a build after every step. {todo}\
             Tool calls that change things may need the user's approval; \
             if one is denied, do not retry it.\n\
             When a decision is really the user's (which of two approaches, whether to overwrite \
             something they own, anything irreversible), call `ask_user` with 2-4 concrete options \
             instead of guessing or picking one yourself. Do not use it for anything a tool can find \
             out, and do not ask the same question twice.\n\
             {delegation}\
             {BACKGROUND}\n\
             Keep replies concise. Answer in the user's language. Use Markdown for structure and code.\n\
             When a reply runs past three paragraphs or covers more than one topic, split it with \
short headings (numbered or a few words), one topic per section, list items within a section, \
and keep headings bare (no trailing explanation).\n\
             {progress}\
             Act on the task directly: don't restate a plan, don't narrate each tool call, and don't \
             re-confirm facts you already have from the conversation or from tool results.\n\
             A short final reply is better than a thorough-looking one.",
            cwd.display(),
            std::env::consts::OS,
        ),
    ));
    if let Some(agent) = agent {
        sections.push(("agent", subagent::prompt_section(agent)));
    }
    if restored {
        sections.push((
            "restored",
            "This conversation was restored from a saved log. Earlier tool calls appear as \
             `[tool] <title> → <status>: <summary>` lines; their full inputs and outputs are not \
             available, so re-read files if you need details. Never write such lines yourself."
                .to_string(),
        ));
    }
    if let Some(section) = instructions::prompt_section(project) {
        sections.push(("project", section));
    }
    // 越稳定的越靠前：项目说明和技能一改就整段变，记忆每轮重读、摘要随压缩变，都放后面
    if !skills.is_empty() {
        let mut text = "# Skills\n\
             The user's plugins provide these skills: instructions for specific kinds of tasks. \
             When a task matches a skill's description, read its file with read_file before starting \
             and follow it; paths it mentions are relative to the skill's folder."
            .to_string();
        for skill in skills {
            text.push_str(&format!(
                "\n- {}: {} ({})",
                skill.name,
                skill.description,
                skill.path.display()
            ));
        }
        sections.push(("skills", text));
    }
    if mode == MODE_PLAN {
        let mut text = "Plan mode is active: only read-only tools are available. \
             Investigate, then present a concrete step-by-step plan for the user to approve. \
             Do not attempt to modify anything."
            .to_string();
        if agent.is_none() {
            text.push_str(
                " When helpful, consult `explore` subagents (the task tool, found with tool_search) \
                 to map parts of the codebase you have not seen yet, and build the plan on their \
                 findings.",
            );
        }
        sections.push(("plan", text));
    }
    if mode == MODE_AUTO {
        sections.push((
            "auto",
            "Automatic mode is active: a safety classifier reviews every action that changes \
             something, instead of the user approving it. It blocks actions that go beyond what the \
             user asked for and anything irreversible, and it does not see your explanations or the \
             contents of tool results, so it judges each action on its own. When it blocks a call, \
             the reason comes back in the tool result: do not retry the same thing in another form. \
             Either take a clearly safer route, or use `ask_user` to have the user decide."
                .to_string(),
        ));
    }
    if let Some(summary) = summary {
        sections.push(("summary", compact::prompt_section(summary)));
    }
    if let Some(section) = claudesktop_core::memory::prompt_section(memory_project, memory_global) {
        sections.push(("memory", section));
    }
    sections
}

/// 聊天会话的系统提示词：通用助手 + 用户的「聊天说明」+ 全局记忆。
/// 没有工作目录、平台、`BACKGROUND`、待办清单、技能、计划 / 自动模式那几段。
/// 日期按天写（一天之内提示词不变，不废提示缓存）。
pub(crate) fn chat_system_prompt(
    web: bool,
    restored: bool,
    summary: Option<&str>,
    instructions: &[instructions::Instruction],
    memory_global: &[claudesktop_core::memory::Entry],
) -> String {
    let web = if web {
        "You can search the web with web_search and read pages with web_fetch. Use them for recent \
         events, facts you are unsure of, and URLs the user gives; cite the pages you used."
    } else {
        "You have no internet access; say so when a question needs current information."
    };
    let mut prompt = format!(
        "You are a helpful assistant inside claudesktop, a desktop app. Today is {}.\n\
         Answer in the user's language. Use Markdown for structure and code. Be direct; skip filler.\n\
         Write math in LaTeX: $...$ inline and $$...$$ on its own lines for display. \
         Do not use \\( \\) or \\[ \\].\n\
         When the answer depends on a choice only the user can make, call `ask_user` with 2-4 \
         concrete options instead of guessing. Do not use it for anything you can find out yourself.\n\
         {web}",
        today_local()
    );
    if restored {
        prompt.push_str(
            "\n\nThis conversation was restored from a saved log; the earlier turns above are \
             its full text.",
        );
    }
    if let Some(section) = instructions::chat_prompt_section(instructions) {
        prompt.push_str("\n\n");
        prompt.push_str(&section);
    }
    if let Some(section) = claudesktop_core::memory::prompt_section(&[], memory_global) {
        prompt.push_str("\n\n");
        prompt.push_str(&section);
    }
    if let Some(summary) = summary {
        prompt.push_str("\n\n");
        prompt.push_str(&compact::prompt_section(summary));
    }
    prompt
}

/// 本地日期（`2026-10-22`）：按天写进聊天提示词，一天之内不变。
pub(crate) fn today_local() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

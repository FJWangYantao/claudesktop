//! 子 agent（`task` 工具）：主模型把一个自成一体的子任务交给子会话，子会话在自己的上下文里跑完，
//! 最后一条回复当工具结果交回主会话。
//!
//! - 角色：内置通用的（[`GENERAL`]）和只读调研的（[`EXPLORE`]），其余来自启用的插件的 `agents/*.md`
//!   （格式同 Claude Code；插件里有同名的就用插件的）
//! - 什么时候派子任务写在主会话的系统提示词里（[`DELEGATION`]），工具说明里只讲怎么用
//! - 子会话和主会话共用同一个工具循环（`run_steps`），只换系统提示词和工具：工具按角色的清单筛，
//!   不给 `task`（不嵌套）和 `todo_write`（计划是主会话的）
//! - 子会话 ID 是 `主会话/task 调用 ID`。它的事件经 [`forward`] 转到主会话：工具行（ID 加上 task 调用的前缀、
//!   带 `parent`，界面收进 task 那张卡片、在子任务详情页里列出来）、回复和思考（换成 `SubagentChunk`，
//!   只给详情页看）、权限请求、文件改动（检查点照常记）、重试、用量；压缩和一轮的结束是子会话自己的事，不转
//! - 权限跟主会话当前的模式和「总是允许」规则；停止主会话的这一轮，子会话一起停
//! - 同一步里的几个 `task` 一起跑（和连续的只读调用一样，见 `run_tools`）；各自的事件按 task 调用 ID
//!   归到界面上自己那个子任务（`Session::subagents`）

use claudesktop_core::KernelEvent;
use claudesktop_core::plugin::{self, AgentDef};
use serde::Deserialize;
use serde_json::{Value, json};

pub const TASK: &str = "task";
pub const GENERAL: &str = "general-purpose";
pub const EXPLORE: &str = "explore";
/// 只读调研角色能用的工具：只看不改，读文件、搜代码不用审批（联网照常按模式问）
const EXPLORE_TOOLS: [&str; 7] =
    ["read_file", "list_dir", "glob", "grep", "web_fetch", "web_search", "memory_search"];

/// 主会话系统提示词里的一段：什么时候该派子任务。不写的话模型几乎只自己搜，
/// 中间过程全堆在主上下文里。
pub const DELEGATION: &str = "Delegate with the task tool to keep your own context focused: \
    use an `explore` subagent for open-ended investigation that will take several rounds of \
    searching and reading (finding where something is implemented across the codebase, tracing how a \
    feature works, surveying usages before a refactor), and launch several in one step when the \
    questions are independent. Do it yourself when you already know the file or a single glob / grep \
    will answer it, and when you need the exact file contents in front of you to edit them.";
/// 工具行摘要最多几个字
const SUMMARY_CHARS: usize = 80;

#[derive(Deserialize)]
pub struct TaskArgs {
    #[serde(default)]
    pub description: String,
    pub prompt: String,
    #[serde(default)]
    pub subagent_type: Option<String>,
}

/// 内置的角色（插件里有同名的就用插件的），按在工具说明里的顺序。
pub fn builtin() -> [AgentDef; 2] {
    [general(), explore()]
}

/// 内置的通用角色。
pub fn general() -> AgentDef {
    AgentDef {
        name: GENERAL.into(),
        description: "General-purpose agent for researching questions, searching the codebase \
                      and carrying out multi-step tasks."
            .into(),
        tools: None,
        model: None,
        prompt: "Work through the task with the tools available. Prefer searching and reading; \
                 only change files or run commands when the task asks for it."
            .into(),
        plugin: String::new(),
    }
}

/// 内置的只读调研角色：搜、读、查网页，不改文件不跑命令，交回结论和出处。
pub fn explore() -> AgentDef {
    AgentDef {
        name: EXPLORE.into(),
        description: "Read-only research agent for open-ended questions about the codebase: finds \
                      where things are implemented, traces how a feature works across files, and \
                      surveys usages. Cannot change files or run commands. Say how thorough to be."
            .into(),
        tools: Some(EXPLORE_TOOLS.map(String::from).to_vec()),
        model: None,
        prompt: "You are read-only: search and read, never try to change anything. Start broad \
                 (glob, grep with several phrasings and naming conventions), then read the relevant \
                 parts. Issue independent searches together in one step. In the report, lead with \
                 the answer, then the evidence as `path:line` references with short excerpts; say \
                 what you looked for but did not find."
            .into(),
        plugin: String::new(),
    }
}

/// 子会话 ID → (主会话 ID, task 调用 ID)；主会话返回 None。
pub fn split_id(session: &str) -> Option<(&str, &str)> {
    session.split_once('/')
}

/// `task` 的工具定义：说明里列出可选的角色。
pub fn spec(agents: &[AgentDef]) -> Value {
    let mut description = String::from(
        "Launch a subagent to carry out a self-contained task in its own context window. Only its \
         final report comes back to you, so the searching and reading it does along the way does \
         not fill up your context. Good uses: open-ended searches across the codebase, tracing how \
         something works through several files, surveying usages, researching a question on the \
         web, or any independent piece of work you can describe completely up front. To run several \
         independent subtasks at once, call task several times in the same response: they run in \
         parallel.\n\n\
         The subagent does not see this conversation: the prompt must contain everything it needs \
         (the goal, relevant paths and what you already know, constraints, and what to report \
         back). The report is seen only by you, so relay what matters to the user. Its tool calls \
         follow the same permission rules as yours.\n\nAvailable agents:",
    );
    for agent in agents {
        description.push_str(&format!("\n- {}: {}", agent.name, agent.description));
    }
    let names: Vec<&str> = agents.iter().map(|a| a.name.as_str()).collect();
    json!({
        "type": "function",
        "function": {
            "name": TASK,
            "description": description,
            "parameters": {
                "type": "object",
                "properties": {
                    "description": { "type": "string", "description": "A short (3-5 word) label for the task, shown to the user" },
                    "prompt": { "type": "string", "description": "The full task for the subagent" },
                    "subagent_type": { "type": "string", "enum": names, "description": format!("Which agent to use (default: {GENERAL})") }
                },
                "required": ["description", "prompt"]
            }
        }
    })
}

/// 这个角色能不能用这个工具。
pub fn allows(agent: &AgentDef, tool: &str) -> bool {
    tool != TASK
        && tool != crate::tools::TODO_WRITE
        // 子会话记下的东西没有归属（算谁的会话、哪一轮都不明确），不给它
        && tool != crate::tools::MEMORY_WRITE
        && tool != crate::tools::MEMORY_FORGET
        && agent.tools.as_ref().is_none_or(|t| plugin::tool_listed(t, tool))
}

/// 工具定义的名字。
pub fn spec_name(spec: &Value) -> &str {
    spec["function"]["name"].as_str().unwrap_or_default()
}

/// 子会话系统提示词里的角色说明。
pub fn prompt_section(agent: &AgentDef) -> String {
    let mut section = format!(
        "# Subagent\n\
         You are a subagent (`{}`) started by the main agent to do one task. The user does not see \
         your messages. When you are done, reply with a complete, self-contained report of what you \
         found or did, with file paths and key details: that reply is returned to the main agent as \
         the result. You cannot ask questions and get answers; make reasonable assumptions and state them.",
        agent.name
    );
    if !agent.prompt.is_empty() {
        section.push_str("\n\n");
        section.push_str(&agent.prompt);
    }
    section
}

/// 子会话的事件 → 主会话里的事件（`call` 是 task 调用的 ID）；None = 不转。
pub fn forward(call: &str, event: KernelEvent) -> Option<KernelEvent> {
    let id = |id: String| format!("{call}/{id}");
    Some(match event {
        KernelEvent::ToolCallStarted { id: i, title, status, kind, .. } => {
            KernelEvent::ToolCallStarted {
                id: id(i),
                title,
                status,
                kind,
                parent: Some(call.to_string()),
            }
        }
        KernelEvent::ToolCallUpdated { id: i, title, status, raw_output, output } => {
            KernelEvent::ToolCallUpdated { id: id(i), title, status, raw_output, output }
        }
        KernelEvent::ToolCallProgress { id: i, output } => {
            KernelEvent::ToolCallProgress { id: id(i), output }
        }
        KernelEvent::ToolCallDiff { id: i, diff } => KernelEvent::ToolCallDiff { id: id(i), diff },
        KernelEvent::AgentMessageChunk { text } => {
            KernelEvent::SubagentChunk { task: call.to_string(), text, thought: false }
        }
        KernelEvent::AgentThoughtChunk { text } => {
            KernelEvent::SubagentChunk { task: call.to_string(), text, thought: true }
        }
        KernelEvent::PermissionRequest { id: req, title, options, tool, hunks } => {
            KernelEvent::PermissionRequest {
                id: req,
                title: format!("子任务 · {title}"),
                options,
                tool: tool.map(id),
                hunks,
            }
        }
        e @ (KernelEvent::FileChanged { .. }
        | KernelEvent::Retrying { .. }
        | KernelEvent::Notice(_)
        | KernelEvent::Status(_)
        | KernelEvent::UsageUpdated { .. }) => e,
        _ => return None,
    })
}

/// 工具行上的结果摘要（续接历史会话时主模型也只看到这个）：报告的第一行，截短。
pub fn summary(report: &str) -> String {
    let line = report
        .lines()
        .map(|l| l.trim().trim_start_matches('#').trim())
        .find(|l| !l.is_empty())
        .unwrap_or("没有结果");
    if line.chars().count() > SUMMARY_CHARS {
        format!("{}…", line.chars().take(SUMMARY_CHARS).collect::<String>())
    } else {
        line.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forwards_only_what_the_main_session_shows() {
        let started = KernelEvent::ToolCallStarted {
            id: "c1".into(),
            title: "Read a.rs".into(),
            status: "pending".into(),
            kind: "read".into(),
            parent: None,
        };
        assert_eq!(
            forward("t1", started),
            Some(KernelEvent::ToolCallStarted {
                id: "t1/c1".into(),
                title: "Read a.rs".into(),
                status: "pending".into(),
                kind: "read".into(),
                parent: Some("t1".into()),
            })
        );
        assert_eq!(
            forward("t1", KernelEvent::AgentMessageChunk { text: "x".into() }),
            Some(KernelEvent::SubagentChunk {
                task: "t1".into(),
                text: "x".into(),
                thought: false
            })
        );
        assert_eq!(forward("t1", KernelEvent::Compacting), None);
        assert_eq!(forward("t1", KernelEvent::TurnEnded { stop_reason: "end_turn".into() }), None);
    }

    #[test]
    fn tool_lists_filter_and_never_nest() {
        let mut agent = general();
        assert!(allows(&agent, "shell"));
        assert!(!allows(&agent, TASK));
        assert!(!allows(&agent, crate::tools::TODO_WRITE));
        agent.tools = Some(vec!["Read".into(), "Grep".into(), "mcp__db__query".into()]);
        assert!(allows(&agent, "read_file"));
        assert!(allows(&agent, "mcp__db__query"));
        assert!(!allows(&agent, "edit_file"));
    }

    #[test]
    fn explore_is_read_only() {
        let agent = explore();
        for tool in ["read_file", "grep", "glob", "list_dir", "web_fetch"] {
            assert!(allows(&agent, tool), "{tool}");
        }
        for tool in ["edit_file", "write_file", "shell", "shell_background", TASK] {
            assert!(!allows(&agent, tool), "{tool}");
        }
    }

    #[test]
    fn spec_lists_builtin_agents_and_when_to_use_them() {
        let spec = spec(&builtin());
        let doc = spec["function"]["description"].as_str().unwrap();
        assert!(doc.contains("- general-purpose:") && doc.contains("- explore:"), "{doc}");
        assert!(doc.contains("Good uses"), "{doc}");
        assert_eq!(
            spec["function"]["parameters"]["properties"]["subagent_type"]["enum"],
            json!([GENERAL, EXPLORE])
        );
    }

    #[test]
    fn summary_is_the_first_line() {
        assert_eq!(summary("\n## 结论\n细节"), "结论");
        assert_eq!(summary("  "), "没有结果");
        assert_eq!(summary(&"字".repeat(100)).chars().count(), SUMMARY_CHARS + 1);
    }
}

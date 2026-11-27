//! 自研内核的内置工具：读文件 / 列目录 / 写文件 / 精确替换编辑 / shell / 抓网页（见 [`crate::fetch`]），
//! 以及不碰文件的待办清单（`todo_write`，内核直接转成界面上的计划，见 [`parse_todos`]）。
//!
//! 给模型看的说明与结果用英文（与系统提示词一致），给 UI 的摘要用中文。

use crate::provider::ToolSpec;
use claudesktop_core::memory::MAX_TEXT_CHARS;
use claudesktop_core::{
    DiffHunk, DiffView, PermissionHunks, PlanEntryView, QuestionAnswer, QuestionView,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::fmt::Write as _;
use std::io::Read as _;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};

mod files;
mod safety;
mod shell;

#[cfg(test)]
use files::*;
#[cfg(test)]
use safety::*;
use shell::*;

pub(crate) use files::KeepHunks;
pub(crate) use safety::{hard_block, reads_only};
pub use shell::run_command;
pub(crate) use shell::{background_command, kill_tree, shell_rule_matches, start_background};

const READ_FILE: &str = "read_file";
const LIST_DIR: &str = "list_dir";
const WRITE_FILE: &str = "write_file";
const EDIT_FILE: &str = "edit_file";
const SHELL: &str = "shell";
const SHELL_FOLLOWUP: &str = "shell_followup";
const SHELL_BACKGROUND: &str = "shell_background";
const SHELL_OUTPUT: &str = "shell_output";
const SHELL_STOP: &str = "shell_stop";
const GLOB: &str = "glob";
const GREP: &str = "grep";
const WEB_FETCH: &str = "web_fetch";
const WEB_SEARCH: &str = "web_search";
pub const TODO_WRITE: &str = "todo_write";
pub const ASK_USER: &str = "ask_user";
pub const MEMORY_WRITE: &str = "memory_write";
pub const MEMORY_SEARCH: &str = "memory_search";
pub const MEMORY_FORGET: &str = "memory_forget";
pub const TOOL_SEARCH: &str = "tool_search";
/// `memory_search` 最多返回几条
pub const MEMORY_SEARCH_MAX: usize = 12;
/// 待办清单最多几项
const TODO_MAX: usize = 50;
/// 清单里还有没做完的项、连续这么多步没更新时，提醒模型一次。
/// 原来取 8，太密（一个五项的活至少多出五六次调用），按「方案-内核提速.md」阶段 1 提到 15。
pub const TODO_REMIND_STEPS: u32 = 15;
/// 一轮里连着这么多步（每步一次请求）都没对用户说话，提醒模型汇报一次进度。
/// 改前的数据：模型自己大约每 7 个工具说一句；10 步留出余量，只在真闷头时触发。
pub const PROGRESS_REMIND_STEPS: u32 = 10;
/// 连着这么多步的工具调用全部失败，提醒模型说明卡点、考虑停下来问用户。
/// 按步数不按调用数：一步里并行试探几条路径都失败是查证，不是卡住。
pub const FAILURE_REMIND_STEPS: u32 = 3;
/// 压缩摘要末尾附的清单的标题（再压缩时先去掉，免得旧清单被抄进新摘要）
const TODO_SECTION: &str = "\n\n## Todo list at the time of compaction\n";

/// 能不能和相邻的只读调用并行跑。
///
/// 只收**真正走 `start_tool` + `finish_tool`** 的只读工具：`todo_write` / `memory_*` 这些在
/// `Inner::run_tool` 里直接拦截的不能进来（并行分支 bypass 掉那段拦截，会当成普通工具解析）。
pub fn parallel_safe(name: &str) -> bool {
    matches!(name, READ_FILE | LIST_DIR | GLOB | GREP)
}

/// 单次读取的默认行数 / 单行最长字符数 / 最大文件
const READ_LIMIT: usize = 2000;
const LINE_MAX_CHARS: usize = 2000;
const READ_MAX_BYTES: u64 = 10 << 20;
const LIST_MAX: usize = 1000;
/// 交给模型的工具结果上限（字符），超出保留首尾
const OUTPUT_MAX_CHARS: usize = 30_000;
/// 每个管道最多收集的字节
const PIPE_MAX_BYTES: usize = 2 << 20;
/// 命令运行中多久报一次输出
const LIVE_EVERY: Duration = Duration::from_millis(500);
/// 报运行中输出时每路只解码末尾这么多字节
const LIVE_TAIL_BYTES: usize = 16 << 10;
/// 超过这个大小的文件不做 diff 预览
const PREVIEW_MAX_BYTES: u64 = 2 << 20;
const SHELL_TIMEOUT_DEFAULT: u64 = 120;
const SHELL_TIMEOUT_MAX: u64 = 600;
/// glob 最多返回的路径数
const GLOB_MAX: usize = 200;
/// grep 最多返回的匹配行数 / 文件数
const GREP_MAX_LINES: usize = 300;
const GREP_MAX_FILES: usize = 200;
/// grep 跳过的大文件
const GREP_MAX_FILE_BYTES: u64 = 2 << 20;
/// grep 输出里单行最多保留的字符
const GREP_LINE_CHARS: usize = 300;

/// 工具对权限的需求。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Read,
    Edit,
    Exec,
    /// 联网（只读，但会把网址发出去，所以要问）
    Fetch,
    /// MCP 服务器的工具：副作用不知道，一律要问；服务器标了只读的 Plan 模式下也能用
    Mcp {
        read_only: bool,
    },
}

/// 解析好的一次工具调用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Read {
        path: PathBuf,
        offset: usize,
        limit: usize,
    },
    List {
        path: PathBuf,
    },
    Write {
        path: PathBuf,
        content: String,
    },
    Edit {
        path: PathBuf,
        old: String,
        new: String,
        all: bool,
    },
    Shell {
        command: String,
        timeout: Duration,
        cwd: PathBuf,
    },
    /// 起一条后台命令，立刻返回任务 ID
    ShellBackground {
        command: String,
        cwd: PathBuf,
        on_complete: Option<String>,
    },
    ShellFollowup {
        id: String,
        instruction: String,
    },
    /// 取后台任务的增量输出（最多等一会儿）
    ShellOutput {
        id: String,
        wait_secs: u64,
    },
    /// 结束后台任务
    ShellStop {
        id: String,
    },
    /// 按文件名模式找文件（遵守 .gitignore）
    Glob {
        pattern: String,
        root: PathBuf,
        cwd: PathBuf,
    },
    /// 按正则搜文件内容（遵守 .gitignore）
    Grep {
        pattern: String,
        root: PathBuf,
        include: Option<String>,
        ignore_case: bool,
        files_only: bool,
        cwd: PathBuf,
    },
    /// 抓网页；`offset` 是转换后文本的字符偏移
    Fetch {
        url: String,
        offset: usize,
        /// 只许访问公网（聊天会话联网免问，不能让网页里的注入指使模型去碰本机 / 局域网）
        public_only: bool,
    },
    /// 搜网页（Brave 有 key 走 Brave，否则 DuckDuckGo 兜底，见 `websearch` 模块）
    Search {
        query: String,
    },
    /// MCP 服务器的工具（由内核交给 `McpHub::call` 执行，不走 [`Action::run`]）
    Mcp {
        server: String,
        tool: String,
        arguments: Value,
        read_only: bool,
    },
    /// 派子 agent 跑一个子任务（由内核开子会话执行，不走 [`Action::run`]，见 `subagent` 模块）
    Task {
        description: String,
        prompt: String,
        /// 角色名；None = 通用
        agent: Option<String>,
    },
}

/// 工具结果里带回的图片（base64，跟 `ContentPart::Image` 同形状）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputImage {
    /// 如 `image/png`
    pub media_type: String,
    /// 图片字节（base64 编码）
    pub data: String,
}

/// 工具执行结果：`content` 回给模型，`summary` 显示在 UI 工具行；
/// `images` 是 MCP 服务器返回的图片（多模态工具结果），一并进消息历史。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutput {
    pub ok: bool,
    pub content: String,
    pub summary: String,
    pub images: Vec<OutputImage>,
    /// 这一轮到此为止：工具结果已经写好，不要再请求模型（用户选了「自己说」/ 取消提问）
    pub stop_turn: bool,
    /// 这批工具结果提交后，释放会话等待后台通知。
    pub wait_background: bool,
}

impl ToolOutput {
    pub fn ok(content: impl Into<String>, summary: impl Into<String>) -> Self {
        Self {
            ok: true,
            wait_background: false,
            content: content.into(),
            summary: summary.into(),
            images: Vec::new(),
            stop_turn: false,
        }
    }

    pub fn fail(content: impl Into<String>, summary: impl Into<String>) -> Self {
        Self {
            ok: false,
            wait_background: false,
            content: content.into(),
            summary: summary.into(),
            images: Vec::new(),
            stop_turn: false,
        }
    }

    /// 同 [`ToolOutput::fail`]，并让这一轮停在这里（用户不再等模型继续说）。
    pub fn fail_stop(content: impl Into<String>, summary: impl Into<String>) -> Self {
        Self { stop_turn: true, ..Self::fail(content, summary) }
    }

    /// 带图片的成功结果（`read_file` 读到图片、MCP 的多模态结果）。
    /// 图片跟着工具结果一起进下一次请求；模型 / 协议带不了时由内核降级成文字说明。
    pub fn ok_with_images(
        content: impl Into<String>,
        summary: impl Into<String>,
        images: Vec<OutputImage>,
    ) -> Self {
        Self { images, ..Self::ok(content, summary) }
    }
}

/// 工具对模型的暴露档位（「方案-内核提速.md」阶段 2）。
///
/// `Direct` 每次请求都声明；`Deferred` 不声明，模型得先用 `tool_search` 把它找出来，
/// 找出来之后这一次会话往后的请求都带上它。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Exposure {
    Direct,
    Deferred,
}

/// 哪些工具默认不声明给模型，要先用 `tool_search` 找出来（「方案-内核提速.md」阶段 2）。
///
/// 常驻的是每次都要用的那几个（读写文件、改文件、shell、搜代码、待办清单、提问、搜工具）；
/// 这里是不常用、但真要用时必须能找回来的。`task` 来自 `subagent`，名字直接写字符串。
const DEFERRED: &[&str] = &[
    LIST_DIR,
    WEB_FETCH,
    WEB_SEARCH,
    MEMORY_SEARCH,
    MEMORY_WRITE,
    MEMORY_FORGET,
    // 后台命令的配套工具按组加载。
    SHELL_BACKGROUND,
    SHELL_OUTPUT,
    SHELL_STOP,
    SHELL_FOLLOWUP,
    "task",
];

/// 工具名 → 暴露档位。改暴露档位就改 [`DEFERRED`]，别在调用处散着写。
/// MCP 工具一律 `Deferred`（名字带 `mcp__` 前缀，见 [`crate::mcp::is_mcp_tool`]）：
/// 一个服务器挂几个工具事先不知道，全常驻比内置工具还贵。
pub fn exposure(name: &str) -> Exposure {
    if DEFERRED.contains(&name) || crate::mcp::is_mcp_tool(name) {
        Exposure::Deferred
    } else {
        Exposure::Direct
    }
}

/// 按 exposure 筛掉这次请求不该声明的工具定义。`loaded` 是本会话已经通过 `tool_search`
/// 加载的名字（`SessionState::loaded_tools`）。
pub fn for_model(specs: Vec<ToolSpec>, loaded: &[String]) -> Vec<ToolSpec> {
    if specs.iter().all(|spec| exposure(spec_name(spec)) == Exposure::Direct) {
        return specs;
    }
    specs
        .into_iter()
        .filter(|spec| {
            let name = spec_name(spec);
            exposure(name) == Exposure::Direct || loaded.iter().any(|loaded| loaded == name)
        })
        .collect()
}

/// 和 [`for_model`] 一样，但延迟工具只放预算内的那些（`active` 由 `select_within_budget` 给）。
/// 预算外的工具本轮不声明，模型要用时再搜一次就回来了。
pub fn for_model_within(
    specs: Vec<ToolSpec>,
    loaded: &[String],
    active: &[String],
) -> Vec<ToolSpec> {
    if active.len() == loaded.len() {
        return for_model(specs, loaded);
    }
    specs
        .into_iter()
        .filter(|spec| {
            let name = spec_name(spec);
            exposure(name) == Exposure::Direct || active.iter().any(|tool| tool == name)
        })
        .collect()
}

/// 一份工具定义里的名字（`{"function": {"name": ..}}`）。
fn spec_name(spec: &Value) -> &str {
    spec["function"]["name"].as_str().unwrap_or_default()
}

/// 发给 provider 的工具定义（OpenAI `tools`）；`writable = false`（Plan 模式）只给只读工具。
pub fn specs(writable: bool) -> Vec<ToolSpec> {
    static SPECS: [OnceLock<Vec<ToolSpec>>; 2] = [const { OnceLock::new() }; 2];
    SPECS[writable as usize]
        .get_or_init(|| build_specs(writable).into_iter().map(ToolSpec::from).collect())
        .clone()
}

fn build_specs(writable: bool) -> Vec<Value> {
    let f = |name: &str, description: String, parameters: Value| {
        json!({
            "type": "function",
            "function": { "name": name, "description": description, "parameters": parameters },
        })
    };
    let mut tools = vec![
        f(
            READ_FILE,
            format!(
                "Read a file. Text files: returns lines prefixed with their 1-based line number and a tab, \
                 up to {READ_LIMIT} lines by default (use offset/limit for large files). \
                 Images (png / jpeg / gif / webp / bmp): the image is attached to the conversation so you \
                 can look at it, with a one-line note giving its size; offset/limit do not apply. \
                 Use this to look at screenshots, mockups and diagrams. \
                 Relative paths resolve against the working directory."
            ),
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path" },
                    "offset": { "type": "integer", "description": "1-based line to start from" },
                    "limit": { "type": "integer", "description": "Number of lines to read" }
                },
                "required": ["path"]
            }),
        ),
        f(
            LIST_DIR,
            "List the entries of a directory (directories end with '/'). Defaults to the working directory.".into(),
            json!({
                "type": "object",
                "properties": { "path": { "type": "string", "description": "Directory path" } }
            }),
        ),
        f(
            GLOB,
            format!(
                "Find files by name pattern, e.g. `**/*.rs` or `src/**/mod.rs`. Respects .gitignore and skips hidden files. \
                 Returns paths relative to the working directory, most recently modified first (at most {GLOB_MAX})."
            ),
            json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Glob pattern, matched against paths relative to `path`" },
                    "path": { "type": "string", "description": "Directory to search in (default: working directory)" }
                },
                "required": ["pattern"]
            }),
        ),
        f(
            TODO_WRITE,
            "Create or update the task list for the current work; the user sees it as a checklist. \
             Use it only for work that is clearly multi-step and will take several tool-calling rounds \
             (a migration, a feature across files, a long investigation); skip it for anything you can \
             finish in one or two steps. Write the whole list up front, then call it again when the \
             picture changes. Mark items completed as soon as they are done — batching several \
             completions into one call is fine — and keep at most one item in_progress. Add items you \
             discover along the way and remove ones that no longer apply. \
             Always send the complete list (it replaces the previous one); an empty list clears it. \
             Before your final reply, make sure the list matches what was actually done."
                .into(),
            json!({
                "type": "object",
                "properties": {
                    "todos": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "content": { "type": "string", "description": "What to do, in the user's language" },
                                "status": { "type": "string", "enum": ["pending", "in_progress", "completed"] }
                            },
                            "required": ["content", "status"]
                        }
                    }
                },
                "required": ["todos"]
            }),
        ),
        f(
            ASK_USER,
            ASK_USER_DESCRIPTION.into(),
            ask_user_params(),
        ),
        f(
            GREP,
            format!(
                "Search file contents with a regular expression (Rust regex syntax). Respects .gitignore, skips hidden and binary files. \
                 Returns `path:line: text` for each matching line (at most {GREP_MAX_LINES}), or only the matching file paths \
                 when files_only is true. Prefer this over running grep/rg/findstr in the shell."
            ),
            json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Regular expression" },
                    "path": { "type": "string", "description": "File or directory to search (default: working directory)" },
                    "include": { "type": "string", "description": "Only search files matching this glob, e.g. `*.rs`" },
                    "ignore_case": { "type": "boolean" },
                    "files_only": { "type": "boolean", "description": "Return only paths of matching files" }
                },
                "required": ["pattern"]
            }),
        ),
        f(
            WEB_FETCH,
            format!(
                "Fetch a web page (http/https) and return its text: HTML is converted to plain text with \
                 headings, lists, code blocks and [text](absolute-url) links; JSON and other text are returned as is. \
                 Scripts are not run. Returns at most {} characters; use offset to read further. \
                 Use it for documentation, issues or articles the user points to; the user may be asked to approve the domain.",
                crate::fetch::FETCH_PAGE_CHARS
            ),
            json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "Full URL, e.g. https://docs.rs/serde" },
                    "offset": { "type": "integer", "description": "Character offset into the converted text (for long pages)" }
                },
                "required": ["url"]
            }),
        ),
        f(
            WEB_SEARCH,
            format!(
                "Search the web and return at most {} results (title, URL, snippet). \
                 Use it to find documentation, solutions or current information, then web_fetch the most \
                 promising URL for details. The user may be asked to approve the search.",
                crate::websearch::SEARCH_MAX_RESULTS
            ),
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Search query" }
                },
                "required": ["query"]
            }),
        ),
        f(
            MEMORY_SEARCH,
            "Search the notes saved for later sessions (user preferences, project conventions, \
             decisions, pitfalls) and return the matching ones. Only the notes that fit the budget \
             are in the system prompt; use this to look up the rest. Plain words, not a regex; \
             case-insensitive. Read-only."
                .to_string(),
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Words to look for" },
                    "scope": {
                        "type": "string",
                        "enum": ["project", "global", "all"],
                        "description": "all (default): this project and every project"
                    }
                },
                "required": ["query"]
            }),
        ),
    ];
    if writable {
        // 记忆会写文件（项目记忆在项目目录里），Plan 模式只读，不给
        tools.push(f(
            MEMORY_WRITE,
            format!(
                "Save one short note for later sessions: a user preference, a project convention, \
                 a decision with its reason, or a pitfall you hit. `text` is one sentence (at most \
                 {MAX_TEXT_CHARS} characters), not a transcript and not \"the user asked me to …\". \
                 Do not record things the repository already says (layout, dependency versions, \
                 function signatures: read the files instead), one-off task details, or secrets \
                 (tokens, passwords, keys). If the user asks you to remember something, save it \
                 here instead of repeating the conversation. Use the `memory_write` note only when \
                 it will matter in later sessions; rules that belong in the repository go into \
                 AGENTS.md with edit_file."
            ),
            json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string", "description": "One sentence, with the why when it matters" },
                    "tags": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "A few short categories, e.g. 构建 / 测试 / 偏好"
                    },
                    "scope": {
                        "type": "string",
                        "enum": ["project", "global"],
                        "description": "project (default): this working directory; global: every project"
                    },
                    "pinned": { "type": "boolean", "description": "Keep it in context and never evict it" }
                },
                "required": ["text"]
            }),
        ));
        // 删记忆是写文件，和 memory_write 一样在 Plan 模式下不给
        tools.push(f(
            MEMORY_FORGET,
            "Delete a note saved earlier. Use it when a note is wrong, outdated, or the user says \
             to forget it. You can only delete notes this same session saved (the user can delete \
             any of them in the settings); if the note came from an earlier session, tell the user \
             where to delete it instead of guessing."
                .to_string(),
            json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Words from the note (or its text), enough to pick it out"
                    },
                    "scope": {
                        "type": "string",
                        "enum": ["project", "global", "all"],
                        "description": "all (default): this project and every project"
                    }
                },
                "required": ["query"]
            }),
        ));
        tools.push(f(
            WRITE_FILE,
            "Create or overwrite a file with the given content. Parent directories are created. \
             Prefer edit_file for changing existing files."
                .into(),
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string", "description": "Full file content" }
                },
                "required": ["path", "content"]
            }),
        ));
        tools.push(f(
            EDIT_FILE,
            "Replace an exact string in a file. old_string must match exactly (including whitespace) \
             and be unique in the file unless replace_all is true; include surrounding lines to make it unique. \
             Read the file first."
                .into(),
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "old_string": { "type": "string" },
                    "new_string": { "type": "string" },
                    "replace_all": { "type": "boolean", "description": "Replace every occurrence" }
                },
                "required": ["path", "old_string", "new_string"]
            }),
        ));
        tools.push(f(
            SHELL,
            format!(
                "Run a command in {} with the working directory as cwd. stdin is closed; \
                 do not run interactive programs. Timeout defaults to {SHELL_TIMEOUT_DEFAULT}s (max {SHELL_TIMEOUT_MAX}s). \
                 For long jobs, find shell_background with tool_search and set on_complete to resume when done. Returns stdout, stderr and the exit code.{}",
                shell_name(),
                DETACH_HINT,
            ),
            json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" },
                    "timeout_secs": { "type": "integer" }
                },
                "required": ["command"]
            }),
        ));
        tools.push(f(
            SHELL_BACKGROUND,
            format!(
                "Start a long-running command ({}) in the background and return immediately with a \
                 task id. Use it for dev servers, watchers and anything that does not finish \
                 quickly; read what it printed with `{SHELL_OUTPUT}`, stop it with `{SHELL_STOP}`. \
                 In a main session set on_complete for a finite job when its result is needed: this ends the current turn and wakes you automatically. Omit for servers or fire-and-forget. The process is killed when the application exits.{}",
                shell_name(),
                DETACH_HINT,
            ),
            json!({
                "type": "object",
                "properties": { "command": { "type": "string" }, "on_complete": { "type": "string", "description": "What to do once the command exits. Later user directions take priority." } },
                "required": ["command"]
            }),
        ));
        tools.push(f(
            SHELL_OUTPUT,
            format!(
                "Read the new output of a background command started with `{SHELL_BACKGROUND}`. \
                 Only what was printed since the last read is returned, plus whether it is still \
                 running or its exit code. `wait_secs` (default 5, max {}) waits that long for new \
                 output before answering; raise it to follow a build instead of polling.",
                crate::bgtasks::WAIT_MAX_SECS,
            ),
            json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Task id from shell_background" },
                    "wait_secs": { "type": "integer" }
                },
                "required": ["id"]
            }),
        ));
        tools.push(f(
            SHELL_FOLLOWUP,
            "Change the pending follow-up of a background job in this session. Set instruction to an empty string to cancel the callback and leave the process running.".into(),
            json!({
                "type": "object",
                "properties": { "id": { "type": "string" }, "instruction": { "type": "string" } },
                "required": ["id", "instruction"]
            }),
        ));
        tools.push(f(
            SHELL_STOP,
            "Stop a background command (and everything it started). \
             Use it when a dev server is no longer needed or a test run hangs."
                .to_string(),
            json!({
                "type": "object",
                "properties": { "id": { "type": "string" } },
                "required": ["id"]
            }),
        ));
    }
    // `tool_search` 在两种模式下都给（它不碰文件）：Plan 模式也得能把 `web_fetch` 这类找回来
    tools.push(f(
        TOOL_SEARCH,
        "Some of your tools are not listed here (memory, subagents, background \
         commands, web access, MCP servers, and so on). Describe in one sentence what you want to do \
         and this finds them by name and description; they become callable from your NEXT request \
         onwards, not in this one. So: search first, finish this request, and call the tool you found \
         in the next round."
            .to_string(),
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "What you want to do, e.g. `remember this` or `run a command in the background`"
                }
            },
            "required": ["query"]
        }),
    ));
    tools
}

/// 聊天会话的工具定义：只有联网（`web` 开着时）和记忆（只全局）。
/// 不给工作目录那套：聊天改不了文件、跑不了命令。
pub fn chat_specs(web: bool) -> Vec<ToolSpec> {
    static SPECS: [OnceLock<Vec<ToolSpec>>; 2] = [const { OnceLock::new() }; 2];
    SPECS[web as usize]
        .get_or_init(|| build_chat_specs(web).into_iter().map(ToolSpec::from).collect())
        .clone()
}

fn build_chat_specs(web: bool) -> Vec<Value> {
    let f = |name: &str, description: String, parameters: Value| {
        json!({
            "type": "function",
            "function": { "name": name, "description": description, "parameters": parameters },
        })
    };
    let mut tools = vec![f(ASK_USER, ASK_USER_DESCRIPTION.into(), ask_user_params())];
    if web {
        tools.push(f(
            WEB_FETCH,
            format!(
                "Fetch a web page (http/https) and return its text: HTML is converted to plain text with \
                 headings, lists, code blocks and [text](absolute-url) links; JSON and other text are returned as is. \
                 Scripts are not run. Returns at most {} characters; use offset to read further.",
                crate::fetch::FETCH_PAGE_CHARS
            ),
            json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "Full URL, e.g. https://docs.rs/serde" },
                    "offset": { "type": "integer", "description": "Character offset into the converted text (for long pages)" }
                },
                "required": ["url"]
            }),
        ));
        tools.push(f(
            WEB_SEARCH,
            format!(
                "Search the web and return at most {} results (title, URL, snippet). \
                 Use it to find current information, then web_fetch the most promising URL for details.",
                crate::websearch::SEARCH_MAX_RESULTS
            ),
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Search query" }
                },
                "required": ["query"]
            }),
        ));
    }
    tools.push(f(
        MEMORY_SEARCH,
        "Search the notes saved for later sessions (user preferences, decisions, pitfalls) and \
         return the matching ones. Only the notes that fit the budget are in the system prompt; \
         use this to look up the rest. Plain words, not a regex; case-insensitive. Read-only."
            .to_string(),
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Words to look for" }
            },
            "required": ["query"]
        }),
    ));
    // 聊天会话没有工作目录：记忆一律记到全局，所以不带 `scope` 参数
    tools.push(f(
        MEMORY_WRITE,
        format!(
            "Save one short note for later sessions: a user preference, a decision with its reason, \
             or a pitfall you hit. `text` is one sentence (at most {MAX_TEXT_CHARS} characters), not \
             a transcript and not \"the user asked me to …\". Do not record secrets (tokens, \
             passwords, keys) or one-off task details. If the user asks you to remember something, \
             save it here instead of repeating the conversation."
        ),
        json!({
            "type": "object",
            "properties": {
                "text": { "type": "string", "description": "One sentence, with the why when it matters" },
                "tags": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "A few short categories, e.g. 偏好 / 习惯"
                },
                "pinned": { "type": "boolean", "description": "Keep it in context and never evict it" }
            },
            "required": ["text"]
        }),
    ));
    tools.push(f(
        MEMORY_FORGET,
        "Delete a note saved earlier. Use it when a note is wrong, outdated, or the user says to \
         forget it. You can only delete notes this same session saved (the user can delete any of \
         them in the settings); if the note came from an earlier session, tell the user where to \
         delete it instead of guessing."
            .to_string(),
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Words from the note (or its text), enough to pick it out"
                }
            },
            "required": ["query"]
        }),
    ));
    tools
}

/// 这个工具名在聊天会话里能不能用（只靠「不给定义」不够：有的端点不校验工具名，
/// 模型也可能照着续接前的 `[tool] …` 文字瞎编）。
pub fn chat_allows(web: bool, name: &str) -> bool {
    match name {
        WEB_FETCH | WEB_SEARCH => web,
        MEMORY_WRITE | MEMORY_SEARCH | MEMORY_FORGET | ASK_USER => true,
        _ => false,
    }
}

#[derive(Deserialize)]
struct FetchArgs {
    url: String,
    #[serde(default)]
    offset: Option<usize>,
}

#[derive(Deserialize)]
struct SearchArgs {
    query: String,
}

#[derive(Deserialize)]
struct ShellOutputArgs {
    id: String,
    #[serde(default)]
    wait_secs: Option<u64>,
}

/// `ask_user` 的说明（Code / 聊天会话共用）。
const ASK_USER_DESCRIPTION: &str = "Ask the user to choose between options when you need a decision you should not make \
     yourself (which approach, which file, whether to run something risky). Blocks this turn until they answer. \
     Do not use it to confirm a plan or to ask something you can find out yourself with a tool.";

/// `ask_user` 的参数定义（Code / 聊天会话共用）。
fn ask_user_params() -> Value {
    json!({
        "type": "object",
        "properties": {
            "questions": {
                "type": "array", "minItems": 1, "maxItems": 3,
                "items": {
                    "type": "object",
                    "properties": {
                        "question": { "type": "string", "description": "One sentence, shown as the card title" },
                        "header": { "type": "string", "description": "At most 12 characters, for the summary line" },
                        "options": {
                            "type": "array", "minItems": 2, "maxItems": 4,
                            "items": {
                                "type": "object",
                                "properties": {
                                    "label": { "type": "string", "description": "One line, the button text" },
                                    "description": { "type": "string", "description": "Grey line under the button: what this choice means" }
                                },
                                "required": ["label"]
                            }
                        }
                    },
                    "required": ["question", "options"]
                }
            }
        },
        "required": ["questions"]
    })
}

/// 一次最多问几题（再多不如分两次问，卡片也一屏放不下）。
pub const ASK_USER_MAX_QUESTIONS: usize = 3;
/// 每题最多几个选项（和权限卡的数字键位数对齐）。
pub const ASK_USER_MAX_OPTIONS: usize = 4;

#[derive(Deserialize)]
struct AskUserArgs {
    questions: Vec<AskUserQuestion>,
}

#[derive(Deserialize)]
struct AskUserQuestion {
    question: String,
    #[serde(default)]
    header: Option<String>,
    options: Vec<AskUserOption>,
}

#[derive(Deserialize)]
struct AskUserOption {
    label: String,
    #[serde(default)]
    description: Option<String>,
}

/// 解析 `ask_user` 的参数；错误信息指明哪个字段（直接回给模型）。
pub fn parse_questions(arguments: &str) -> Result<Vec<QuestionView>, String> {
    let args: AskUserArgs = serde_json::from_str(arguments)
        .map_err(|e| format!("Invalid arguments for {ASK_USER}: {e}"))?;
    if args.questions.is_empty() {
        return Err(format!("{ASK_USER}: at least one question is required"));
    }
    if args.questions.len() > ASK_USER_MAX_QUESTIONS {
        return Err(format!("{ASK_USER}: at most {ASK_USER_MAX_QUESTIONS} questions per call"));
    }
    let mut out = Vec::with_capacity(args.questions.len());
    for (i, q) in args.questions.into_iter().enumerate() {
        let n = i + 1;
        if q.question.trim().is_empty() {
            return Err(format!("{ASK_USER}: question {n} has no text"));
        }
        if q.options.len() < 2 {
            return Err(format!("{ASK_USER}: question {n} needs at least 2 options"));
        }
        if q.options.len() > ASK_USER_MAX_OPTIONS {
            return Err(format!(
                "{ASK_USER}: question {n} has more than {ASK_USER_MAX_OPTIONS} options"
            ));
        }
        let mut options = Vec::with_capacity(q.options.len());
        for opt in q.options {
            if opt.label.trim().is_empty() {
                return Err(format!("{ASK_USER}: question {n} has an option with no label"));
            }
            options.push(claudesktop_core::QuestionOptionView {
                label: opt.label.trim().to_string(),
                description: opt
                    .description
                    .map(|d| d.trim().to_string())
                    .filter(|d| !d.is_empty()),
                value: None,
            });
        }
        out.push(QuestionView {
            question: q.question.trim().to_string(),
            header: q.header.map(|h| h.trim().to_string()).filter(|h| !h.is_empty()),
            options,
            ..Default::default()
        });
    }
    Ok(out)
}

/// 用户答完回给模型的内容和卡片摘要（中文）。
pub fn question_result(questions: &[QuestionView], answer: &QuestionAnswer) -> (String, String) {
    match answer {
        QuestionAnswer::Choices { choices } => {
            let mut lines = vec!["The user answered:".to_string()];
            let mut picked = Vec::new();
            for (i, q) in questions.iter().enumerate() {
                let Some(label) = choices.get(i).and_then(|n| q.options.get(*n)) else {
                    continue;
                };
                lines.push(format!("- {} → {}", q.question, label.label));
                picked.push(format!("{} → {}", q.question, label.label));
            }
            if picked.is_empty() {
                return (
                    "The user did not pick an option. Ask again only if it really matters, otherwise \
                     make a reasonable assumption and say so."
                        .into(),
                    "问了一下（没选）".into(),
                );
            }
            (lines.join("\n"), format!("问了一下：{}", picked.join("；")))
        }
        QuestionAnswer::Form { values } => {
            let mut lines = vec!["The user answered:".to_string()];
            let mut picked = Vec::new();
            for answer in values {
                let text = match &answer.value {
                    claudesktop_core::FormValueKind::Text(text) => text.clone(),
                    claudesktop_core::FormValueKind::Number(number) => number.to_string(),
                    claudesktop_core::FormValueKind::Integer(number) => number.to_string(),
                    claudesktop_core::FormValueKind::Boolean(value) => value.to_string(),
                    claudesktop_core::FormValueKind::Strings(values) => values.join(", "),
                };
                lines.push(format!("- {} → {}", answer.id, text));
                picked.push(format!("{} → {}", answer.id, text));
            }
            (lines.join("\n"), format!("问了一下：{}", picked.join("；")))
        }
        QuestionAnswer::SelfAnswer => (
            "The user will answer in their next message. Stop here and ask nothing else.".into(),
            "问了一下（等用户回答）".into(),
        ),
        QuestionAnswer::ModelDecides => (
            "The user let you decide. Pick the option you think is best, say which one and why in one \
             line, and continue."
                .into(),
            "问了一下（让模型自己定）".into(),
        ),
        QuestionAnswer::Cancelled => (
            "Cancelled by the user. Do not retry; wait for their next message.".into(),
            "问了一下（未回答）".into(),
        ),
    }
}

/// 清单里还有没做完的项。
pub fn todos_open(entries: &[PlanEntryView]) -> bool {
    entries.iter().any(|e| e.status != "completed")
}

/// 清单一项一行（提醒 / 压缩摘要用）。
fn todo_lines(entries: &[PlanEntryView]) -> String {
    todo_marked(entries).join("\n")
}

/// 好几步没更新清单时挂在请求末尾的提醒（只进这一次请求，不进历史）。
pub fn todo_reminder(entries: &[PlanEntryView]) -> String {
    format!(
        "\n\n<system-reminder>\nThe todo list has not been updated for a while. Current list:\n{}\n\
         If you finished items, mark them completed now with {TODO_WRITE}; if the plan changed, rewrite \
         the list; if it no longer applies, clear it. Do not mention this reminder to the user.\n\
         </system-reminder>",
        todo_lines(entries)
    )
}

/// 连着好几步没对用户说话时挂在请求末尾（只进这一次请求，不进历史）。
pub fn progress_reminder(steps: u32) -> String {
    format!(
        "\n\n<progress-reminder>\nYou have worked {steps} steps without telling the user anything. \
         Along with your next tool call, add one or two plain sentences: what is done, what you \
         found, what comes next. Do not mention this reminder.\n</progress-reminder>"
    )
}

/// 连着好几步的工具调用都没成功时挂在请求末尾。
pub fn blocker_reminder(steps: u32) -> String {
    format!(
        "\n\n<progress-reminder>\nThe tool calls of the last {steps} steps all failed. Tell the user in \
         a sentence what is blocking you. If fixing it is outside what they asked for, stop and ask \
         them how to proceed (ask_user) instead of trying more workarounds. Do not mention this \
         reminder.\n</progress-reminder>"
    )
}

/// 压缩摘要：去掉上次附上的清单，再附上当前的（还有没做完的项才附）。
/// 清单由内核附，不靠总结的模型抄：抄的会过时，附的和界面上的一致。
pub fn summary_with_todos(summary: &str, entries: &[PlanEntryView]) -> String {
    let body = summary_without_todos(summary);
    if todos_open(entries) {
        format!("{body}{TODO_SECTION}{}", todo_lines(entries))
    } else {
        body.to_string()
    }
}

/// 摘要去掉附上的清单（交给模型再总结前用）。
pub fn summary_without_todos(summary: &str) -> &str {
    summary.split_once(TODO_SECTION).map_or(summary, |(body, _)| body)
}

#[derive(Deserialize)]
struct TodoArgs {
    todos: Vec<TodoItem>,
}

#[derive(Deserialize)]
struct TodoItem {
    content: String,
    #[serde(default)]
    status: String,
}

/// `todo_write` 的参数 → 计划条目（错误信息回给模型）。认不出的状态按 pending。
pub fn parse_todos(arguments: &str) -> Result<Vec<PlanEntryView>, String> {
    let args: TodoArgs = serde_json::from_str(arguments)
        .map_err(|e| format!("Invalid arguments for {TODO_WRITE}: {e}"))?;
    if args.todos.len() > TODO_MAX {
        return Err(format!("{TODO_WRITE}: at most {TODO_MAX} items"));
    }
    Ok(args
        .todos
        .into_iter()
        .filter(|t| !t.content.trim().is_empty())
        .map(|t| PlanEntryView {
            content: t.content.trim().to_string(),
            priority: "medium".into(),
            status: match t.status.as_str() {
                "in_progress" | "completed" => t.status,
                _ => "pending".into(),
            },
        })
        .collect())
}

/// 待办清单的一行摘要（日志重建上下文 / 回给模型的确认用）：`[x] 已完成; [>] 进行中; [ ] 待做`。
pub fn todo_summary(entries: &[PlanEntryView]) -> String {
    todo_marked(entries).join("; ")
}

/// 每项带上状态记号：`[x]` 已完成、`[>]` 进行中、`[ ]` 待做。
fn todo_marked(entries: &[PlanEntryView]) -> Vec<String> {
    entries
        .iter()
        .map(|e| {
            let mark = match e.status.as_str() {
                "completed" => "[x]",
                "in_progress" => "[>]",
                _ => "[ ]",
            };
            format!("{mark} {}", e.content)
        })
        .collect()
}

#[derive(Deserialize)]
struct MemoryArgs {
    text: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    pinned: bool,
}

/// `memory_write` 的参数 → (记到哪个范围，一条记忆)；错误信息回给模型。
pub fn parse_memory(
    arguments: &str,
) -> Result<(claudesktop_core::memory::Scope, claudesktop_core::memory::Entry), String> {
    use claudesktop_core::memory::{self, Scope};
    let args: MemoryArgs = serde_json::from_str(arguments)
        .map_err(|e| format!("Invalid arguments for {MEMORY_WRITE}: {e}"))?;
    let text = args.text.trim();
    if text.is_empty() {
        return Err(format!("{MEMORY_WRITE}: `text` is empty"));
    }
    if args.tags.len() > memory::MAX_TAGS {
        return Err(format!("{MEMORY_WRITE}: at most {} tags", memory::MAX_TAGS));
    }
    let scope = match args.scope.as_deref() {
        None | Some("") => Scope::Project,
        other => Scope::parse(other.unwrap_or_default())
            .ok_or_else(|| format!("{MEMORY_WRITE}: `scope` must be \"project\" or \"global\""))?,
    };
    let mut entry = memory::Entry::new(text, now_ms());
    entry.tags = args
        .tags
        .into_iter()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .take(memory::MAX_TAGS)
        .collect();
    entry.pinned = args.pinned;
    Ok((scope, entry))
}

#[derive(Deserialize)]
struct MemoryQueryArgs {
    query: String,
    #[serde(default)]
    scope: Option<String>,
}

/// 记忆工具的作用范围：`all`（默认，两份都算）/ `project` / `global`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryWhere {
    All,
    Only(claudesktop_core::memory::Scope),
}

impl MemoryWhere {
    /// 这个范围要不要读某一份。
    pub fn includes(self, scope: claudesktop_core::memory::Scope) -> bool {
        match self {
            MemoryWhere::All => true,
            MemoryWhere::Only(s) => s == scope,
        }
    }
}

/// `memory_search` / `memory_forget` 的参数 → (查什么, 查哪些范围)；错误信息回给模型。
pub fn parse_memory_query(tool: &str, arguments: &str) -> Result<(String, MemoryWhere), String> {
    use claudesktop_core::memory::Scope;
    let args: MemoryQueryArgs = serde_json::from_str(arguments)
        .map_err(|e| format!("Invalid arguments for {tool}: {e}"))?;
    let query = args.query.trim().to_string();
    if query.is_empty() {
        return Err(format!("{tool}: `query` is empty"));
    }
    let where_ = match args.scope.as_deref().map(str::trim) {
        None | Some("") | Some("all") => MemoryWhere::All,
        Some(other) => MemoryWhere::Only(Scope::parse(other).ok_or_else(|| {
            format!("{tool}: `scope` must be \"project\", \"global\" or \"all\"")
        })?),
    };
    Ok((query, where_))
}

/// `tool_search` 一次最多给几个（再多不如换个说法）。
pub const SEARCH_MAX: usize = 8;

/// `tool_search` 的参数 → 查询串；空 / 缺参数报错（错误信息直接回给模型）。
pub fn parse_tool_search(arguments: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Args {
        #[serde(default)]
        query: String,
    }
    let args: Args = serde_json::from_str(arguments)
        .map_err(|e| format!("Invalid arguments for {TOOL_SEARCH}: {e}"))?;
    let query = args.query.trim().to_string();
    if query.is_empty() {
        return Err(format!(
            "{TOOL_SEARCH}: `query` is empty — describe in one sentence what you want to do"
        ));
    }
    Ok(query)
}

/// `tool_search` 的排序：`candidates` 是 (名字, 说明)，按 query 分词后在名字和说明里打分
/// （命中名字 +5、命中说明 +1，大小写不敏感，按非字母数字分词），按分数降序取前
/// [`SEARCH_MAX`] 个；一条都不命中返回空 vec（调用方回一句「换个说法」）。
///
/// 按词比，不按子串：`a`、`in` 这类子串几乎哪条说明里都有，按子串算会让每个候选都得分，
/// 一次把 8 个不相干的工具永久加载进会话。query 里的虚词（[`SEARCH_STOP_WORDS`]）和单个字母不算；
/// 前缀也算命中（`command` 对上 `commands`，`todos` 对上 `todo`）。非 ASCII 的词（中文）没有空格
/// 可分，照旧按子串比。
///
/// 语料就是工具定义本身（名字 + description），不另维护一份描述。
pub fn search(query: &str, candidates: &[(String, String)]) -> Vec<String> {
    let words: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|word| !(word.is_ascii() && word.len() < 2))
        .filter(|word| !SEARCH_STOP_WORDS.contains(&word.as_str()))
        .collect();
    let mut scored: Vec<(usize, &str)> = Vec::new();
    for (name, description) in candidates {
        let lower_name = name.to_lowercase();
        let lower_description = description.to_lowercase();
        let mut score = 0usize;
        for word in &words {
            if word_hits(word, &lower_name) {
                score += 5;
            }
            if word_hits(word, &lower_description) {
                score += 1;
            }
        }
        if score > 0 {
            scored.push((score, name.as_str()));
        }
    }
    // 稳定排序：同分时保持候选顺序（= specs 里的顺序）
    scored.sort_by_key(|&(score, _)| std::cmp::Reverse(score));
    let mut names: Vec<String> =
        scored.into_iter().take(SEARCH_MAX).map(|(_, name)| name.to_string()).collect();
    // 后台命令的配套工具是一套：搜到其中一个就一起给。只拿到 `shell_background`
    // 的模型不知道怎么读输出，等于没给。它们同属一套，所以不占 [`SEARCH_MAX`] 的名额。
    const SHELL_FAMILY: [&str; 4] = [SHELL_BACKGROUND, SHELL_OUTPUT, SHELL_STOP, SHELL_FOLLOWUP];
    if names.iter().any(|name| SHELL_FAMILY.contains(&name.as_str())) {
        for name in SHELL_FAMILY {
            if !names.iter().any(|found| found == name) {
                names.push(name.to_string());
            }
        }
    }
    names
}

/// `tool_search` 里不算数的词：哪条说明里都有，算进去只会给每个候选加分。
const SEARCH_STOP_WORDS: &[&str] = &[
    "an", "the", "in", "on", "of", "to", "for", "and", "or", "with", "from", "by", "at", "into",
    "is", "are", "be", "it", "its", "my", "me", "do", "can", "how", "what", "want", "need", "some",
    "this", "that", "use", "using", "tool", "tools",
];

/// `word` 在 `text`（已转小写）里算不算命中：ASCII 词按词比（前缀也算，见 [`search`]），
/// 非 ASCII 的按子串比。
fn word_hits(word: &str, text: &str) -> bool {
    if !word.is_ascii() {
        return text.contains(word);
    }
    text.split(|c: char| !c.is_alphanumeric())
        .any(|token| token.starts_with(word) || (token.len() >= 3 && word.starts_with(token)))
}

fn now_ms() -> u64 {
    claudesktop_core::memory::now_ms()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 按 magic bytes 认图片：扩展名不可靠，内容说了算；认不出的按文本 / 二进制走。
    #[test]
    fn image_media_type_goes_by_magic_bytes() {
        assert_eq!(
            image_media_type(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0]),
            Some("image/png")
        );
        assert_eq!(image_media_type(&[0xff, 0xd8, 0xff, 0xe0]), Some("image/jpeg"));
        assert_eq!(image_media_type(b"GIF89a....."), Some("image/gif"));
        assert_eq!(image_media_type(b"GIF87a....."), Some("image/gif"));
        assert_eq!(image_media_type(b"RIFF$\x00\x00\x00WEBPVP8 "), Some("image/webp"));
        assert_eq!(image_media_type(b"BM6\x00\x00\x00"), Some("image/bmp"));
        // 不是图片：文本、空文件、只认一半的魔数
        assert_eq!(image_media_type(b"fn main() {}"), None);
        assert_eq!(image_media_type(b""), None);
        assert_eq!(image_media_type(b"RIFF$\x00\x00\x00WAVE"), None);
        assert_eq!(image_media_type(&[0x89, b'P', b'N']), None);
    }

    /// 输出比管道缓冲大也不卡；超时连同后台的子孙一起结束（不等子孙退出才返回）。
    #[cfg(unix)]
    #[test]
    fn run_command_drains_output_and_kills_the_group() {
        let dir = std::env::temp_dir();
        let out =
            run_command("head -c 300000 /dev/zero | tr '\\0' x", &dir, Duration::from_secs(20));
        assert!(out.ok, "{}", out.summary);
        let started = Instant::now();
        let out = run_command("sleep 30 & sleep 30", &dir, Duration::from_secs(1));
        assert!(!out.ok);
        assert_eq!(out.summary, "超时");
        assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
    }

    /// 跑命令默认不弹窗口：没设过的环境变量带上，用户自己设了的不覆盖。
    #[cfg(not(windows))]
    #[test]
    fn shell_commands_run_quietly_by_default() {
        if std::env::var_os("MPLBACKEND").is_some() {
            return;
        }
        let out = shell_command("echo \"$MPLBACKEND $PLAYWRIGHT_HTML_OPEN\"").output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "Agg never");
    }

    fn tmp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("claudesktop-tools-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn parse_resolves_paths_and_titles() {
        let cwd = Path::new(if cfg!(windows) { r"C:\proj" } else { "/proj" });
        let a = Action::parse(READ_FILE, r#"{"path":"src/../src/main.rs"}"#, cwd).unwrap();
        assert_eq!(a.title(cwd), "Read src/main.rs");
        assert_eq!(a.access(), Access::Read);
        let a = Action::parse(LIST_DIR, "", cwd).unwrap();
        assert_eq!(a.title(cwd), "List .");
        let a = Action::parse(SHELL, r#"{"command":"cargo test -p x"}"#, cwd).unwrap();
        assert_eq!(a.title(cwd), "`cargo test -p x`");
        assert_eq!(a.rule(cwd).as_deref(), Some("shell:cargo test"));
        assert!(Action::parse(WRITE_FILE, r#"{"path":"a"}"#, cwd).unwrap_err().contains("content"));
        assert!(Action::parse("nope", "{}", cwd).unwrap_err().contains("Unknown tool"));
        let a = Action::parse(WEB_FETCH, r#"{"url":"Docs.rs/serde"}"#, cwd).unwrap();
        assert_eq!(a.title(cwd), "Fetch https://Docs.rs/serde");
        assert_eq!((a.access(), a.kind()), (Access::Fetch, "fetch"));
        assert_eq!(a.rule(cwd).as_deref(), Some("fetch:docs.rs"));
        assert!(Action::parse(WEB_FETCH, r#"{"url":"file:///etc/passwd"}"#, cwd).is_err());
        // 工作目录外的写入：不提供「总是允许」
        let outside =
            Action::parse(WRITE_FILE, r#"{"path":"../other/x","content":""}"#, cwd).unwrap();
        assert!(!outside.inside(cwd));
        assert_eq!(outside.rule(cwd), None);
    }

    #[test]
    fn glob_and_grep_respect_gitignore() {
        let dir = tmp("search");
        let w = |rel: &str, text: &[u8]| {
            let p = dir.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        };
        w(".gitignore", b"target/\n");
        w("src/main.rs", b"fn main() {\n    println!(\"Hello\");\n}\n");
        w("src/app/mod.rs", b"pub fn hello() {}\n");
        w("README.md", b"hello world\n");
        w("target/debug/gen.rs", b"fn hello_generated() {}\n");
        w("data.bin", b"hello\0binary");
        let never = || false;
        let run = |name: &str, args: &str| {
            let a = Action::parse(name, args, &dir).unwrap();
            assert_eq!(a.access(), Access::Read);
            assert_eq!(a.kind(), "search");
            a.run(&never, &|_| {}, None)
        };

        // 不含 / 的模式按文件名匹配任意层级；target/ 被 .gitignore 排除
        let out = run(GLOB, r#"{"pattern":"*.rs"}"#);
        let mut found: Vec<&str> = out.content.lines().collect();
        found.sort();
        assert_eq!(found, vec!["src/app/mod.rs", "src/main.rs"]);
        assert_eq!(out.summary, "2 个文件");
        let out = run(GLOB, r#"{"pattern":"src/*.rs"}"#);
        assert_eq!(out.content.trim(), "src/main.rs");
        assert_eq!(run(GLOB, r#"{"pattern":"*.py"}"#).summary, "0 个文件");

        // 内容搜索：大小写、include、files_only；二进制和忽略目录不搜
        let out = run(GREP, r#"{"pattern":"hello"}"#);
        let mut found: Vec<&str> = out.content.lines().collect();
        found.sort();
        assert_eq!(found, vec!["README.md:1: hello world", "src/app/mod.rs:1: pub fn hello() {}"]);
        let out = run(GREP, r#"{"pattern":"hello","ignore_case":true,"include":"*.rs"}"#);
        let mut found: Vec<&str> = out.content.lines().collect();
        found.sort();
        assert_eq!(
            found,
            vec!["src/app/mod.rs:1: pub fn hello() {}", "src/main.rs:2:     println!(\"Hello\");"]
        );
        assert_eq!(out.summary, "2 处匹配");
        let out = run(GREP, r#"{"pattern":"fn \\w+","files_only":true,"path":"src"}"#);
        assert_eq!(out.summary, "2 个文件");
        assert!(out.content.lines().all(|l| l.starts_with("src/")));
        assert_eq!(run(GREP, r#"{"pattern":"nothing-here"}"#).summary, "无匹配");

        // 参数错误直接回给模型
        assert!(
            Action::parse(GREP, r#"{"pattern":"("}"#, &dir).unwrap_err().contains("invalid regex")
        );
        assert!(Action::parse(GLOB, r#"{"pattern":""}"#, &dir).is_err());
        let a =
            Action::parse(GREP, r#"{"pattern":"x","path":"src","include":"*.rs"}"#, &dir).unwrap();
        assert_eq!(a.title(&dir), "Grep x in src (*.rs)");
        // Plan 模式（只读工具）里也有这两个
        let names: Vec<String> = specs(false)
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap().to_string())
            .collect();
        assert!(names.contains(&"glob".to_string()) && names.contains(&"grep".to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn summary_carries_only_the_current_open_todo_list() {
        let entries = parse_todos(
            r#"{"todos":[{"content":"读代码","status":"completed"},{"content":"改; 测","status":"in_progress"}]}"#,
        )
        .unwrap();
        let summary = summary_with_todos("## 摘要\n做到一半", &entries);
        assert!(summary.starts_with("## 摘要\n做到一半"), "{summary}");
        assert!(summary.ends_with("[x] 读代码\n[>] 改; 测"), "{summary}");
        // 再压缩：旧清单先去掉，只留新的一份
        let again = summary_with_todos(&summary, &entries);
        assert_eq!(again, summary);
        assert_eq!(summary_without_todos(&summary), "## 摘要\n做到一半");
        // 都做完了 / 没有清单：不附
        let done = parse_todos(r#"{"todos":[{"content":"读代码","status":"completed"}]}"#).unwrap();
        assert!(!todos_open(&done));
        assert_eq!(summary_with_todos(&summary, &done), "## 摘要\n做到一半");
        assert_eq!(summary_with_todos("摘要", &[]), "摘要");
        let reminder = todo_reminder(&entries);
        assert!(reminder.contains("<system-reminder>") && reminder.contains("[>] 改; 测"));
    }

    #[test]
    fn todos_become_plan_entries() {
        let entries = parse_todos(
            r#"{"todos":[{"content":"读代码","status":"completed"},{"content":" 改 ","status":"in_progress"},{"content":"测","status":"x"},{"content":"  ","status":"pending"}]}"#,
        )
        .unwrap();
        let got: Vec<(&str, &str)> =
            entries.iter().map(|e| (e.content.as_str(), e.status.as_str())).collect();
        assert_eq!(got, [("读代码", "completed"), ("改", "in_progress"), ("测", "pending")]);
        assert_eq!(todo_summary(&entries), "[x] 读代码; [>] 改; [ ] 测");
        assert!(parse_todos(r#"{"items":[]}"#).unwrap_err().contains("todo_write"));
        // Plan 模式也能用
        assert!(specs(false).iter().any(|t| t["function"]["name"] == TODO_WRITE));
    }

    #[test]
    fn ask_user_parses_and_validates() {
        let ok = parse_questions(
            r#"{"questions":[{"question":"走哪条？","options":[{"label":"重写"},{"label":"保留","description":"还有引用"}]}]}"#,
        )
        .unwrap();
        assert_eq!(ok.len(), 1);
        assert_eq!(ok[0].options.len(), 2);
        assert_eq!(ok[0].options[1].description.as_deref(), Some("还有引用"));

        let err = |json: &str| parse_questions(json).unwrap_err();
        // 没题 / 题太多
        assert!(err(r#"{"questions":[]}"#).contains("at least one"));
        assert!(err(
            r#"{"questions":[{"question":"a","options":[{"label":"x"},{"label":"y"}]},{"question":"b","options":[{"label":"x"},{"label":"y"}]},{"question":"c","options":[{"label":"x"},{"label":"y"}]},{"question":"d","options":[{"label":"x"},{"label":"y"}]}]}"#
        )
        .contains("at most 3"));
        // 选项太少 / 太多
        assert!(
            err(r#"{"questions":[{"question":"a","options":[{"label":"x"}]}]}"#)
                .contains("at least 2")
        );
        assert!(err(
            r#"{"questions":[{"question":"a","options":[{"label":"1"},{"label":"2"},{"label":"3"},{"label":"4"},{"label":"5"}]}]}"#
        )
        .contains("more than 4"));
        // 空 label / 空问题
        assert!(
            err(r#"{"questions":[{"question":"a","options":[{"label":" "},{"label":"y"}]}]}"#)
                .contains("no label")
        );
        assert!(
            err(r#"{"questions":[{"question":"  ","options":[{"label":"x"},{"label":"y"}]}]}"#)
                .contains("no text")
        );
        // Plan 模式、聊天会话都给
        assert!(specs(false).iter().any(|t| t["function"]["name"] == ASK_USER));
        assert!(chat_specs(false).iter().any(|t| t["function"]["name"] == ASK_USER));
    }

    #[test]
    fn question_result_lists_the_picked_labels() {
        let questions = parse_questions(
            r#"{"questions":[{"question":"走哪条？","options":[{"label":"重写"},{"label":"保留"}]},{"question":"删掉吗？","options":[{"label":"删"},{"label":"留"}]}]}"#,
        )
        .unwrap();
        let (content, summary) =
            question_result(&questions, &QuestionAnswer::Choices { choices: vec![0, 1] });
        assert!(content.contains("走哪条？ → 重写"));
        assert!(content.contains("删掉吗？ → 留"));
        assert_eq!(summary, "问了一下：走哪条？ → 重写；删掉吗？ → 留");
        let (content, summary) = question_result(&questions, &QuestionAnswer::Cancelled);
        assert!(content.contains("Cancelled by the user"));
        assert!(summary.contains("未回答"));
    }

    #[test]
    fn shell_rules() {
        assert_eq!(shell_rule("git status -s").as_deref(), Some("git status"));
        assert_eq!(shell_rule("ls -la").as_deref(), Some("ls"));
        assert_eq!(shell_rule("python a.py").as_deref(), Some("python"));
        assert_eq!(shell_rule("git status && rm -rf x"), None);
        assert!(shell_rule_matches("git status", "git status --short"));
        assert!(!shell_rule_matches("git status", "git push"));
        assert!(!shell_rule_matches("git status", "git status; rm x"));
        assert!(!shell_rule_matches("cargo", "cargo test > out.txt"));
    }

    #[test]
    fn hard_block_stops_critical_removals() {
        let cwd = if cfg!(windows) { Path::new("C:\\work\\proj") } else { Path::new("/work/proj") };
        let shell = |command: &str| {
            hard_block(
                &Action::Shell {
                    command: command.into(),
                    timeout: Duration::from_secs(1),
                    cwd: cwd.to_path_buf(),
                },
                cwd,
            )
        };

        // 文件系统根、盘符根、家目录、工作目录本身及父级：一律拦下
        for bad in [
            "rm -rf /",
            "rm -rf /*",
            "rm -rf ~",
            "rm -rf ~/",
            "rm -rf ..",
            "rm -r -f .",
            "rm -rf /work",
            "rm -rf /work/proj",
            "sudo rm -rf /work/proj/src/../../..",
            "echo hi && rm -rf /work",
        ] {
            assert!(shell(bad).is_some(), "应拦下：{bad}");
        }
        // 盘符路径只在 Windows 上是绝对路径
        if cfg!(windows) {
            for bad in ["Remove-Item -Recurse -Force C:\\", "rd /s /q C:\\Windows"] {
                assert!(shell(bad).is_some(), "应拦下：{bad}");
            }
        }

        // 项目里的具体目录、单文件删除、非删除命令：放行给分类器判
        for ok in [
            "rm -rf target",
            "rm -rf ./build/tmp",
            "rm -rf src/generated/*.rs",
            "rm a.txt",
            "git status",
            "cargo test",
            "rm -rf $BUILD_DIR",
        ] {
            assert!(shell(ok).is_none(), "不该拦下：{ok}");
        }
        // 写受保护的路径：不交给分类器
        let write = |path: &str| {
            hard_block(
                &Action::parse(WRITE_FILE, &json!({"path": path, "content": "x"}).to_string(), cwd)
                    .unwrap(),
                cwd,
            )
        };
        assert!(write(".env").is_some());
        assert!(write(".env.local").is_some());
        assert!(write(".git/config").is_some());
        assert!(write("scripts/.gitconfig").is_some());
        assert!(write("src/main.rs").is_none());
        assert!(write("docs/environment.md").is_none());
        // 项目级配置（含权限规则）：改了就能绕过分类器
        assert!(write(".claudesktop/settings.json").is_some());

        // 本程序的数据目录（设置、权限规则、mcp.json、插件）整个受保护，不分大小写
        let data = cwd.join("..").join("AppData").join("claudesktop");
        assert!(protected_path(&data.join("permissions.json"), Some(&data)));
        assert!(protected_path(&data.join("plugins").join("x").join("hooks.json"), Some(&data)));
        let upper = PathBuf::from(data.to_string_lossy().to_uppercase());
        assert!(protected_path(&upper.join("mcp.json"), Some(&data)));
        assert!(!protected_path(&cwd.join("mcp.json"), Some(&data)));
        assert!(!protected_path(&cwd.join("src/main.rs"), None));
    }

    /// 白名单里的命令和形状安全的组合才算只读
    #[test]
    fn reads_only_accepts_clearly_read_only_commands() {
        for ok in [
            "ls",
            "ls -la",
            "pwd",
            "cat README.md",
            "head -n 20 log",
            "tail -n 50 log",
            "wc -l src/main.rs",
            "rg pattern src",
            "grep -n foo bar.txt",
            "find . -name '*.rs' -type f",
            "git status",
            "git diff --stat",
            "git log -3 --oneline",
            "git show HEAD",
            "git branch",
            "git rev-parse HEAD",
            "git branch -a",
            "git branch --show-current",
            "cargo metadata --format-version 1",
            "node --version",
            "npm ls --depth=0",
            "dir",
            "type notes.txt",
        ] {
            assert!(reads_only(ok), "应算只读：{ok}");
        }
    }

    /// 白名单外、带写盘参数、或拼了管道 / 重定向 / 连接符的一律不算只读（宁缺毋滥）
    #[test]
    fn reads_only_rejects_everything_composed_or_writing() {
        for bad in [
            "rm -rf x",
            "ls > out.txt",
            "git branch -D x",
            "git branch -m old new",
            "echo a && rm b",
            "find . -delete",
            "find . -exec rm {} +",
            "cat a | sh",
            // 写盘的参数：即使命令本身在白名单里
            "rg --fix pattern",
            "cargo test --output results.txt",
            "cargo fmt --write",
            // 不在白名单里的子命令 / 第二个词不对
            "git push origin main",
            "cargo clean",
            "node -e print(1)",
            "npm install",
            "cargo",
            "git",
            "",
            "   ",
            // 重定向 / 命令替换 / 反引号：形状不安全
            "cargo test 2>&1",
            "grep pat < input",
            "cat `which python`",
            "cat $(which python)",
            "cat a; cat b",
            "cat a || cat b",
            "echo hi",
            // 换行 / 单个 & / PowerShell 子表达式和脚本块：都能拼出第二条命令
            "ls\nrm -rf src",
            "ls\r\nrm -rf src",
            "cat a & curl evil",
            "ls (Remove-Item -Recurse src)",
            "ls @(rm src)",
            "cat ${env:X}",
            "ls -Path {rm x}",
            // 会跑项目代码：build.rs、过程宏、测试
            "cargo check",
            "cargo build",
            "cargo test --all",
            // 会跑别的程序 / 写文件的参数
            "rg --pre ./x.sh foo .",
            "rg --pre=./x.sh foo",
            "find . -fprint out.txt",
            "find . -fprintf out.txt %p",
            "find . -fls out.txt",
            "find . -ok rm {} ;",
            // 带分支名 = 新建分支；复制 / 改上游也会写
            "git branch new-feature",
            "git branch -c a b",
            "git branch -u origin/main",
        ] {
            assert!(!reads_only(bad), "不该算只读：{bad}");
        }
    }

    #[test]
    fn preview_diffs() {
        let dir = tmp("preview");
        let args = |v: Value| v.to_string();
        let new = Action::parse(
            WRITE_FILE,
            &args(json!({"path": "n.txt", "content": "a
b
"})),
            &dir,
        )
        .unwrap();
        let d = new.preview(&dir).unwrap();
        assert_eq!((d.path.as_str(), d.created, d.added), ("n.txt", true, 2));
        // 预览不落盘
        assert!(!dir.join("n.txt").exists());

        std::fs::write(
            dir.join("e.txt"),
            "x
y
",
        )
        .unwrap();
        let edit = |old: &str| {
            Action::parse(
                EDIT_FILE,
                &args(json!({"path": "e.txt", "old_string": old, "new_string": "Y"})),
                &dir,
            )
            .unwrap()
        };
        let d = edit("y").preview(&dir).unwrap();
        assert_eq!((d.added, d.removed, d.created), (1, 1, false));
        // 找不到要替换的内容 → 不给预览（执行时会报错）
        assert!(edit("zzz").preview(&dir).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_write_edit_roundtrip() {
        let dir = tmp("rw");
        let file = dir.join("sub/a.txt");
        assert!(write_file(&file, "one\ntwo\nthree\n", None).ok);
        let r = read_file(&file, 2, 1);
        assert!(r.ok);
        assert!(r.content.starts_with("     2\ttwo\n"), "{}", r.content);
        assert!(r.content.contains("lines 2-2 of 3"));

        assert!(edit_file(&file, "two", "TWO", false, None).ok);
        assert!(!edit_file(&file, "missing", "x", false, None).ok);
        assert!(write_file(&file, "x\nx\n", None).ok);
        let dup = edit_file(&file, "x", "y", false, None);
        assert!(!dup.ok && dup.content.contains("2 times"));
        assert!(edit_file(&file, "x", "y", true, None).ok);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "y\ny\n");

        // CRLF 文件 + LF 的 old_string
        std::fs::write(&file, "a\r\nb\r\nc\r\n").unwrap();
        assert!(edit_file(&file, "a\nb", "A\nB", false, None).ok);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "A\r\nB\r\nc\r\n");

        let l = list_dir(&dir);
        assert_eq!(l.content.trim(), "sub/");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn shell_runs_and_reports_exit_code() {
        let dir = tmp("sh");
        let ok = run_shell("echo hello", &dir, Duration::from_secs(60), &|| false, &|_| {});
        assert!(ok.ok, "{ok:?}");
        assert_eq!(ok.summary, "hello");
        let bad = run_shell("exit 3", &dir, Duration::from_secs(60), &|| false, &|_| {});
        assert!(!bad.ok);
        assert!(bad.content.contains("[exit code: 3]"), "{bad:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn shell_stops_on_cancel() {
        let dir = tmp("cancel");
        let started = Instant::now();
        let cmd = if cfg!(windows) { "Start-Sleep 30" } else { "sleep 30" };
        let out = run_shell(
            cmd,
            &dir,
            Duration::from_secs(60),
            &|| started.elapsed() > Duration::from_millis(300),
            &|_| {},
        );
        assert!(!out.ok && out.content.contains("cancelled"), "{out:?}");
        assert!(started.elapsed() < Duration::from_secs(10));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn shell_reports_output_while_running() {
        let dir = tmp("live");
        let cmd = if cfg!(windows) {
            "Write-Output first; Start-Sleep -Milliseconds 1500; Write-Output second"
        } else {
            "echo first; sleep 1.5; echo second"
        };
        let seen = Mutex::new(Vec::new());
        let out = run_shell(cmd, &dir, Duration::from_secs(60), &|| false, &|p| {
            seen.lock().unwrap().push(p)
        });
        assert!(out.ok, "{out:?}");
        let seen = seen.into_inner().unwrap();
        // 跑着的时候报过只有第一行的预览，同样的内容不重复报
        assert!(seen.iter().any(|p| p == "first"), "{seen:?}");
        assert_eq!(seen.iter().filter(|p| *p == "first").count(), 1, "{seen:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 按块写盘：只写放行的块，其余块保持文件原来的内容；模型看的结果和摘要里都说清楚写了几块。
    #[test]
    fn partial_write_keeps_unselected_hunks() {
        let dir = tmp("partial");
        let file = dir.join("a.txt");
        std::fs::write(&file, "a\nb\nc\nd\ne\n").unwrap();
        // 改两处（第 0 块把 b 改成 B，第 1 块把 e 改成 E）：只放行第 1 块
        let full = Action::Write { path: file.clone(), content: "a\nB\nc\nd\nE\n".into() };
        let keep = |k: &[u32]| KeepHunks::new(k.to_vec(), &full.permission_hunks(&dir).unwrap());
        let hunks = full.permission_hunks(&dir).expect("能按块同意");
        assert_eq!(hunks.files.len(), 1);
        assert_eq!(hunks.files[0].hunks.len(), 2, "{:?}", hunks.files[0].hunks);
        assert!(!hunks.files[0].whole_file);
        let out = full.run(&|| false, &|_| {}, Some(&keep(&[1])));
        assert!(out.ok, "{out:?}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "a\nb\nc\nd\nE\n");
        assert!(out.summary.contains("只写了 1 块"), "{}", out.summary);
        // 模型看的结果里也要有：不然它以为整份都写下去了
        assert!(out.content.contains("只写了 1 块，另有 1 块没写"), "{}", out.content);
        // 全部放行 = 整份写，不再提「只写了」
        std::fs::write(&file, "a\nb\nc\nd\ne\n").unwrap();
        let out = full.run(&|| false, &|_| {}, Some(&keep(&[0, 1, 1, 7])));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "a\nB\nc\nd\nE\n");
        assert!(!out.content.contains("只写了"), "重复、越界的编号不算数：{}", out.content);
        // 一块不放：文件保持现在的内容
        std::fs::write(&file, "a\nb\nc\nd\ne\n").unwrap();
        full.run(&|| false, &|_| {}, Some(&keep(&[])));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "a\nb\nc\nd\ne\n");
        // 改文件同样按块，模型看的结果里写明没写的块
        let edit = Action::Edit {
            path: file.clone(),
            old: "b\nc\nd\ne".into(),
            new: "B\nc\nd\nE".into(),
            all: false,
        };
        let asked = edit.permission_hunks(&dir).expect("改文件也能按块同意");
        assert_eq!(asked.files[0].hunks.len(), 2);
        let out = edit.run(&|| false, &|_| {}, Some(&KeepHunks::new(vec![0], &asked)));
        assert!(out.ok, "{out:?}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "a\nB\nc\nd\ne\n");
        assert!(out.content.contains("只写了 1 块"), "{}", out.content);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 问过之后文件又被改了（并行的子任务写了同一个文件）：块的编号指的已经是别的改动，整次不写。
    #[test]
    fn partial_write_refuses_when_file_changed_after_asking() {
        let dir = tmp("partial-stale");
        let file = dir.join("a.txt");
        std::fs::write(&file, "a\nb\nc\nd\ne\n").unwrap();
        let full = Action::Write { path: file.clone(), content: "a\nB\nc\nd\nE\n".into() };
        let keep = KeepHunks::new(vec![1], &full.permission_hunks(&dir).unwrap());
        // 用户勾选的时候，另一边把文件改了
        std::fs::write(&file, "x\na\nb\nc\nd\ne\n").unwrap();
        let out = full.run(&|| false, &|_| {}, Some(&keep));
        assert!(!out.ok, "{out:?}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "x\na\nb\nc\nd\ne\n", "一个字都不写");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 很大的改动（生成的大文件）不给按块勾：块要整份进权限事件和会话日志。
    #[test]
    fn huge_writes_are_not_offered_per_hunk() {
        let dir = tmp("partial-huge");
        let big: String =
            (0..PERMISSION_HUNK_MAX_LINES + 50).map(|i| format!("line {i}\n")).collect();
        let write = Action::Write { path: dir.join("gen.txt"), content: big };
        assert!(write.permission_hunks(&dir).is_none());
        let small = Action::Write { path: dir.join("small.txt"), content: "hi\n".into() };
        assert!(small.permission_hunks(&dir).is_some_and(|h| h.files[0].whole_file));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 记忆工具的参数：`scope` 的三种写法、空查询、写错的 scope
    #[test]
    fn memory_query_args() {
        use claudesktop_core::memory::Scope;
        let (query, only) = parse_memory_query(MEMORY_SEARCH, r#"{"query":" 构建 "}"#).unwrap();
        assert_eq!(query, "构建");
        assert_eq!(only, MemoryWhere::All);
        let (_, only) =
            parse_memory_query(MEMORY_SEARCH, r#"{"query":"x","scope":"global"}"#).unwrap();
        assert!(only.includes(Scope::Global) && !only.includes(Scope::Project));
        let (_, only) =
            parse_memory_query(MEMORY_FORGET, r#"{"query":"x","scope":"all"}"#).unwrap();
        assert!(only.includes(Scope::Global) && only.includes(Scope::Project));
        assert!(parse_memory_query(MEMORY_SEARCH, r#"{"query":"  "}"#).is_err());
        assert!(parse_memory_query(MEMORY_SEARCH, r#"{"query":"x","scope":"both"}"#).is_err());
        assert!(parse_memory_query(MEMORY_SEARCH, "{}").is_err());
    }

    /// `memory_write` 的参数：默认项目、标签截到上限、没写 scope 就是项目
    #[test]
    fn memory_write_args() {
        use claudesktop_core::memory::{self, Scope};
        let (scope, entry) =
            parse_memory(r#"{"text":"回复用中文","tags":["偏好","语言"]}"#).unwrap();
        assert_eq!(scope, Scope::Project);
        assert_eq!(entry.text, "回复用中文");
        assert_eq!(entry.tags, ["偏好", "语言"]);
        let (scope, _) = parse_memory(r#"{"text":"x","scope":"global"}"#).unwrap();
        assert_eq!(scope, Scope::Global);
        assert!(parse_memory(r#"{"text":"  "}"#).is_err());
        assert!(parse_memory(r#"{"text":"x","scope":"both"}"#).is_err());
        let many: Vec<String> = (0..memory::MAX_TAGS + 3).map(|i| format!("t{i}")).collect();
        // 标签超上限：报错让模型自己挑（不悄悄截掉，截掉的可能正是要紧的那个）
        let args = serde_json::json!({"text": "x", "tags": many}).to_string();
        assert!(parse_memory(&args).is_err());
    }

    /// `tool_search` 的候选：(名字, 说明)。
    fn candidates() -> Vec<(String, String)> {
        [
            ("todo_write", "Create or update the task list"),
            ("memory_search", "Search the notes saved for later sessions"),
            ("web_fetch", "Fetch a web page"),
            ("web_search", "Search the web"),
            ("list_dir", "List the entries of a directory"),
            ("shell_background", "Start a command in the background"),
            ("shell_output", "Read the new output of a background command"),
            ("shell_stop", "Stop a background command"),
            ("shell_followup", "Change a background follow-up"),
            ("task", "Launch a subagent"),
        ]
        .iter()
        .map(|(name, description)| (name.to_string(), description.to_string()))
        .collect()
    }

    /// 命中名字（+5）排在只命中说明（+1）的前面。
    #[test]
    fn search_ranks_name_hits_above_description_hits() {
        let cands = vec![
            ("alpha".to_string(), "does beta things".to_string()),
            ("beta_tool".to_string(), "nothing in common".to_string()),
        ];
        assert_eq!(search("beta", &cands), ["beta_tool", "alpha"]);
        // 大小写不敏感
        assert_eq!(search("BETA", &cands), ["beta_tool", "alpha"]);
    }

    /// 多个词累加：两个词都命中的排在只命中一个的前面。
    #[test]
    fn search_sums_the_words() {
        let found = search("todo list", &candidates());
        assert_eq!(found.first().map(String::as_str), Some("todo_write"));
    }

    /// 虚词和单个字母不算分，也不按子串比：`a` / `in` 不该让每个候选都得分
    #[test]
    fn search_ignores_filler_words() {
        let found = search("run a command in the background", &candidates());
        assert_eq!(found.first().map(String::as_str), Some(SHELL_BACKGROUND));
        for name in ["task", "web_search", "memory_search", "todo_write", "list_dir"] {
            assert!(!found.iter().any(|f| f == name), "{name} 不该命中：{found:?}");
        }
        // 前缀算命中（复数、词形）
        assert_eq!(search("todos", &candidates()).first().map(String::as_str), Some("todo_write"));
        assert!(search("a in the", &candidates()).is_empty());
    }

    /// 最多 8 个。
    #[test]
    fn search_returns_at_most_eight() {
        let cands: Vec<(String, String)> =
            (0..12).map(|i| (format!("widget_{i}"), format!("tool number {i}"))).collect();
        assert_eq!(search("widget", &cands).len(), SEARCH_MAX);
    }

    /// 一条都不命中：空 vec（调用方回「换个说法」）。
    #[test]
    fn search_returns_nothing_when_no_word_matches() {
        assert!(search("zzzz", &candidates()).is_empty());
        assert!(search("", &candidates()).is_empty());
    }

    /// 搜到后台命令时，它的配套工具一起给。
    #[test]
    fn search_brings_the_shell_family_together() {
        let found = search("background", &candidates());
        for name in [SHELL_BACKGROUND, SHELL_OUTPUT, SHELL_STOP, SHELL_FOLLOWUP] {
            assert!(found.iter().any(|f| f == name), "{name} 没一起给：{found:?}");
        }
        // `task` 不影响别的
        assert_eq!(search("subagent", &candidates()), ["task"]);
    }

    /// 参数：`query` 空的 / 缺参数的报错。
    #[test]
    fn tool_search_args() {
        assert_eq!(parse_tool_search(r#"{"query":"  todo "}"#).unwrap(), "todo");
        assert!(parse_tool_search(r#"{"query":"  "}"#).is_err());
        assert!(parse_tool_search("{}").is_err());
        assert!(parse_tool_search("nope").is_err());
    }

    #[test]
    fn fixed_specs_share_definitions_without_mixing_permission_modes() {
        for writable in [false, true] {
            let first = specs(writable);
            let next = specs(writable);
            assert!(first.iter().zip(&next).all(|(a, b)| std::ptr::eq(&**a, &**b)));
            assert_eq!(
                serde_json::to_value(&first).unwrap(),
                serde_json::to_value(build_specs(writable)).unwrap()
            );
        }
        for web in [false, true] {
            let first = chat_specs(web);
            let next = chat_specs(web);
            assert!(first.iter().zip(&next).all(|(a, b)| std::ptr::eq(&**a, &**b)));
            assert_eq!(
                serde_json::to_value(&first).unwrap(),
                serde_json::to_value(build_chat_specs(web)).unwrap()
            );
        }
    }

    /// `for_model`：`Direct` 恒在，`Deferred` 只在 `loaded` 里时出现，`loaded` 里不存在
    /// 的名字不影响结果。
    #[test]
    fn for_model_keeps_direct_and_loaded_only() {
        let names = |list: Vec<ToolSpec>| -> Vec<String> {
            list.iter().map(|spec| spec_name(spec).to_string()).collect()
        };
        let direct = names(for_model(specs(true), &[]));
        for name in [READ_FILE, WRITE_FILE, EDIT_FILE, SHELL, GREP, GLOB, ASK_USER, TOOL_SEARCH] {
            assert!(direct.contains(&name.to_string()), "{name} 应该常驻：{direct:?}");
        }
        // 清单也常驻：它是用户看进度的主渠道，搜一次才能用会让模型干脆不建（见 `DEFERRED`）
        assert!(direct.contains(&TODO_WRITE.to_string()), "{direct:?}");
        for name in [LIST_DIR, MEMORY_SEARCH, SHELL_OUTPUT, "task"] {
            assert!(!direct.contains(&name.to_string()), "{name} 应该先搜：{direct:?}");
        }
        // 加载过的出现；不存在的名字不报错也不多给
        let loaded = vec![LIST_DIR.to_string(), "no_such_tool".to_string()];
        let with_list_dir = names(for_model(specs(true), &loaded));
        assert!(with_list_dir.contains(&LIST_DIR.to_string()), "{with_list_dir:?}");
        assert!(!with_list_dir.contains(&MEMORY_SEARCH.to_string()), "{with_list_dir:?}");
        assert_eq!(with_list_dir.len(), direct.len() + 1);
    }

    /// MCP 工具也是 `Deferred`：一个服务器挂几个工具事先不知道，全常驻太贵。
    #[test]
    fn mcp_tools_are_deferred_too() {
        let mcp = json!({
            "type": "function",
            "function": { "name": "mcp__github__search_issues", "description": "d", "parameters": {} }
        });
        let mut all = specs(true);
        all.push(mcp.into());
        let has = |loaded: &[String]| -> bool {
            for_model(all.clone(), loaded)
                .iter()
                .any(|s| spec_name(s) == "mcp__github__search_issues")
        };
        assert!(!has(&[]));
        assert!(has(&["mcp__github__search_issues".to_string()]));
    }
}

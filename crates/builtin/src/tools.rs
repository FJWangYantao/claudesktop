//! 自研内核的内置工具：读文件 / 列目录 / 写文件 / 精确替换编辑 / shell / 抓网页（见 [`crate::fetch`]），
//! 以及不碰文件的待办清单（`todo_write`，内核直接转成界面上的计划，见 [`parse_todos`]）。
//!
//! 给模型看的说明与结果用英文（与系统提示词一致），给 UI 的摘要用中文。

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
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

const READ_FILE: &str = "read_file";
const LIST_DIR: &str = "list_dir";
const WRITE_FILE: &str = "write_file";
const EDIT_FILE: &str = "edit_file";
const SHELL: &str = "shell";
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
}

impl ToolOutput {
    pub fn ok(content: impl Into<String>, summary: impl Into<String>) -> Self {
        Self {
            ok: true,
            content: content.into(),
            summary: summary.into(),
            images: Vec::new(),
            stop_turn: false,
        }
    }

    pub fn fail(content: impl Into<String>, summary: impl Into<String>) -> Self {
        Self {
            ok: false,
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
/// 常驻的是每次都要用的那几个（读写文件、改文件、shell、搜代码、提问、搜工具）；
/// 这里是不常用、但真要用时必须能找回来的。`task` 来自 `subagent`，名字直接写字符串。
const DEFERRED: &[&str] = &[
    LIST_DIR,
    TODO_WRITE,
    WEB_FETCH,
    WEB_SEARCH,
    MEMORY_SEARCH,
    MEMORY_WRITE,
    MEMORY_FORGET,
    // `shell` 的三个配套工具：搜到其中一个，`search` 会把三个一起给
    SHELL_BACKGROUND,
    SHELL_OUTPUT,
    SHELL_STOP,
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
pub fn for_model(specs: Vec<Value>, loaded: &[String]) -> Vec<Value> {
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

/// 一份工具定义里的名字（`{"function": {"name": ..}}`）。
fn spec_name(spec: &Value) -> &str {
    spec["function"]["name"].as_str().unwrap_or_default()
}

/// 发给 provider 的工具定义（OpenAI `tools`）；`writable = false`（Plan 模式）只给只读工具。
pub fn specs(writable: bool) -> Vec<Value> {
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
                "Read a text file. Returns lines prefixed with their 1-based line number and a tab. \
                 Reads up to {READ_LIMIT} lines by default; use offset/limit for large files. \
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
                 Returns stdout, stderr and the exit code.{}",
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
                 The process is killed when the application exits.{}",
                shell_name(),
                DETACH_HINT,
            ),
            json!({
                "type": "object",
                "properties": { "command": { "type": "string" } },
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
    // `tool_search` 在两种模式下都给（它不碰文件）：Plan 模式也得能把 `todo_write` / `web_fetch` 找回来
    tools.push(f(
        TOOL_SEARCH,
        "Some of your tools are not listed here (memory, todo list, subagents, background \
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
                    "description": "What you want to do, e.g. `todo list` or `run a command in the background`"
                }
            },
            "required": ["query"]
        }),
    ));
    tools
}

/// 聊天会话的工具定义：只有联网（`web` 开着时）和记忆（只全局）。
/// 不给工作目录那套：聊天改不了文件、跑不了命令。
pub fn chat_specs(web: bool) -> Vec<Value> {
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
            });
        }
        out.push(QuestionView {
            question: q.question.trim().to_string(),
            header: q.header.map(|h| h.trim().to_string()).filter(|h| !h.is_empty()),
            options,
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
/// 语料就是工具定义本身（名字 + description），不另维护一份描述。
pub fn search(query: &str, candidates: &[(String, String)]) -> Vec<String> {
    let words: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect();
    let mut scored: Vec<(usize, &str)> = Vec::new();
    for (name, description) in candidates {
        let lower_name = name.to_lowercase();
        let lower_description = description.to_lowercase();
        let mut score = 0usize;
        for word in &words {
            if lower_name.contains(word.as_str()) {
                score += 5;
            }
            if lower_description.contains(word.as_str()) {
                score += 1;
            }
        }
        if score > 0 {
            scored.push((score, name.as_str()));
        }
    }
    // 稳定排序：同分时保持候选顺序（= specs 里的顺序）
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    let mut names: Vec<String> =
        scored.into_iter().take(SEARCH_MAX).map(|(_, name)| name.to_string()).collect();
    // `shell` 的三个配套工具是一套：搜到其中一个就把三个一起给。只拿到 `shell_background`
    // 的模型不知道怎么读输出，等于没给。它们同属一套，所以不占 [`SEARCH_MAX`] 的名额。
    const SHELL_FAMILY: [&str; 3] = [SHELL_BACKGROUND, SHELL_OUTPUT, SHELL_STOP];
    if names.iter().any(|name| SHELL_FAMILY.contains(&name.as_str())) {
        for name in SHELL_FAMILY {
            if !names.iter().any(|found| found == name) {
                names.push(name.to_string());
            }
        }
    }
    names
}

fn now_ms() -> u64 {
    claudesktop_core::memory::now_ms()
}

#[derive(Deserialize)]
struct PathArgs {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Deserialize)]
struct WriteArgs {
    path: String,
    content: String,
}

#[derive(Deserialize)]
struct EditArgs {
    path: String,
    old_string: String,
    new_string: String,
    #[serde(default)]
    replace_all: bool,
}

#[derive(Deserialize)]
struct GlobArgs {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
}

#[derive(Deserialize)]
struct GrepArgs {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    include: Option<String>,
    #[serde(default)]
    ignore_case: bool,
    #[serde(default)]
    files_only: bool,
}

#[derive(Deserialize)]
struct ShellArgs {
    command: String,
    #[serde(default)]
    timeout_secs: Option<u64>,
}

impl Action {
    /// 工具名 + JSON 参数 → 动作；错误信息直接回给模型。
    pub fn parse(name: &str, arguments: &str, cwd: &Path) -> Result<Self, String> {
        let args = if arguments.trim().is_empty() { "{}" } else { arguments };
        fn de<T: for<'a> Deserialize<'a>>(name: &str, args: &str) -> Result<T, String> {
            serde_json::from_str(args).map_err(|e| format!("Invalid arguments for {name}: {e}"))
        }
        let need_path = |p: Option<String>| {
            p.filter(|p| !p.trim().is_empty())
                .map(|p| resolve(cwd, &p))
                .ok_or_else(|| format!("{name}: `path` is required"))
        };
        Ok(match name {
            READ_FILE => {
                let a: PathArgs = de(name, args)?;
                Self::Read {
                    path: need_path(a.path)?,
                    offset: a.offset.unwrap_or(1).max(1),
                    limit: a.limit.filter(|&l| l > 0).unwrap_or(READ_LIMIT),
                }
            }
            LIST_DIR => {
                let a: PathArgs = de(name, args)?;
                Self::List {
                    path: a
                        .path
                        .filter(|p| !p.trim().is_empty())
                        .map_or_else(|| cwd.to_path_buf(), |p| resolve(cwd, &p)),
                }
            }
            WRITE_FILE => {
                let a: WriteArgs = de(name, args)?;
                Self::Write { path: need_path(Some(a.path))?, content: a.content }
            }
            EDIT_FILE => {
                let a: EditArgs = de(name, args)?;
                Self::Edit {
                    path: need_path(Some(a.path))?,
                    old: a.old_string,
                    new: a.new_string,
                    all: a.replace_all,
                }
            }
            SHELL => {
                let a: ShellArgs = de(name, args)?;
                if a.command.trim().is_empty() {
                    return Err("shell: `command` is empty".into());
                }
                let secs =
                    a.timeout_secs.unwrap_or(SHELL_TIMEOUT_DEFAULT).clamp(1, SHELL_TIMEOUT_MAX);
                Self::Shell {
                    command: a.command,
                    timeout: Duration::from_secs(secs),
                    cwd: cwd.to_path_buf(),
                }
            }
            SHELL_BACKGROUND => {
                let a: ShellArgs = de(name, args)?;
                if a.command.trim().is_empty() {
                    return Err("shell_background: `command` is empty".into());
                }
                Self::ShellBackground { command: a.command, cwd: cwd.to_path_buf() }
            }
            SHELL_OUTPUT => {
                let a: ShellOutputArgs = de(name, args)?;
                let id = a.id.trim();
                if id.is_empty() {
                    return Err("shell_output: `id` is empty".into());
                }
                Self::ShellOutput {
                    id: id.to_string(),
                    wait_secs: a.wait_secs.unwrap_or(5).min(crate::bgtasks::WAIT_MAX_SECS),
                }
            }
            SHELL_STOP => {
                let a: ShellOutputArgs = de(name, args)?;
                let id = a.id.trim();
                if id.is_empty() {
                    return Err("shell_stop: `id` is empty".into());
                }
                Self::ShellStop { id: id.to_string() }
            }
            GLOB => {
                let a: GlobArgs = de(name, args)?;
                if a.pattern.trim().is_empty() {
                    return Err("glob: `pattern` is empty".into());
                }
                globset::Glob::new(a.pattern.trim())
                    .map_err(|e| format!("glob: invalid pattern: {e}"))?;
                Self::Glob {
                    pattern: a.pattern.trim().to_string(),
                    root: dir_or_cwd(cwd, a.path),
                    cwd: cwd.to_path_buf(),
                }
            }
            GREP => {
                let a: GrepArgs = de(name, args)?;
                if a.pattern.is_empty() {
                    return Err("grep: `pattern` is empty".into());
                }
                regex::RegexBuilder::new(&a.pattern)
                    .case_insensitive(a.ignore_case)
                    .build()
                    .map_err(|e| format!("grep: invalid regex: {e}"))?;
                let include = a.include.map(|g| g.trim().to_string()).filter(|g| !g.is_empty());
                if let Some(g) = &include {
                    globset::Glob::new(g)
                        .map_err(|e| format!("grep: invalid include glob: {e}"))?;
                }
                Self::Grep {
                    pattern: a.pattern,
                    root: dir_or_cwd(cwd, a.path),
                    include,
                    ignore_case: a.ignore_case,
                    files_only: a.files_only,
                    cwd: cwd.to_path_buf(),
                }
            }
            WEB_FETCH => {
                let a: FetchArgs = de(name, args)?;
                Self::Fetch {
                    url: crate::fetch::normalize_url(&a.url)?,
                    offset: a.offset.unwrap_or(0),
                    public_only: false,
                }
            }
            WEB_SEARCH => {
                let a: SearchArgs = de(name, args)?;
                Self::Search { query: a.query }
            }
            crate::subagent::TASK => {
                let a: crate::subagent::TaskArgs = de(name, args)?;
                if a.prompt.trim().is_empty() {
                    return Err("task: `prompt` is empty".into());
                }
                Self::Task {
                    description: a.description.trim().to_string(),
                    prompt: a.prompt,
                    agent: a.subagent_type.map(|t| t.trim().to_string()).filter(|t| !t.is_empty()),
                }
            }
            other => return Err(format!("Unknown tool `{other}`")),
        })
    }

    /// 写文件 / 改文件的目标路径（检查点在执行前后各读一次）；其余工具为 None。
    pub fn written_path(&self) -> Option<&Path> {
        match self {
            Self::Write { path, .. } | Self::Edit { path, .. } => Some(path),
            _ => None,
        }
    }

    pub fn access(&self) -> Access {
        match self {
            // 子任务本身不用授权：它里面的每个调用照常问
            Self::Read { .. }
            | Self::List { .. }
            | Self::Glob { .. }
            | Self::Grep { .. }
            | Self::Task { .. } => Access::Read,
            // 读后台输出 / 停任务是对已有调用的跟进，不再问
            Self::ShellOutput { .. } | Self::ShellStop { .. } => Access::Read,
            Self::Write { .. } | Self::Edit { .. } => Access::Edit,
            Self::Shell { .. } | Self::ShellBackground { .. } => Access::Exec,
            Self::Fetch { .. } | Self::Search { .. } => Access::Fetch,
            Self::Mcp { read_only, .. } => Access::Mcp { read_only: *read_only },
        }
    }

    /// ACP 工具种类（UI 据此选图标 / 名称）
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Read { .. } | Self::List { .. } => "read",
            Self::Glob { .. } | Self::Grep { .. } => "search",
            Self::Write { .. } | Self::Edit { .. } => "edit",
            Self::Shell { .. } | Self::ShellBackground { .. } => "execute",
            Self::ShellOutput { .. } | Self::ShellStop { .. } => "read",
            Self::Fetch { .. } | Self::Search { .. } => "fetch",
            Self::Mcp { .. } => "other",
            Self::Task { .. } => "think",
        }
    }

    /// UI 工具行标题：`Read src/main.rs`、`` `cargo test` ``
    pub fn title(&self, cwd: &Path) -> String {
        match self {
            Self::Read { path, .. } => format!("Read {}", display(cwd, path)),
            Self::List { path } => format!("List {}", display(cwd, path)),
            Self::Write { path, .. } => format!("Write {}", display(cwd, path)),
            Self::Edit { path, .. } => format!("Edit {}", display(cwd, path)),
            Self::Shell { command, .. } => format!("`{}`", command.trim()),
            Self::ShellBackground { command, .. } => format!("`{}`（后台）", command.trim()),
            Self::ShellOutput { id, .. } => format!("后台输出 {id}"),
            Self::ShellStop { id } => format!("结束后台任务 {id}"),
            Self::Fetch { url, .. } => format!("Fetch {url}"),
            Self::Search { query } => format!("Search {query}"),
            Self::Task { description, agent, .. } => match agent {
                Some(agent) if agent != crate::subagent::GENERAL => {
                    format!("Task {description}（{agent}）")
                }
                _ => format!("Task {description}"),
            },
            // `github:search_issues {"q":"bug"}`：参数压成一行、截短
            Self::Mcp { server, tool, arguments, .. } => {
                let args = arguments.to_string();
                let args: String = if args.chars().count() > 80 {
                    args.chars().take(80).collect::<String>() + "…"
                } else {
                    args
                };
                format!("{server}:{tool} {args}")
            }
            Self::Glob { pattern, root, cwd } => match display(cwd, root).as_str() {
                "." => format!("Glob {pattern}"),
                dir => format!("Glob {pattern} in {dir}"),
            },
            Self::Grep { pattern, root, include, cwd, .. } => {
                let mut t = format!("Grep {pattern}");
                let dir = display(cwd, root);
                if dir != "." {
                    t.push_str(&format!(" in {dir}"));
                }
                if let Some(g) = include {
                    t.push_str(&format!(" ({g})"));
                }
                t
            }
        }
    }

    /// 写类工具的目标是否在工作目录内（工作目录外的写入永远要询问）
    pub fn inside(&self, cwd: &Path) -> bool {
        match self {
            Self::Write { path, .. } | Self::Edit { path, .. } => path.starts_with(normalize(cwd)),
            _ => true,
        }
    }

    /// 「总是允许」对应的规则；None = 不提供该选项。
    /// - 编辑：`edit`（仅工作目录内）
    /// - shell：`shell:<程序> [子命令]`，含管道 / 重定向 / 串联等的复合命令不提供
    /// - 抓网页：`fetch:<主机名>`
    /// - MCP 工具：`mcp:<服务器>:<工具>`
    pub fn rule(&self, cwd: &Path) -> Option<String> {
        match self {
            Self::Write { .. } | Self::Edit { .. } => self.inside(cwd).then(|| "edit".to_string()),
            Self::Shell { command, .. } | Self::ShellBackground { command, .. } => {
                shell_rule(command).map(|r| format!("shell:{r}"))
            }
            Self::Fetch { url, .. } => Some(format!("fetch:{}", crate::fetch::host_of(url))),
            Self::Search { .. } => Some("search".to_string()),
            Self::Mcp { server, tool, .. } => Some(format!("mcp:{server}:{tool}")),
            _ => None,
        }
    }

    /// 写 / 改文件将产生的改动（执行前算，供权限卡片和工具卡片展示）；
    /// 其他工具、预计会失败或文件不适合预览时返回 None。
    pub fn preview(&self, cwd: &Path) -> Option<DiffView> {
        match self {
            Self::Write { path, content } => {
                let old = read_for_preview(path).ok()?;
                Some(DiffView::compute(display(cwd, path), old.as_deref(), content))
            }
            Self::Edit { path, old, new, all } => {
                let text = read_for_preview(path).ok()??;
                let r = replace_in(&text, path, old, new, *all).ok()?;
                Some(DiffView::compute(display(cwd, path), Some(&text), &r.text))
            }
            _ => None,
        }
    }

    /// 这次改动能不能按块同意：写文件 / 改文件且预先算得出 diff 时给出每个文件的块
    /// （块的划分在 `claudesktop_core::hunks_with_context`，和逐块审查、按块写盘同一份）。
    pub fn permission_hunks(&self, cwd: &Path) -> Option<claudesktop_core::PermissionHunks> {
        let old = match self {
            Self::Write { path, .. } | Self::Edit { path, .. } => read_for_preview(path).ok()?,
            _ => return None,
        };
        let new = match self {
            Self::Write { path: _, content } => content.clone(),
            Self::Edit { path, old: from, new, all } => {
                let text = old.as_deref()?;
                replace_in(text, path, from, new, *all).ok()?.text
            }
            _ => return None,
        };
        let hunks = claudesktop_core::hunks_with_context(old.as_deref().unwrap_or(""), &new);
        // 太大的改动（生成的大文件之类）不给按块勾：块要整份进权限事件和会话日志、权限卡每帧都要画，
        // 这时权限卡退回整段放行 / 拒绝（预览走 `DiffView`，有行数上限）
        let lines: usize = hunks.iter().map(|h| h.lines.len()).sum();
        if hunks.is_empty() || lines > PERMISSION_HUNK_MAX_LINES {
            return None;
        }
        let path = self.written_path().map(|p| display(cwd, p)).unwrap_or_default();
        Some(claudesktop_core::PermissionHunks {
            files: vec![claudesktop_core::HunkFile { path, hunks, whole_file: old.is_none() }],
        })
    }

    /// 执行；`should_stop` 用于中断长命令，命令跑着时隔一阵把到目前为止的输出预览交给
    /// `progress`（见 [`claudesktop_core::output_tail_preview`]）。`keep` 给了就只写这些块
    /// （按块部分同意，见 [`claudesktop_core::PartialGrant`]）：其余块保持文件原来的内容。
    /// 只有写文件 / 改文件认它，其它工具忽略。
    pub fn run(
        &self,
        should_stop: &dyn Fn() -> bool,
        progress: &dyn Fn(String),
        keep: Option<&KeepHunks>,
    ) -> ToolOutput {
        match self {
            Self::Read { path, offset, limit } => read_file(path, *offset, *limit),
            Self::List { path } => list_dir(path),
            Self::Write { path, content } => write_file(path, content, keep),
            Self::Edit { path, old, new, all } => edit_file(path, old, new, *all, keep),
            Self::Shell { command, timeout, cwd } => {
                run_shell(command, cwd, *timeout, should_stop, progress)
            }
            Self::ShellBackground { command, cwd } => start_background(command, cwd),
            Self::ShellOutput { id, wait_secs } => {
                match crate::bgtasks::output(id, Duration::from_secs(*wait_secs), should_stop) {
                    Ok(text) => {
                        let summary = crate::bgtasks::clip(
                            text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim(),
                            60,
                        );
                        ToolOutput::ok(clip_output_front(&text), summary)
                    }
                    Err(e) => ToolOutput::fail(e, "没有这个任务"),
                }
            }
            Self::ShellStop { id } => match crate::bgtasks::stop(id) {
                Ok(text) => ToolOutput::ok(text.clone(), text),
                Err(e) => ToolOutput::fail(e, "没有这个任务"),
            },
            Self::Glob { pattern, root, cwd } => glob_files(pattern, root, cwd, should_stop),
            Self::Grep { pattern, root, include, ignore_case, files_only, cwd } => grep_files(
                pattern,
                root,
                include.as_deref(),
                *ignore_case,
                *files_only,
                cwd,
                should_stop,
            ),
            Self::Fetch { url, offset, public_only } => {
                crate::fetch::fetch(url, *offset, *public_only, should_stop)
            }
            Self::Search { query } => crate::websearch::search(query, should_stop),
            Self::Mcp { .. } => {
                ToolOutput::fail("MCP tool was not routed to its server", "内部错误")
            }
            Self::Task { .. } => ToolOutput::fail("task was not routed to a subagent", "内部错误"),
        }
    }
}

/// 硬熔断：即使「自动」模式也不放行的动作，返回原因（英文，直接当拒绝理由给模型）。
///
/// - 删除类命令指向关键路径（文件系统根、盘符根、家目录、工作目录及其父级、顶层目录）：
///   `rm -rf /`、`rm -rf ~`、`rm -rf <工作目录>`、`Remove-Item -Recurse -Force C:\` …
/// - 写受保护的文件（`.gitconfig`、shell 启动脚本、包管理器配置），以及本程序自己的配置：
///   数据目录（`settings.json`、`permissions.json`、`mcp.json`、插件……）和项目的 `.claudesktop/`。
///   改了它们就能给自己加「总是允许」规则或钩子，绕过分类器
///
/// 模型的分类器再准也会被参数骗过去，这类「一句命令就能毁掉这台机器」的东西不交给它判。
pub fn hard_block(action: &Action, cwd: &Path) -> Option<String> {
    match action {
        Action::Shell { command, .. } | Action::ShellBackground { command, .. } => {
            let target = critical_removal_target(command, cwd)?;
            Some(format!(
                "Automatic mode blocks recursive removal of a critical path (`{target}`): \
                 the filesystem root, a drive root, the home directory, or the working directory \
                 and its parents. Nothing was deleted. Choose a safer approach (a specific \
                 subdirectory, or a branch), or ask the user to run this themselves."
            ))
        }
        Action::Write { path, .. } | Action::Edit { path, .. } => {
            let data_dir = claudesktop_core::default_data_dir().ok();
            protected_path(path, data_dir.as_deref()).then(|| {
                format!(
                    "Automatic mode never writes to protected files such as secrets, shell startup \
                     scripts, package-manager settings and the agent's own config (`{}`). \
                     Ask the user to make this change themselves.",
                    path.display()
                )
            })
        }
        _ => None,
    }
}

/// 受保护的文件名（写这些一律不放行）：密钥、shell 启动脚本、包管理器 / 钩子配置、
/// 以及本程序自己的 MCP 配置和设置。
const PROTECTED_FILES: [&str; 24] = [
    ".gitconfig",
    ".gitmodules",
    ".bashrc",
    ".bash_profile",
    ".bash_login",
    ".bash_aliases",
    ".bash_logout",
    ".zshrc",
    ".zprofile",
    ".zshenv",
    ".zlogin",
    ".profile",
    ".envrc",
    ".npmrc",
    ".yarnrc",
    ".yarnrc.yml",
    ".pnpmfile.cjs",
    ".pre-commit-config.yaml",
    "lefthook.yml",
    "gradle-wrapper.properties",
    "maven-wrapper.properties",
    ".mcp.json",
    ".ripgreprc",
    "pyrightconfig.json",
];

/// 受保护的目录（其中任何文件的写入都不放行）；`.claudesktop` 是项目级配置（含权限规则）
const PROTECTED_DIRS: [&str; 5] = [".git", ".cargo", ".config/git", ".ssh", ".claudesktop"];

/// 这个路径是不是受保护的（写它要人工处理）。`data_dir` 是本程序的数据目录（整个受保护）。
fn protected_path(path: &Path, data_dir: Option<&Path>) -> bool {
    let path = normalize(path);
    if let Some(dir) = data_dir
        && starts_with_ignore_case(&path, &normalize(dir))
    {
        return true;
    }
    if let Some(name) = path.file_name().and_then(|n| n.to_str())
        && (PROTECTED_FILES.iter().any(|f| f.eq_ignore_ascii_case(name))
            // 点开头的环境变量文件都算（`.env`、`.env.local`、`.env.production` …）
            || name.eq_ignore_ascii_case(".env")
            || name.to_ascii_lowercase().starts_with(".env."))
    {
        return true;
    }
    // 目录：逐段比较（路径里任何一节命中就算）
    let parts: Vec<String> = path
        .components()
        .filter_map(|c| c.as_os_str().to_str().map(str::to_ascii_lowercase))
        .collect();
    for (i, part) in parts.iter().enumerate() {
        for dir in PROTECTED_DIRS {
            let mut want = dir.split('/');
            let first = want.next().unwrap_or_default();
            if part != first {
                continue;
            }
            // 两段的（`.config/git`）要求下一段也对上
            if let Some(second) = want.next()
                && parts.get(i + 1).map(String::as_str) != Some(second)
            {
                continue;
            }
            return true;
        }
    }
    false
}

/// `path` 是否在 `dir` 下（逐段比较、不分大小写：Windows 上 `C:\Users\Me` 和 `c:\users\me` 是同一处）。
fn starts_with_ignore_case(path: &Path, dir: &Path) -> bool {
    let lower = |p: &Path| -> Vec<String> {
        p.components().map(|c| c.as_os_str().to_string_lossy().to_lowercase()).collect()
    };
    let (path, dir) = (lower(path), lower(dir));
    !dir.is_empty() && path.starts_with(&dir)
}

/// 命令里如果有删除类命令指向关键路径，返回那个目标。
///
/// 只管「一句话就不可挽回」的情况：出现 `rm` / `rmdir` / `rd` / `del` / `Remove-Item`
/// 这类删除命令且带递归，目标（按最坏读法展开）是文件系统根、盘符根、家目录、
/// 工作目录本身或其父级、顶层目录。变量和命令替换展开结果不知道，不在这里判——
/// 留给分类器，它能看到上一句赋的值。
fn critical_removal_target(command: &str, cwd: &Path) -> Option<String> {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .map(|h| normalize(&h));
    let cwd = normalize(cwd);
    for (program, args) in removal_commands(command) {
        if !removes_recursively(&program, &args) {
            continue;
        }
        for arg in &args {
            if is_option(arg) {
                continue;
            }
            if let Some(target) = critical_target(arg, &cwd, home.as_deref()) {
                return Some(target);
            }
        }
    }
    None
}

/// 命令里所有「删除」类的片段：(程序名（小写，去掉路径和 .exe）, 参数)。
fn removal_commands(command: &str) -> Vec<(String, Vec<String>)> {
    // 参数是临时字符串，程序名要单独持有，所以整体返回所有权
    let owned: Vec<(String, Vec<String>)> = command
        .split([';', '&', '|', '\n', '\r'])
        .filter_map(|segment| {
            let words: Vec<&str> = segment.split_whitespace().collect();
            // 跳过前置的 `sudo` / `time` / `A=1` 之类
            let start = words
                .iter()
                .position(|w| {
                    !matches!(*w, "sudo" | "time" | "nohup" | "nice" | "env" | "call")
                        && !w.contains('=')
                })
                .unwrap_or(0);
            let word = *words.get(start)?;
            let lower = word.trim_matches(['"', '\'']).to_ascii_lowercase();
            let lower = lower.rsplit(['/', '\\']).next().unwrap_or(&lower).to_string();
            let name = lower.trim_end_matches(".exe").to_string();
            if !matches!(name.as_str(), "rm" | "rmdir" | "rd" | "del" | "remove-item" | "erase") {
                return None;
            }
            let args = words[start + 1..]
                .iter()
                .map(|w| w.trim_matches(['"', '\'']).to_string())
                .collect();
            Some((name, args))
        })
        .collect();
    owned
}

fn removes_recursively(program: &str, args: &[String]) -> bool {
    if matches!(program, "rmdir" | "rd") {
        return true;
    }
    args.iter().any(|a| {
        let a = a.to_ascii_lowercase();
        (a.starts_with('-') && (a.contains('r') || a == "--recursive" || a.contains("recurse")))
            || a == "/s"
    })
}

/// 参数是选项而不是路径：`-rf`、`--recursive`、`/s`、`/q`。
/// `/` 和 `/usr` 这种绝对路径不算。
fn is_option(arg: &str) -> bool {
    if arg.starts_with('-') {
        return true;
    }
    // 形如 `/s`、`/q` 的：一个斜杠 + 一两个字母
    let rest = arg.strip_prefix('/').or_else(|| arg.strip_prefix('\\'));
    matches!(rest, Some(r) if !r.is_empty() && r.len() <= 2 && r.chars().all(|c| c.is_ascii_alphabetic()))
}

/// 一个删除目标是不是关键路径；是就返回它（给人看的原样）。
fn critical_target(arg: &str, cwd: &Path, home: Option<&Path>) -> Option<String> {
    let arg = arg.trim();
    if arg.is_empty() || arg.contains("$(") || arg.contains('`') {
        return None;
    }
    if arg.contains('$') {
        return None;
    }
    // `~` / `~/*`：家目录本身
    if let Some(rest) = arg.strip_prefix('~')
        && (rest.is_empty() || rest == "/" || rest == "\\" || rest == "/*" || rest == "\\*")
    {
        return Some(arg.to_string());
    }
    // `/*`、`/`、`\`：文件系统根
    if matches!(arg, "/" | "//" | "\\" | "/*" | "\\*") {
        return Some(arg.to_string());
    }
    // 去掉结尾的通配 / 分隔符再判（`C:\*` → `C:\`，`D:/build/` → `D:/build`）
    let bare = arg.trim_end_matches(['*', '/', '\\']);
    if bare.is_empty() {
        return Some(arg.to_string());
    }
    let path = normalize(&if Path::new(bare).is_absolute() {
        PathBuf::from(bare)
    } else {
        cwd.join(bare)
    });
    // 文件系统根 / 盘符根（`/`、`C:\`）
    if path.parent().is_none() {
        return Some(arg.to_string());
    }
    // 顶层目录（`C:\Windows`、`/usr`）：删掉它等于毁掉系统
    if path.ancestors().count() <= 2 {
        return Some(arg.to_string());
    }
    // 家目录本身
    if home == Some(path.as_path()) {
        return Some(arg.to_string());
    }
    // 工作目录本身或它的某一级父目录
    if cwd.starts_with(&path) {
        return Some(arg.to_string());
    }
    None
}

/// `path` 参数为空时用工作目录
fn dir_or_cwd(cwd: &Path, path: Option<String>) -> PathBuf {
    path.filter(|p| !p.trim().is_empty()).map_or_else(|| cwd.to_path_buf(), |p| resolve(cwd, &p))
}

/// 遍历 `root` 下的文件（遵守 .gitignore / .ignore，跳过隐藏文件；不要求是 git 仓库）
fn walk(root: &Path) -> ignore::Walk {
    ignore::WalkBuilder::new(root).require_git(false).follow_links(false).build()
}

/// 相对 `root` 的路径，统一用 `/`
fn rel_slash(root: &Path, path: &Path) -> String {
    path.strip_prefix(root).unwrap_or(path).to_string_lossy().replace('\\', "/")
}

fn glob_files(
    pattern: &str,
    root: &Path,
    cwd: &Path,
    should_stop: &dyn Fn() -> bool,
) -> ToolOutput {
    if !root.is_dir() {
        return ToolOutput::fail(format!("Not a directory: {}", root.display()), "目录不存在");
    }
    let matcher = match globset::GlobBuilder::new(pattern).literal_separator(true).build() {
        Ok(g) => g.compile_matcher(),
        Err(e) => return ToolOutput::fail(format!("Invalid pattern: {e}"), "模式无效"),
    };
    // 不含 `/` 的模式（如 `*.rs`）按文件名匹配任意层级，和常见工具的习惯一致
    let by_name = !pattern.contains('/');
    let mut hits: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for entry in walk(root).flatten() {
        if should_stop() {
            return ToolOutput::fail("Cancelled", "已取消");
        }
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        let ok = if by_name {
            path.file_name().is_some_and(|n| matcher.is_match(n))
        } else {
            matcher.is_match(rel_slash(root, path))
        };
        if ok {
            let mtime = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .unwrap_or(std::time::UNIX_EPOCH);
            hits.push((mtime, path.to_path_buf()));
        }
    }
    hits.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let total = hits.len();
    if total == 0 {
        return ToolOutput::ok("No files found", "0 个文件");
    }
    let mut out = String::new();
    for (_, path) in hits.iter().take(GLOB_MAX) {
        let _ = writeln!(out, "{}", display(cwd, path));
    }
    if total > GLOB_MAX {
        let _ = writeln!(out, "… {} more files; narrow the pattern", total - GLOB_MAX);
    }
    ToolOutput::ok(out, format!("{total} 个文件"))
}

#[allow(clippy::too_many_arguments)]
fn grep_files(
    pattern: &str,
    root: &Path,
    include: Option<&str>,
    ignore_case: bool,
    files_only: bool,
    cwd: &Path,
    should_stop: &dyn Fn() -> bool,
) -> ToolOutput {
    let re = match regex::RegexBuilder::new(pattern).case_insensitive(ignore_case).build() {
        Ok(r) => r,
        Err(e) => return ToolOutput::fail(format!("Invalid regex: {e}"), "正则无效"),
    };
    let include = match include.map(globset::Glob::new).transpose() {
        Ok(g) => g.map(|g| g.compile_matcher()),
        Err(e) => return ToolOutput::fail(format!("Invalid include glob: {e}"), "模式无效"),
    };
    if !root.exists() {
        return ToolOutput::fail(
            format!("No such file or directory: {}", root.display()),
            "路径不存在",
        );
    }
    let mut out = String::new();
    let (mut lines, mut files, mut truncated) = (0usize, 0usize, false);
    'walk: for entry in walk(root).flatten() {
        if should_stop() {
            return ToolOutput::fail("Cancelled", "已取消");
        }
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        // include 按文件名匹配（`*.rs`），含 `/` 时按相对路径
        if let Some(m) = &include {
            let ok = match include_has_slash(m) {
                true => m.is_match(rel_slash(root, path)),
                false => path.file_name().is_some_and(|n| m.is_match(n)),
            };
            if !ok {
                continue;
            }
        }
        if entry.metadata().is_ok_and(|m| m.len() > GREP_MAX_FILE_BYTES) {
            continue;
        }
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        // 二进制文件：前 8KB 里有 NUL
        if bytes.iter().take(8192).any(|b| *b == 0) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        let mut matched = false;
        for (no, line) in text.lines().enumerate() {
            if !re.is_match(line) {
                continue;
            }
            matched = true;
            if files_only {
                break;
            }
            if lines >= GREP_MAX_LINES {
                truncated = true;
                break 'walk;
            }
            let line = truncate_chars(line.trim_end(), GREP_LINE_CHARS);
            let _ = writeln!(out, "{}:{}: {line}", display(cwd, path), no + 1);
            lines += 1;
        }
        if matched {
            files += 1;
            if files_only {
                if files > GREP_MAX_FILES {
                    truncated = true;
                    break;
                }
                let _ = writeln!(out, "{}", display(cwd, path));
            }
        }
    }
    if files == 0 {
        return ToolOutput::ok("No matches", "无匹配");
    }
    if truncated {
        out.push_str("… results truncated; narrow the pattern or path\n");
    }
    let summary = if files_only {
        format!("{} 个文件", files.min(GREP_MAX_FILES))
    } else {
        format!("{lines} 处匹配{}", if truncated { "（已截断）" } else { "" })
    };
    ToolOutput::ok(clip_output(out), summary)
}

/// GlobMatcher 不暴露原模式里有没有 `/`，看它的 glob 文本
fn include_has_slash(m: &globset::GlobMatcher) -> bool {
    m.glob().glob().contains('/')
}

/// 规则是否覆盖这条 shell 命令（按词前缀匹配，复合命令不匹配）。
pub fn shell_rule_matches(rule: &str, command: &str) -> bool {
    if is_compound(command) {
        return false;
    }
    let want: Vec<&str> = rule.split_whitespace().collect();
    let got: Vec<&str> = command.split_whitespace().collect();
    !want.is_empty()
        && got.len() >= want.len()
        && want.iter().zip(&got).all(|(w, g)| w.eq_ignore_ascii_case(g))
}

fn is_compound(command: &str) -> bool {
    command.contains([';', '&', '|', '>', '<', '`', '\n', '\r']) || command.contains("$(")
}

/// `git status -s` → `git status`；`cargo test -p x` → `cargo test`；`ls -la` → `ls`
fn shell_rule(command: &str) -> Option<String> {
    if is_compound(command) {
        return None;
    }
    let mut words = command.split_whitespace();
    let program = words.next()?;
    let sub = words.next().filter(|w| {
        w.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') && !w.starts_with('-')
    });
    Some(match sub {
        Some(sub) => format!("{program} {sub}"),
        None => program.to_string(),
    })
}

fn resolve(cwd: &Path, p: &str) -> PathBuf {
    let p = Path::new(p.trim());
    normalize(&if p.is_absolute() { p.to_path_buf() } else { cwd.join(p) })
}

/// 词法规整（去掉 `.` / `..`），不访问文件系统。
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// 工作目录内显示相对路径（`/` 分隔），否则原样。
fn display(cwd: &Path, path: &Path) -> String {
    match path.strip_prefix(normalize(cwd)) {
        Ok(rel) if rel.as_os_str().is_empty() => ".".into(),
        Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
        Err(_) => path.display().to_string(),
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}

/// 过长的结果保留首尾各一半
fn clip_output(s: String) -> String {
    let n = s.chars().count();
    if n <= OUTPUT_MAX_CHARS {
        return s;
    }
    let half = OUTPUT_MAX_CHARS / 2;
    let head: String = s.chars().take(half).collect();
    let tail: String = s.chars().skip(n - half).collect();
    format!("{head}\n… [{} chars truncated] …\n{tail}", n - OUTPUT_MAX_CHARS)
}

/// 后台任务的输出只留最近的：超了丢最前面的（日志越新越有用）。
fn clip_output_front(s: &str) -> String {
    let n = s.chars().count();
    if n <= OUTPUT_MAX_CHARS {
        return s.to_string();
    }
    let tail: String = s.chars().skip(n - OUTPUT_MAX_CHARS).collect();
    format!("… [{} chars truncated] …\n{tail}", n - OUTPUT_MAX_CHARS)
}

/// `shell_background`：起一条后台命令，立刻回任务 ID。
fn start_background(command: &str, cwd: &Path) -> ToolOutput {
    match crate::bgtasks::start(command, cwd) {
        Ok(id) => ToolOutput::ok(
            format!(
                "Started {id} in the background. Read its output with `shell_output` \
                 {{\"id\":\"{id}\"}} (it waits for new output); stop it with `shell_stop`."
            ),
            format!("后台 {id}"),
        ),
        Err(e) => ToolOutput::fail(e, "无法启动后台命令"),
    }
}

fn read_file(path: &Path, offset: usize, limit: usize) -> ToolOutput {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) => {
            return ToolOutput::fail(format!("Cannot read {}: {e}", path.display()), "读取失败");
        }
    };
    if meta.is_dir() {
        return ToolOutput::fail(
            format!("{} is a directory; use list_dir", path.display()),
            "是目录",
        );
    }
    if meta.len() > READ_MAX_BYTES {
        return ToolOutput::fail(
            format!("{} is too large ({} bytes)", path.display(), meta.len()),
            "文件过大",
        );
    }
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            return ToolOutput::fail(format!("Cannot read {}: {e}", path.display()), "读取失败");
        }
    };
    if bytes[..bytes.len().min(8192)].contains(&0) {
        return ToolOutput::fail(
            format!("{} looks like a binary file", path.display()),
            "二进制文件",
        );
    }
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    if total == 0 {
        return ToolOutput::ok("(empty file)", "空文件");
    }
    let start = offset - 1;
    if start >= total {
        return ToolOutput::fail(
            format!("offset {offset} is past the end ({total} lines)"),
            "超出行数",
        );
    }
    let end = (start + limit).min(total);
    let mut out = String::new();
    for (i, line) in lines[start..end].iter().enumerate() {
        let _ = writeln!(out, "{:>6}\t{}", start + i + 1, truncate_chars(line, LINE_MAX_CHARS));
    }
    if start > 0 || end < total {
        let _ = writeln!(
            out,
            "… showing lines {}-{end} of {total}; use offset to read more",
            start + 1
        );
    }
    ToolOutput::ok(clip_output(out), format!("{} 行", end - start))
}

fn list_dir(path: &Path) -> ToolOutput {
    let entries = match std::fs::read_dir(path) {
        Ok(e) => e,
        Err(e) => {
            return ToolOutput::fail(format!("Cannot list {}: {e}", path.display()), "列目录失败");
        }
    };
    let mut items: Vec<(bool, String)> = entries
        .flatten()
        .map(|e| {
            let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
            (is_dir, e.file_name().to_string_lossy().into_owned())
        })
        .collect();
    items.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase())));
    let total = items.len();
    let mut out = String::new();
    for (is_dir, name) in items.iter().take(LIST_MAX) {
        let _ = writeln!(out, "{name}{}", if *is_dir { "/" } else { "" });
    }
    if total > LIST_MAX {
        let _ = writeln!(out, "… {} more entries", total - LIST_MAX);
    }
    if total == 0 {
        out.push_str("(empty directory)");
    }
    ToolOutput::ok(out, format!("{total} 项"))
}

/// 按块同意时最多给多少行（各块连上下文的行数加起来）；再大就只能整段放行 / 拒绝。
const PERMISSION_HUNK_MAX_LINES: usize = 400;

/// 按块同意的结果：放行哪几块，以及问的时候给用户看的是哪些块。
/// 写盘时按当时的文件重新分块，和问的时候对不上（文件在这期间被改过，块的编号指的已经是别的改动）就不写。
#[derive(Debug, Clone, PartialEq)]
pub struct KeepHunks {
    /// 放行的块编号（[`DiffHunk::index`]）
    pub hunks: Vec<u32>,
    /// 问的时候算出来的块
    pub asked: Vec<DiffHunk>,
}

impl KeepHunks {
    /// 用户在权限卡上勾的块 + 那张卡上列的块。
    pub fn new(hunks: Vec<u32>, asked: &PermissionHunks) -> Self {
        let asked = asked.files.iter().flat_map(|f| f.hunks.iter().cloned()).collect();
        Self { hunks, asked }
    }
}

/// 按块写盘前的核对：文件现在的块和问的时候不一样，就把整次写入拒掉。
fn check_hunks(old: &str, new: &str, keep: &KeepHunks, path: &Path) -> Result<(), ToolOutput> {
    if claudesktop_core::hunks_with_context(old, new) == keep.asked {
        return Ok(());
    }
    Err(ToolOutput::fail(
        format!(
            "{} changed after the user approved some of the hunks, so nothing was written. \
             Read the file again and redo the change.",
            path.display()
        ),
        "文件在授权后又被改过，没写",
    ))
}

/// 按块写入时给模型和用户的说明：写了几块、几块没写（`None` = 全写了）。
fn partial_note(taken: usize, skipped: usize) -> Option<String> {
    (skipped > 0).then(|| format!("只写了 {taken} 块，另有 {skipped} 块没写：用户没同意"))
}

fn write_file(path: &Path, content: &str, keep: Option<&KeepHunks>) -> ToolOutput {
    let existed = path.exists();
    // 按块同意：只把放行的块写进去，其余块保持文件原来的内容
    let (content, note) = match keep {
        None => (content.to_string(), None),
        Some(keep) => {
            let old = std::fs::read_to_string(path).ok();
            if let Err(out) = check_hunks(old.as_deref().unwrap_or(""), content, keep, path) {
                return out;
            }
            let partial = splice_partial(old.as_deref(), content, &keep.hunks);
            if partial.text.is_empty() && old.is_none() {
                return ToolOutput::fail("Nothing was approved to write", "没有放行任何改动");
            }
            (partial.text, partial_note(partial.taken, partial.skipped))
        }
    };
    let content = content.as_str();
    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        return ToolOutput::fail(format!("Cannot create {}: {e}", parent.display()), "写入失败");
    }
    match std::fs::write(path, content) {
        Ok(()) => {
            let lines = content.lines().count();
            // 按块写入时说清楚：模型和用户都要知道只写了几块（模型不知道的话会以为整份都落盘了）
            let (for_model, for_user) = match &note {
                Some(n) => (format!(". {n}"), format!(" · {n}")),
                None => (String::new(), String::new()),
            };
            ToolOutput::ok(
                format!("Wrote {lines} lines to {}{for_model}", path.display()),
                if existed {
                    format!("覆盖 · {lines} 行{for_user}")
                } else {
                    format!("新建 · {lines} 行{for_user}")
                },
            )
        }
        Err(e) => ToolOutput::fail(format!("Cannot write {}: {e}", path.display()), "写入失败"),
    }
}

/// 按块拼出要写入的内容：`keep` 里放行的块取新内容，其余块保留旧内容
/// （块的划分在 `claudesktop_core::hunk_ranges`，和权限卡上列的一致）。
/// 块数按实际有的算：重复的、超出范围的编号不算。
fn splice_partial(old: Option<&str>, new: &str, keep: &[u32]) -> Partial {
    let text = claudesktop_core::splice_ranges(old, new, Some(keep));
    let total = claudesktop_core::hunk_ranges(old.unwrap_or(""), new).len();
    let taken = (0..total as u32).filter(|i| keep.contains(i)).count();
    Partial { text, taken, skipped: total - taken }
}

/// 按块写盘的结果：写下去的全文和放行 / 没放行的块数。
struct Partial {
    text: String,
    /// 实际写进文件的块
    taken: usize,
    /// 没写的块（这些改动没落到盘上）
    skipped: usize,
}

/// 一次替换的结果：新全文、替换处数、实际使用的旧 / 新片段（可能已转成 CRLF）
struct Replaced {
    text: String,
    count: usize,
    old: String,
    new: String,
}

/// 在 `text` 里做精确替换；失败时返回给模型 / UI 的错误。
fn replace_in(
    text: &str,
    path: &Path,
    old: &str,
    new: &str,
    all: bool,
) -> Result<Replaced, ToolOutput> {
    if old.is_empty() {
        return Err(ToolOutput::fail(
            "old_string must not be empty; use write_file to create files",
            "参数无效",
        ));
    }
    // 文件是 CRLF 而模型给的是 LF：按 CRLF 再试一次
    let crlf = |s: &str| s.replace("\r\n", "\n").replace('\n', "\r\n");
    let (old, new) = if !text.contains(old) && text.contains("\r\n") && text.contains(&crlf(old)) {
        (crlf(old), crlf(new))
    } else {
        (old.to_string(), new.to_string())
    };
    let count = text.matches(old.as_str()).count();
    if count == 0 {
        return Err(ToolOutput::fail(
            format!(
                "old_string not found in {}. Read the file and copy the text exactly.",
                path.display()
            ),
            "未找到要替换的内容",
        ));
    }
    if count > 1 && !all {
        return Err(ToolOutput::fail(
            format!(
                "old_string occurs {count} times in {}; add surrounding context or set replace_all",
                path.display()
            ),
            format!("匹配到 {count} 处"),
        ));
    }
    let text = if all { text.replace(&old, &new) } else { text.replacen(&old, &new, 1) };
    Ok(Replaced { text, count: if all { count } else { 1 }, old, new })
}

fn edit_file(path: &Path, old: &str, new: &str, all: bool, keep: Option<&KeepHunks>) -> ToolOutput {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            return ToolOutput::fail(format!("Cannot read {}: {e}", path.display()), "读取失败");
        }
    };
    let r = match replace_in(&text, path, old, new, all) {
        Ok(r) => r,
        Err(out) => return out,
    };
    // 按块同意：只把放行的块写进去，其余块保持改之前的样子
    let (written_text, note) = match keep {
        None => (r.text.clone(), None),
        Some(keep) => {
            if let Err(out) = check_hunks(&text, &r.text, keep, path) {
                return out;
            }
            let partial = splice_partial(Some(&text), &r.text, &keep.hunks);
            (partial.text, partial_note(partial.taken, partial.skipped))
        }
    };
    match std::fs::write(path, &written_text) {
        Ok(()) => {
            let (minus, plus) = (r.old.lines().count(), r.new.lines().count());
            let written = note.map(|n| format!("（{n}）")).unwrap_or_default();
            ToolOutput::ok(
                format!(
                    "Replaced {} occurrence(s) in {}{}",
                    r.count,
                    path.display(),
                    if written.is_empty() { String::new() } else { format!(". {written}") }
                ),
                format!("替换 {} 处（−{minus} +{plus} 行）{written}", r.count),
            )
        }
        Err(e) => ToolOutput::fail(format!("Cannot write {}: {e}", path.display()), "写入失败"),
    }
}

/// 读出用于 diff 预览的旧内容：不存在 → Ok(None)；过大或不是 UTF-8 → Err(())
fn read_for_preview(path: &Path) -> Result<Option<String>, ()> {
    match std::fs::metadata(path) {
        Err(_) => Ok(None),
        Ok(m) if m.len() > PREVIEW_MAX_BYTES => Err(()),
        Ok(_) => std::fs::read_to_string(path).map(Some).map_err(|_| ()),
    }
}

/// shell 工具说明的补充：Windows 上应用退出时会连带结束它拉起的整棵进程树
/// （`claudesktop_core::process_guard`），`start` 出去的也逃不掉。要比应用活得久的常驻服务
/// （隧道、本地网关）得用 WMI 拉起，不然用户关掉应用重编译时服务跟着断。
const DETACH_HINT: &str = if cfg!(windows) {
    " Every process started here, including ones launched with `start` or `Start-Process`, is \
     ended when claudesktop exits. To start a long-running service that must outlive the app \
     (an SSH tunnel, a local server the user keeps using), launch it outside the app's process \
     tree with WMI: `Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments \
     @{ CommandLine = '<command>' }`."
} else {
    ""
};

/// 给模型看的 shell 名称
fn shell_name() -> &'static str {
    if cfg!(windows) {
        if has_pwsh() { "PowerShell 7 (pwsh)" } else { "Windows PowerShell 5.1 (no `&&`; use `;`)" }
    } else {
        "sh"
    }
}

#[cfg(windows)]
fn has_pwsh() -> bool {
    use std::sync::OnceLock;
    static FOUND: OnceLock<bool> = OnceLock::new();
    *FOUND.get_or_init(|| {
        std::env::var_os("PATH")
            .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join("pwsh.exe").is_file()))
    })
}

#[cfg(not(windows))]
fn has_pwsh() -> bool {
    false
}

/// 跑命令时默认带上的环境变量：常见的会弹窗口 / 开浏览器的工具改成不弹（用户自己设了的不覆盖）。
const QUIET_ENV: [(&str, &str); 4] = [
    // Playwright 测试失败后默认会打开 HTML 报告
    ("PLAYWRIGHT_HTML_OPEN", "never"),
    ("PW_TEST_HTML_REPORT_OPEN", "never"),
    // create-react-app 等开发服务器启动时开浏览器
    ("BROWSER", "none"),
    // matplotlib 画图不弹窗口（savefig 照常）
    ("MPLBACKEND", "Agg"),
];

fn shell_command(command: &str) -> Command {
    let mut cmd = shell_command_raw(command);
    for (key, value) in QUIET_ENV {
        if std::env::var_os(key).is_none() {
            cmd.env(key, value);
        }
    }
    cmd
}

/// 后台命令用同一套 shell / 编码 / 不弹窗设置（`bgtasks` 模块调）。
pub(crate) fn background_command(command: &str) -> Command {
    shell_command(command)
}

fn shell_command_raw(command: &str) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let exe = if has_pwsh() { "pwsh.exe" } else { "powershell.exe" };
        let mut cmd = Command::new(exe);
        // 管道输出默认走 OEM 代码页（中文系统是 GBK），强制 UTF-8
        cmd.args(["-NoProfile", "-NonInteractive", "-Command"])
            .arg(format!(
                "[Console]::OutputEncoding=[System.Text.Encoding]::UTF8; $OutputEncoding=[System.Text.Encoding]::UTF8; {command}"
            ))
            .creation_flags(CREATE_NO_WINDOW);
        cmd
    }
    #[cfg(not(windows))]
    {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(command);
        cmd
    }
}

/// 连同子孙进程一起结束（Windows 上 `kill` 只结束 shell 本身）
pub(crate) fn kill_tree(child: &mut std::process::Child) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        let _ = Command::new("taskkill")
            .args(["/T", "/F", "/PID", &child.id().to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(0x0800_0000)
            .status();
    }
    let _ = child.kill();
}

/// 界面里不经模型直接跑的命令（worktree 的初始化）：和 `run_shell` 同一套 shell、不弹窗、
/// 边读边攒输出；在 unix 上自成一个进程组，超时连同 `npm` / `node` 这些子孙一起结束。
pub fn run_command(command: &str, cwd: &Path, timeout: Duration) -> ToolOutput {
    let cmd = shell_command(command);
    #[cfg(unix)]
    let cmd = {
        let mut cmd = cmd;
        std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
        cmd
    };
    run_shell_in(cmd, cwd, timeout, &|| false, &|_| {}, cfg!(unix))
}

/// 结束整个进程组（`own_group` = 子进程是用 `process_group(0)` 起的组长）。
fn kill_group(child: &mut std::process::Child, own_group: bool) {
    #[cfg(unix)]
    if own_group {
        let _ = Command::new("kill")
            .args(["-KILL", "--", &format!("-{}", child.id())])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = own_group;
    kill_tree(child);
}

fn run_shell(
    command: &str,
    cwd: &Path,
    timeout: Duration,
    should_stop: &dyn Fn() -> bool,
    progress: &dyn Fn(String),
) -> ToolOutput {
    run_shell_in(shell_command(command), cwd, timeout, should_stop, progress, false)
}

fn run_shell_in(
    mut cmd: Command,
    cwd: &Path,
    timeout: Duration,
    should_stop: &dyn Fn() -> bool,
    progress: &dyn Fn(String),
    own_group: bool,
) -> ToolOutput {
    let mut child = match cmd
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return ToolOutput::fail(format!("Failed to start shell: {e}"), "无法启动 shell"),
    };
    // 边读边攒：后台进程继承了管道时读不到 EOF，退出后最多再等一会儿就取已读到的部分
    let drain = |pipe: Option<Box<dyn std::io::Read + Send>>| {
        let buf: Arc<Mutex<Vec<u8>>> = Arc::default();
        let (done_tx, done_rx) = mpsc::channel::<()>();
        let sink = buf.clone();
        std::thread::spawn(move || {
            if let Some(mut pipe) = pipe {
                let mut chunk = [0u8; 8192];
                while let Ok(n) = pipe.read(&mut chunk) {
                    if n == 0 {
                        break;
                    }
                    let mut b = sink.lock().unwrap();
                    let room = PIPE_MAX_BYTES.saturating_sub(b.len());
                    b.extend_from_slice(&chunk[..n.min(room)]);
                }
            }
            let _ = done_tx.send(());
        });
        (buf, done_rx)
    };
    let stdout = drain(child.stdout.take().map(|p| Box::new(p) as Box<dyn std::io::Read + Send>));
    let stderr = drain(child.stderr.take().map(|p| Box::new(p) as Box<dyn std::io::Read + Send>));

    // 运行中的输出：每 [`LIVE_EVERY`] 看一次，有新内容就报（只取两路缓冲的末尾一截来解码）
    let live = |seen: &mut (usize, usize)| {
        let (out, err) = (stdout.0.lock().unwrap(), stderr.0.lock().unwrap());
        if (out.len(), err.len()) == *seen {
            return;
        }
        *seen = (out.len(), err.len());
        let tail = |b: &[u8]| {
            String::from_utf8_lossy(&b[b.len().saturating_sub(LIVE_TAIL_BYTES)..]).into_owned()
        };
        let mut text = tail(&out);
        if !err.is_empty() {
            let _ = write!(text, "\n[stderr]\n{}", tail(&err));
        }
        drop((out, err));
        progress(claudesktop_core::output_tail_preview(&text));
    };
    let mut seen = (0, 0);
    let mut last_live = Instant::now();

    let started = Instant::now();
    let mut interrupted = None;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(_) => break None,
        }
        if last_live.elapsed() >= LIVE_EVERY {
            last_live = Instant::now();
            live(&mut seen);
        }
        if should_stop() {
            interrupted = Some("cancelled");
        } else if started.elapsed() >= timeout {
            interrupted = Some("timed out");
        }
        if interrupted.is_some() {
            kill_group(&mut child, own_group);
            break child.wait().ok();
        }
        std::thread::sleep(Duration::from_millis(30));
    };
    let collect = |(buf, done): (Arc<Mutex<Vec<u8>>>, mpsc::Receiver<()>)| {
        let _ = done.recv_timeout(Duration::from_millis(1500));
        let bytes = std::mem::take(&mut *buf.lock().unwrap());
        String::from_utf8_lossy(&bytes).into_owned()
    };
    let stdout = collect(stdout);
    let stderr = collect(stderr);

    let mut out = stdout.trim_end().to_string();
    if !stderr.trim().is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        let _ = write!(out, "[stderr]\n{}", stderr.trim_end());
    }
    let code = status.and_then(|s| s.code());
    if let Some(why) = interrupted {
        let _ = write!(out, "\n[command {why} after {}s]", started.elapsed().as_secs());
        return ToolOutput::fail(
            clip_output(out),
            if why == "cancelled" { "已中断" } else { "超时" },
        );
    }
    let first_line =
        |s: &str| s.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
    match code {
        Some(0) => {
            let summary = first_line(&stdout);
            if out.is_empty() {
                out.push_str("(no output)");
            }
            ToolOutput::ok(
                clip_output(out),
                if summary.is_empty() { "完成".into() } else { summary },
            )
        }
        code => {
            let code = code.map_or("unknown".into(), |c| c.to_string());
            let _ = write!(out, "\n[exit code: {code}]");
            let detail = first_line(&stderr);
            ToolOutput::fail(
                clip_output(out),
                if detail.is_empty() {
                    format!("exit {code}")
                } else {
                    format!("exit {code} · {detail}")
                },
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// 搜到 `shell` 的配套工具就把三个一起给（只给一个等于没给）。
    #[test]
    fn search_brings_the_shell_family_together() {
        let found = search("background", &candidates());
        for name in [SHELL_BACKGROUND, SHELL_OUTPUT, SHELL_STOP] {
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

    /// `for_model`：`Direct` 恒在，`Deferred` 只在 `loaded` 里时出现，`loaded` 里不存在
    /// 的名字不影响结果。
    #[test]
    fn for_model_keeps_direct_and_loaded_only() {
        let names = |list: Vec<Value>| -> Vec<String> {
            list.iter().map(|spec| spec_name(spec).to_string()).collect()
        };
        let direct = names(for_model(specs(true), &[]));
        for name in [READ_FILE, WRITE_FILE, EDIT_FILE, SHELL, GREP, GLOB, ASK_USER, TOOL_SEARCH] {
            assert!(direct.contains(&name.to_string()), "{name} 应该常驻：{direct:?}");
        }
        for name in [LIST_DIR, TODO_WRITE, MEMORY_SEARCH, SHELL_OUTPUT, "task"] {
            assert!(!direct.contains(&name.to_string()), "{name} 应该先搜：{direct:?}");
        }
        // 加载过的出现；不存在的名字不报错也不多给
        let loaded = vec![TODO_WRITE.to_string(), "no_such_tool".to_string()];
        let with_todo = names(for_model(specs(true), &loaded));
        assert!(with_todo.contains(&TODO_WRITE.to_string()), "{with_todo:?}");
        assert!(!with_todo.contains(&MEMORY_SEARCH.to_string()), "{with_todo:?}");
        assert_eq!(with_todo.len(), direct.len() + 1);
    }

    /// MCP 工具也是 `Deferred`：一个服务器挂几个工具事先不知道，全常驻太贵。
    #[test]
    fn mcp_tools_are_deferred_too() {
        let mcp = json!({
            "type": "function",
            "function": { "name": "mcp__github__search_issues", "description": "d", "parameters": {} }
        });
        let mut all = specs(true);
        all.push(mcp);
        let has = |loaded: &[String]| -> bool {
            for_model(all.clone(), loaded)
                .iter()
                .any(|s| spec_name(s) == "mcp__github__search_issues")
        };
        assert!(!has(&[]));
        assert!(has(&["mcp__github__search_issues".to_string()]));
    }
}

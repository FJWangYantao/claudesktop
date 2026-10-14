//! 自研内核的内置工具：读文件 / 列目录 / 写文件 / 精确替换编辑 / shell。
//!
//! 给模型看的说明与结果用英文（与系统提示词一致），给 UI 的摘要用中文。

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

/// 单次读取的默认行数 / 单行最长字符数 / 最大文件
const READ_LIMIT: usize = 2000;
const LINE_MAX_CHARS: usize = 2000;
const READ_MAX_BYTES: u64 = 10 << 20;
const LIST_MAX: usize = 1000;
/// 交给模型的工具结果上限（字符），超出保留首尾
const OUTPUT_MAX_CHARS: usize = 30_000;
/// 每个管道最多收集的字节
const PIPE_MAX_BYTES: usize = 2 << 20;
const SHELL_TIMEOUT_DEFAULT: u64 = 120;
const SHELL_TIMEOUT_MAX: u64 = 600;

/// 工具对权限的需求。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Read,
    Edit,
    Exec,
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
}

/// 工具执行结果：`content` 回给模型，`summary` 显示在 UI 工具行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutput {
    pub ok: bool,
    pub content: String,
    pub summary: String,
}

impl ToolOutput {
    pub fn ok(content: impl Into<String>, summary: impl Into<String>) -> Self {
        Self {
            ok: true,
            content: content.into(),
            summary: summary.into(),
        }
    }

    pub fn fail(content: impl Into<String>, summary: impl Into<String>) -> Self {
        Self {
            ok: false,
            content: content.into(),
            summary: summary.into(),
        }
    }
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
    ];
    if writable {
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
                 Returns stdout, stderr and the exit code.",
                shell_name()
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
    }
    tools
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
                Self::Write {
                    path: need_path(Some(a.path))?,
                    content: a.content,
                }
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
                let secs = a
                    .timeout_secs
                    .unwrap_or(SHELL_TIMEOUT_DEFAULT)
                    .clamp(1, SHELL_TIMEOUT_MAX);
                Self::Shell {
                    command: a.command,
                    timeout: Duration::from_secs(secs),
                    cwd: cwd.to_path_buf(),
                }
            }
            other => return Err(format!("Unknown tool `{other}`")),
        })
    }

    pub fn access(&self) -> Access {
        match self {
            Self::Read { .. } | Self::List { .. } => Access::Read,
            Self::Write { .. } | Self::Edit { .. } => Access::Edit,
            Self::Shell { .. } => Access::Exec,
        }
    }

    /// ACP 工具种类（UI 据此选图标 / 名称）
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Read { .. } | Self::List { .. } => "read",
            Self::Write { .. } | Self::Edit { .. } => "edit",
            Self::Shell { .. } => "execute",
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
    pub fn rule(&self, cwd: &Path) -> Option<String> {
        match self {
            Self::Write { .. } | Self::Edit { .. } => self.inside(cwd).then(|| "edit".to_string()),
            Self::Shell { command, .. } => shell_rule(command).map(|r| format!("shell:{r}")),
            _ => None,
        }
    }

    /// 执行；`should_stop` 用于中断长命令。
    pub fn run(&self, should_stop: &dyn Fn() -> bool) -> ToolOutput {
        match self {
            Self::Read {
                path,
                offset,
                limit,
            } => read_file(path, *offset, *limit),
            Self::List { path } => list_dir(path),
            Self::Write { path, content } => write_file(path, content),
            Self::Edit {
                path,
                old,
                new,
                all,
            } => edit_file(path, old, new, *all),
            Self::Shell {
                command,
                timeout,
                cwd,
            } => run_shell(command, cwd, *timeout, should_stop),
        }
    }
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
    let sub = words
        .next()
        .filter(|w| w.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') && !w.starts_with('-'));
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

fn read_file(path: &Path, offset: usize, limit: usize) -> ToolOutput {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) => return ToolOutput::fail(format!("Cannot read {}: {e}", path.display()), "读取失败"),
    };
    if meta.is_dir() {
        return ToolOutput::fail(format!("{} is a directory; use list_dir", path.display()), "是目录");
    }
    if meta.len() > READ_MAX_BYTES {
        return ToolOutput::fail(
            format!("{} is too large ({} bytes)", path.display(), meta.len()),
            "文件过大",
        );
    }
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => return ToolOutput::fail(format!("Cannot read {}: {e}", path.display()), "读取失败"),
    };
    if bytes[..bytes.len().min(8192)].contains(&0) {
        return ToolOutput::fail(format!("{} looks like a binary file", path.display()), "二进制文件");
    }
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    if total == 0 {
        return ToolOutput::ok("(empty file)", "空文件");
    }
    let start = offset - 1;
    if start >= total {
        return ToolOutput::fail(format!("offset {offset} is past the end ({total} lines)"), "超出行数");
    }
    let end = (start + limit).min(total);
    let mut out = String::new();
    for (i, line) in lines[start..end].iter().enumerate() {
        let _ = writeln!(out, "{:>6}\t{}", start + i + 1, truncate_chars(line, LINE_MAX_CHARS));
    }
    if start > 0 || end < total {
        let _ = writeln!(out, "… showing lines {}-{end} of {total}; use offset to read more", start + 1);
    }
    ToolOutput::ok(clip_output(out), format!("{} 行", end - start))
}

fn list_dir(path: &Path) -> ToolOutput {
    let entries = match std::fs::read_dir(path) {
        Ok(e) => e,
        Err(e) => return ToolOutput::fail(format!("Cannot list {}: {e}", path.display()), "列目录失败"),
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

fn write_file(path: &Path, content: &str) -> ToolOutput {
    let existed = path.exists();
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return ToolOutput::fail(format!("Cannot create {}: {e}", parent.display()), "写入失败");
        }
    }
    match std::fs::write(path, content) {
        Ok(()) => {
            let lines = content.lines().count();
            ToolOutput::ok(
                format!("Wrote {lines} lines to {}", path.display()),
                if existed { format!("覆盖 · {lines} 行") } else { format!("新建 · {lines} 行") },
            )
        }
        Err(e) => ToolOutput::fail(format!("Cannot write {}: {e}", path.display()), "写入失败"),
    }
}

fn edit_file(path: &Path, old: &str, new: &str, all: bool) -> ToolOutput {
    if old.is_empty() {
        return ToolOutput::fail("old_string must not be empty; use write_file to create files", "参数无效");
    }
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => return ToolOutput::fail(format!("Cannot read {}: {e}", path.display()), "读取失败"),
    };
    // 文件是 CRLF 而模型给的是 LF：按 CRLF 再试一次
    let crlf = |s: &str| s.replace("\r\n", "\n").replace('\n', "\r\n");
    let (old, new) = if !text.contains(old) && text.contains("\r\n") && text.contains(&crlf(old)) {
        (crlf(old), crlf(new))
    } else {
        (old.to_string(), new.to_string())
    };
    let count = text.matches(old.as_str()).count();
    if count == 0 {
        return ToolOutput::fail(
            format!("old_string not found in {}. Read the file and copy the text exactly.", path.display()),
            "未找到要替换的内容",
        );
    }
    if count > 1 && !all {
        return ToolOutput::fail(
            format!("old_string occurs {count} times in {}; add surrounding context or set replace_all", path.display()),
            format!("匹配到 {count} 处"),
        );
    }
    let updated = if all { text.replace(&old, &new) } else { text.replacen(&old, &new, 1) };
    match std::fs::write(path, updated) {
        Ok(()) => {
            let n = if all { count } else { 1 };
            let (minus, plus) = (old.lines().count(), new.lines().count());
            ToolOutput::ok(
                format!("Replaced {n} occurrence(s) in {}", path.display()),
                format!("替换 {n} 处（−{minus} +{plus} 行）"),
            )
        }
        Err(e) => ToolOutput::fail(format!("Cannot write {}: {e}", path.display()), "写入失败"),
    }
}

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

fn shell_command(command: &str) -> Command {
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
fn kill_tree(child: &mut std::process::Child) {
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

fn run_shell(command: &str, cwd: &Path, timeout: Duration, should_stop: &dyn Fn() -> bool) -> ToolOutput {
    let mut child = match shell_command(command)
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

    let started = Instant::now();
    let mut interrupted = None;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(_) => break None,
        }
        if should_stop() {
            interrupted = Some("cancelled");
        } else if started.elapsed() >= timeout {
            interrupted = Some("timed out");
        }
        if interrupted.is_some() {
            kill_tree(&mut child);
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
        return ToolOutput::fail(clip_output(out), if why == "cancelled" { "已中断" } else { "超时" });
    }
    let first_line = |s: &str| s.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
    match code {
        Some(0) => {
            let summary = first_line(&stdout);
            if out.is_empty() {
                out.push_str("(no output)");
            }
            ToolOutput::ok(clip_output(out), if summary.is_empty() { "完成".into() } else { summary })
        }
        code => {
            let code = code.map_or("unknown".into(), |c| c.to_string());
            let _ = write!(out, "\n[exit code: {code}]");
            let detail = first_line(&stderr);
            ToolOutput::fail(
                clip_output(out),
                if detail.is_empty() { format!("exit {code}") } else { format!("exit {code} · {detail}") },
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("claudesktop-tools-{}-{name}", std::process::id()));
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
        // 工作目录外的写入：不提供「总是允许」
        let outside = Action::parse(WRITE_FILE, r#"{"path":"../other/x","content":""}"#, cwd).unwrap();
        assert!(!outside.inside(cwd));
        assert_eq!(outside.rule(cwd), None);
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
    fn read_write_edit_roundtrip() {
        let dir = tmp("rw");
        let file = dir.join("sub/a.txt");
        assert!(write_file(&file, "one\ntwo\nthree\n").ok);
        let r = read_file(&file, 2, 1);
        assert!(r.ok);
        assert!(r.content.starts_with("     2\ttwo\n"), "{}", r.content);
        assert!(r.content.contains("lines 2-2 of 3"));

        assert!(edit_file(&file, "two", "TWO", false).ok);
        assert!(!edit_file(&file, "missing", "x", false).ok);
        assert!(write_file(&file, "x\nx\n").ok);
        let dup = edit_file(&file, "x", "y", false);
        assert!(!dup.ok && dup.content.contains("2 times"));
        assert!(edit_file(&file, "x", "y", true).ok);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "y\ny\n");

        // CRLF 文件 + LF 的 old_string
        std::fs::write(&file, "a\r\nb\r\nc\r\n").unwrap();
        assert!(edit_file(&file, "a\nb", "A\nB", false).ok);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "A\r\nB\r\nc\r\n");

        let l = list_dir(&dir);
        assert_eq!(l.content.trim(), "sub/");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn shell_runs_and_reports_exit_code() {
        let dir = tmp("sh");
        let ok = run_shell("echo hello", &dir, Duration::from_secs(60), &|| false);
        assert!(ok.ok, "{ok:?}");
        assert_eq!(ok.summary, "hello");
        let bad = run_shell("exit 3", &dir, Duration::from_secs(60), &|| false);
        assert!(!bad.ok);
        assert!(bad.content.contains("[exit code: 3]"), "{bad:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn shell_stops_on_cancel() {
        let dir = tmp("cancel");
        let started = Instant::now();
        let cmd = if cfg!(windows) { "Start-Sleep 30" } else { "sleep 30" };
        let out = run_shell(cmd, &dir, Duration::from_secs(60), &|| started.elapsed() > Duration::from_millis(300));
        assert!(!out.ok && out.content.contains("cancelled"), "{out:?}");
        assert!(started.elapsed() < Duration::from_secs(10));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

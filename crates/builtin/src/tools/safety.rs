//! 工具调用的参数解析与权限判定：Action、危险命令拦截、路径保护。

use super::files::*;
use super::shell::*;
use super::*;

#[derive(Deserialize)]
pub(super) struct PathArgs {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Deserialize)]
pub(super) struct WriteArgs {
    path: String,
    content: String,
}

#[derive(Deserialize)]
pub(super) struct EditArgs {
    path: String,
    old_string: String,
    new_string: String,
    #[serde(default)]
    replace_all: bool,
}

#[derive(Deserialize)]
pub(super) struct GlobArgs {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct GrepArgs {
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
pub(super) struct ShellArgs {
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

/// 明显只读的命令白名单：「自动」模式里命中就直接放行、不调分类器（见 `lib.rs` 的
/// `authorize`）。省掉 `ls`、`git status` 这类命令的一次小模型往返。
///
/// 宁缺毋滥，往保守里判：
/// - 只认**单个**命令——出现任何能拼第二条命令、改输出去向或展开出别的东西的字符就不认：
///   `&`（含 `&&` 和单个 `&`：sh 的后台、cmd 的连接符）、`|`、`;`、换行、重定向、
///   `$`（sh / PowerShell 的变量和命令替换）、反引号、括号（PowerShell 的 `( … )` /
///   `@( … )` 子表达式）、花括号（脚本块）；
/// - 只认白名单里的前缀（第一个词，加上恰好需要的第二个词）；**会跑项目代码的不收**
///   （`cargo build` / `test` / `check` 会跑 build.rs、过程宏和测试，而工作目录内的编辑
///   是免审的，两步一拼就绕过了分类器）；
/// - 白名单内的命令带了会写盘或会跑别的程序的参数也不认（`--output` / `--fix` / `--write`、
///   `rg --pre`、`find -fprint` / `-fls`；前缀匹配会把 `--fix-dry-run` 这类也拦下，
///   多走一次分类器无害）。
///
/// 认不出的后果只是多一次分类器往返，误认的后果是没审就放行。
pub fn reads_only(command: &str) -> bool {
    let command = command.trim();
    if command.is_empty() {
        return false;
    }
    // 任何「不止一条命令 / 输出不落在终端 / 会展开出别的东西」的形状都放弃判定
    if command.contains(['&', '|', ';', '\n', '\r', '>', '<', '$', '`', '(', ')', '{', '}']) {
        return false;
    }
    let words: Vec<&str> = command.split_whitespace().collect();
    // 会写盘 / 会跑别的程序的参数：哪怕命令本身在白名单里也不算只读
    if words[1..]
        .iter()
        .any(|w| w.starts_with("--output") || w.starts_with("--fix") || w.starts_with("--write"))
    {
        return false;
    }
    let second = words.get(1).copied();
    match words[0] {
        "ls" | "pwd" | "cat" | "head" | "tail" | "wc" | "grep" | "dir" | "type" => true,
        // --pre 对每个文件跑一个程序（--pre-glob 跟着它走，一并拦）
        "rg" => !words[1..].iter().any(|w| w.starts_with("--pre")),
        // -exec / -execdir / -ok / -okdir 跑命令，-delete 删文件，-fprint* / -fls 写文件
        "find" => !words[1..].iter().any(|w| {
            w.starts_with("-exec")
                || w.starts_with("-ok")
                || w.starts_with("-fprint")
                || matches!(*w, "-delete" | "-fls")
        }),
        "git" => match second {
            Some("status" | "diff" | "log" | "show" | "rev-parse") => true,
            // 只认列分支：带分支名就是新建，-d / -m / -c / -u 这些是删、改名、复制、改上游
            Some("branch") => words[2..].iter().all(|w| {
                matches!(
                    *w,
                    "-a" | "-r"
                        | "-v"
                        | "-vv"
                        | "-l"
                        | "--list"
                        | "--all"
                        | "--remotes"
                        | "--verbose"
                        | "--show-current"
                )
            }),
            _ => false,
        },
        "cargo" => second == Some("metadata"),
        "node" => second == Some("--version"),
        "npm" => second == Some("ls"),
        _ => false,
    }
}

/// 受保护的文件名（写这些一律不放行）：密钥、shell 启动脚本、包管理器 / 钩子配置、
/// 以及本程序自己的 MCP 配置和设置。
pub(super) const PROTECTED_FILES: [&str; 24] = [
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
pub(super) const PROTECTED_DIRS: [&str; 5] =
    [".git", ".cargo", ".config/git", ".ssh", ".claudesktop"];

/// 这个路径是不是受保护的（写它要人工处理）。`data_dir` 是本程序的数据目录（整个受保护）。
pub(super) fn protected_path(path: &Path, data_dir: Option<&Path>) -> bool {
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
pub(super) fn starts_with_ignore_case(path: &Path, dir: &Path) -> bool {
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
pub(super) fn critical_removal_target(command: &str, cwd: &Path) -> Option<String> {
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
pub(super) fn removal_commands(command: &str) -> Vec<(String, Vec<String>)> {
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

pub(super) fn removes_recursively(program: &str, args: &[String]) -> bool {
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
pub(super) fn is_option(arg: &str) -> bool {
    if arg.starts_with('-') {
        return true;
    }
    // 形如 `/s`、`/q` 的：一个斜杠 + 一两个字母
    let rest = arg.strip_prefix('/').or_else(|| arg.strip_prefix('\\'));
    matches!(rest, Some(r) if !r.is_empty() && r.len() <= 2 && r.chars().all(|c| c.is_ascii_alphabetic()))
}

/// 一个删除目标是不是关键路径；是就返回它（给人看的原样）。
pub(super) fn critical_target(arg: &str, cwd: &Path, home: Option<&Path>) -> Option<String> {
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

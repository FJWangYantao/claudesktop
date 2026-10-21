//! 本机已装的 agent：发现与预设（见「设计说明-本机agent.md」）。
//!
//! 用户机器上往往已经装好并登录了别的 coding agent（Claude Code、OpenCode、pi……），
//! 订阅额度和模型都在那边。这里只做两件事：
//!
//! 1. **预设表**（[`PRESETS`]）：每家一条——怎么在 PATH 里找、ACP 启动命令是什么、
//!    没装时给什么安装命令、登录命令、有没有条款风险。加一家 = 表里加一行。
//! 2. **发现**（[`discover`] / [`discover_in`]）：在 PATH 和几个常见的全局安装目录里找
//!    可执行文件，跑一次 `--version` 抠出版本号，判断装没装、版本够不够新。
//!
//! 边界（设计说明第一、二节）：**只看装没装，不打开它们的配置目录和凭据文件**。
//! 登没登录要等真连上 ACP 了由 agent 自己说；凭据始终在 agent 自己的存储里。
//! 也不替用户安装——没装时只给一条安装命令和文档链接（[`AgentPreset::install_hint`]）。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// 一家 agent 的预设。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentPreset {
    /// 稳定的 ID（设置里存它）：`claude` / `opencode` / `pi` / `gemini`
    pub id: &'static str,
    /// 显示名
    pub name: &'static str,
    /// 怎么判断装没装：在 PATH 里找的可执行文件名（Windows 自动补 `.cmd` / `.exe` / `.ps1`）。
    /// 用 npx 起的适配器这一项是它真正要跑的那个程序（如 `claude`）
    pub probe: &'static str,
    /// 探测版本的参数（一般是 `--version`）；空 = 不探测
    pub version_args: &'static [&'static str],
    /// 最低版本；低于它显示「版本过旧」。只比数字部分（`1.4.0` vs `v1.4.0-rc1`）
    pub min_version: Option<&'static str>,
    /// ACP 启动命令（shell-words 语法，同 `AcpKernelConfig::command`）
    pub command: &'static str,
    /// Windows 上的启动命令（`npx` 是 `.cmd` 垫片，要过 `cmd /c`）；等同时为 None
    pub command_windows: Option<&'static str>,
    /// 命令以 npx 开头：没有 Node 就起不来
    pub needs_node: bool,
    /// 没装时给的安装命令（一行，用户可以复制到终端）
    pub install_hint: &'static str,
    /// 文档链接
    pub docs: &'static str,
    /// 登录命令（agent 没报 `authMethods` 时的退路），如 `["claude", "/login"]`
    pub login: Option<&'static [&'static str]>,
    /// 能不能经用量网关（认 `ANTHROPIC_BASE_URL` / `OPENAI_BASE_URL` 的才行）
    pub gateway: bool,
    /// 条款上有风险时的一句提示：检测列表、启用确认框里都显示
    pub risk: Option<&'static str>,
}

impl AgentPreset {
    /// 实际用的启动命令（Windows 上换成带 `cmd /c` 的那条）。
    pub fn command_for(&self, windows: bool) -> &'static str {
        match (windows, self.command_windows) {
            (true, Some(command)) => command,
            _ => self.command,
        }
    }

    /// 按 ID 找预设。
    pub fn find(id: &str) -> Option<&'static AgentPreset> {
        PRESETS.iter().find(|p| p.id == id)
    }
}

/// Claude Code 适配器的条款提示（和设置页现有的那句同一份措辞）。
pub const CLAUDE_CODE_RISK: &str = "风险：Claude Code 用订阅账号登录时，Anthropic 条款不允许经第三方程序使用，账号可能被限制；用 API Key 登录不受影响";

/// 内置的几家。加一家 = 这里加一条。
pub const PRESETS: &[AgentPreset] = &[
    AgentPreset {
        id: "claude",
        name: "Claude Code",
        probe: "claude",
        version_args: &["--version"],
        min_version: None,
        command: "npx -y @agentclientprotocol/claude-agent-acp",
        command_windows: Some("cmd /c npx -y @agentclientprotocol/claude-agent-acp"),
        needs_node: true,
        install_hint: "npm install -g @anthropic-ai/claude-code",
        docs: "https://docs.anthropic.com/en/docs/claude-code",
        login: Some(&["claude", "/login"]),
        gateway: true,
        risk: Some(CLAUDE_CODE_RISK),
    },
    AgentPreset {
        id: "opencode",
        name: "OpenCode",
        probe: "opencode",
        version_args: &["--version"],
        min_version: None,
        command: "opencode acp",
        command_windows: None,
        needs_node: false,
        install_hint: "npm install -g opencode-ai",
        docs: "https://opencode.ai/docs/acp",
        login: Some(&["opencode", "auth", "login"]),
        // OpenCode 自己管模型和端点，不认 ANTHROPIC_BASE_URL
        gateway: false,
        risk: None,
    },
    AgentPreset {
        id: "pi",
        name: "pi",
        probe: "pi",
        version_args: &["--version"],
        // 社区的 pi-acp 还只支持 0.80.4 以后（设计说明第三节）
        min_version: Some("0.80.4"),
        command: "npx -y pi-acp",
        command_windows: Some("cmd /c npx -y pi-acp"),
        needs_node: true,
        install_hint: "npm install -g @dyta/pi",
        docs: "https://github.com/DxTa/pi-acp",
        login: None,
        gateway: true,
        risk: None,
    },
    AgentPreset {
        id: "gemini",
        name: "Gemini CLI",
        probe: "gemini",
        version_args: &["--version"],
        min_version: None,
        // 参数名实施时核实；现在按社区用法写，起不来时用户可以自己改「自定义命令」
        command: "gemini --experimental-acp",
        command_windows: None,
        needs_node: false,
        install_hint: "npm install -g @google/gemini-cli",
        docs: "https://github.com/google-gemini/gemini-cli",
        login: None,
        gateway: false,
        risk: None,
    },
];

/// 探测结果的状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetectState {
    /// 装好了，可以启用
    Ready,
    /// 版本低于预设要求
    TooOld { need: String },
    /// 要用 npx 起，但这台机器上没有 Node
    NeedsNode,
}

/// 找到的一家 agent。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detected {
    pub preset: &'static AgentPreset,
    /// 找到的可执行文件
    pub path: PathBuf,
    /// `--version` 输出里抠出来的版本号；取不到 = None（不当成没装）
    pub version: Option<String>,
    pub state: DetectState,
}

impl Detected {
    /// 能不能直接启用。
    pub fn is_ready(&self) -> bool {
        self.state == DetectState::Ready
    }

    /// 列表里那一行的短说明：「2.3.1」/「0.79.0 · 版本过旧，需要 0.80.4 以上」/「需要先装 Node」。
    pub fn summary(&self) -> String {
        match &self.state {
            DetectState::Ready => self.version.clone().unwrap_or_else(|| "已安装".into()),
            DetectState::TooOld { need } => format!(
                "{} · 版本过旧，需要 {need} 以上",
                self.version.clone().unwrap_or_else(|| "版本未知".into())
            ),
            DetectState::NeedsNode => "需要先装 Node.js".into(),
        }
    }

    /// 没装成 / 版本不对时怎么办（按钮上的文字）。
    pub fn fix_label(&self) -> &'static str {
        match self.state {
            DetectState::Ready => "启用",
            DetectState::TooOld { .. } => "如何升级",
            DetectState::NeedsNode => "如何安装",
        }
    }
}

/// 发现本机的 agent：结果为「装了的」（没装的不在列表里，界面按 [`PRESETS`] 补一行「未安装」）。
///
/// 要起几个子进程（每家一次 `--version`），在后台线程里跑；界面缓存结果，
/// 设置页打开 / 点「重新检测」时刷新。
pub fn discover() -> Vec<Detected> {
    let mut paths = search_paths();
    paths.extend(common_install_dirs());
    let run = |path: &Path, args: &[&str]| run_version(path, args);
    discover_in(PRESETS, &paths, run)
}

/// 可测的发现：在 `paths` 里找 `presets` 里每一家的可执行文件，用 `run` 取版本输出。
///
/// `run(path, args)` 返回命令的 stdout（取不到返回 None）——单测里换成假的，
/// 不去真起进程。找到文件但 `run` 给 None 时算「装了、版本取不到」，不算没装。
pub fn discover_in(
    presets: &'static [AgentPreset],
    paths: &[PathBuf],
    run: impl Fn(&Path, &[&str]) -> Option<String>,
) -> Vec<Detected> {
    let has_node = find_executable("node", paths).is_some();
    let mut out = Vec::new();
    for preset in presets {
        let Some(path) = find_executable(preset.probe, paths) else {
            continue;
        };
        let output = if preset.version_args.is_empty() {
            None
        } else {
            run(&path, preset.version_args)
        };
        let version = output.as_deref().and_then(parse_version);
        let state = if preset.needs_node && !has_node {
            DetectState::NeedsNode
        } else if let (Some(min), Some(found)) = (preset.min_version, version.as_deref())
            && version_cmp(found, min).is_lt()
        {
            DetectState::TooOld { need: min.to_string() }
        } else {
            DetectState::Ready
        };
        out.push(Detected { preset, path, version, state });
    }
    out
}

/// PATH 各项（图形界面启动时常常比终端里短，所以下面还要补常见目录）。
pub fn search_paths() -> Vec<PathBuf> {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).filter(|d| !d.as_os_str().is_empty()).collect())
        .unwrap_or_default()
}

/// 常见的全局安装目录（终端里的 PATH 多半有，从图形界面起来的进程常常没有）。
pub fn common_install_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let var = |key: &str| std::env::var_os(key).map(PathBuf::from);
    if cfg!(windows) {
        if let Some(appdata) = var("APPDATA") {
            dirs.push(appdata.join("npm"));
        }
        if let Some(local) = var("LOCALAPPDATA") {
            dirs.push(local.join("Programs"));
            // Volta / Scoop 的垫片
            dirs.push(local.join("Volta/bin"));
            dirs.push(local.join("Microsoft/WinGet/Links"));
        }
        if let Some(home) = var("USERPROFILE") {
            dirs.push(home.join(".bun/bin"));
            dirs.push(home.join(".local/bin"));
            dirs.push(home.join("scoop/shims"));
        }
    } else {
        if let Some(home) = var("HOME") {
            dirs.push(home.join(".local/bin"));
            dirs.push(home.join(".npm-global/bin"));
            dirs.push(home.join(".bun/bin"));
            dirs.push(home.join(".cargo/bin"));
            dirs.push(home.join(".opencode/bin"));
        }
        dirs.push(PathBuf::from("/usr/local/bin"));
        dirs.push(PathBuf::from("/opt/homebrew/bin"));
    }
    dirs
}

/// 在 `paths` 里找一个可执行文件；Windows 上按 PATHEXT 补后缀。
pub fn find_executable(name: &str, paths: &[PathBuf]) -> Option<PathBuf> {
    let names = candidate_names(name);
    for dir in paths {
        for candidate in &names {
            let path = dir.join(candidate);
            if path.is_file() {
                return Some(path);
            }
        }
    }
    None
}

/// 可执行文件名 → 要试的几个文件名（Windows 上 `claude` 实际是 `claude.cmd`）。
fn candidate_names(name: &str) -> Vec<String> {
    if !cfg!(windows) || Path::new(name).extension().is_some() {
        return vec![name.to_string()];
    }
    ["exe", "cmd", "bat", "ps1", ""]
        .iter()
        .map(|ext| if ext.is_empty() { name.to_string() } else { format!("{name}.{ext}") })
        .collect()
}

/// 跑一次 `--version`：3 秒超时、不弹窗口、输出当 UTF-8 读（读不出再按 GBK 猜一次）。
fn run_version(path: &Path, args: &[&str]) -> Option<String> {
    let mut command = Command::new(path);
    command.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    // 这些工具偶尔会问「要不要更新」之类的交互问题：不去 stdin 就不会卡住
    for (key, value) in quiet_env() {
        if std::env::var_os(key).is_none() {
            command.env(key, value);
        }
    }
    let mut child = command.spawn().ok()?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() > Duration::from_secs(3) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => return None,
        }
    }
    let out = child.wait_with_output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if text.is_empty() {
        // 有的工具把版本写到 stderr
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return (!err.is_empty()).then_some(err);
    }
    Some(text)
}

/// 「不打扰前台」的环境变量（和内核的后台命令同一套，见 `tools` 的 `QUIET_ENV`）。
fn quiet_env() -> [(&'static str, &'static str); 4] {
    [
        ("PLAYWRIGHT_HTML_OPEN", "never"),
        ("PW_TEST_HTML_REPORT_OPEN", "never"),
        ("BROWSER", "none"),
        ("MPLBACKEND", "Agg"),
    ]
}

/// 从 `--version` 的输出里抠出版本号：`1.4.0` / `v2.3.1` / `claude 2.0.1 (Claude Code)` /
/// `@anthropic-ai/claude-code/1.2.3 win32-x64 node-v20.1.0` 都要认。
/// 取第一个「至少两段数字」的三段式数字；取不到返回 None。
pub fn parse_version(output: &str) -> Option<String> {
    for token in output.split(|c: char| c.is_whitespace() || c == '/' || c == ',' || c == '(') {
        let token = token.trim_start_matches('v');
        let core = token.split(['-', '+']).next().unwrap_or("");
        let parts: Vec<&str> = core.split('.').collect();
        if parts.len() < 2 || !parts.iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        {
            continue;
        }
        // `node-v20.1.0` 这类会被前一条规则先吃掉 `node`，这里只认数字开头的那段
        return Some(core.to_string());
    }
    None
}

/// 版本比较：按数字段比，段数不同时短的补 0（`1.4` < `1.4.1`）；非数字段按字面比。
pub fn version_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let parse = |s: &str| -> Vec<u64> {
        s.split(['-', '+'])
            .next()
            .unwrap_or("")
            .split('.')
            .map(|p| p.parse::<u64>().unwrap_or(0))
            .collect()
    };
    let (mut a, mut b) = (parse(a), parse(b));
    let len = a.len().max(b.len());
    a.resize(len, 0);
    b.resize(len, 0);
    a.cmp(&b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = std::env::temp_dir().join(format!("claudesktop-agents-{name}-{stamp}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 假 PATH：建几个空文件当可执行文件，`run` 给假的版本输出。
    #[test]
    fn finds_installed_agents_and_their_versions() {
        let dir = tmp("discover");
        std::fs::write(dir.join(exe("claude")), "").unwrap();
        std::fs::write(dir.join(exe("opencode")), "").unwrap();
        // claude 走 npx：这台「机器」上得有 node
        std::fs::write(dir.join(exe("node")), "").unwrap();
        let paths = vec![dir.clone()];
        let found = discover_in(PRESETS, &paths, |path, _| {
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            Some(if name.starts_with("claude") { "2.3.1 (Claude Code)" } else { "1.4.0" }.into())
        });
        // pi / gemini 没装：不在结果里（界面按 PRESETS 补「未安装」）
        let ids: Vec<&str> = found.iter().map(|d| d.preset.id).collect();
        assert_eq!(ids, ["claude", "opencode"], "{ids:?}");
        assert_eq!(found[0].version.as_deref(), Some("2.3.1"));
        assert!(found[0].is_ready());
        assert_eq!(found[0].summary(), "2.3.1");
        // 风险提示跟着预设走
        assert!(found[0].preset.risk.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 版本过旧：说出需要多少以上；取不到版本号时不当成没装，也不当成过旧
    #[test]
    fn flags_old_versions_but_not_missing_ones() {
        let dir = tmp("versions");
        std::fs::write(dir.join(exe("pi")), "").unwrap();
        // pi 走 npx：有 node 才轮到比版本
        std::fs::write(dir.join(exe("node")), "").unwrap();
        let paths = vec![dir.clone()];
        let old = discover_in(PRESETS, &paths, |_, _| Some("0.79.0".into()));
        let pi = old.iter().find(|d| d.preset.id == "pi").unwrap();
        assert_eq!(pi.state, DetectState::TooOld { need: "0.80.4".into() });
        assert!(pi.summary().contains("需要 0.80.4 以上"), "{}", pi.summary());
        assert_eq!(pi.fix_label(), "如何升级");
        assert!(!pi.is_ready());

        let new = discover_in(PRESETS, &paths, |_, _| Some("0.80.4".into()));
        assert!(new.iter().find(|d| d.preset.id == "pi").unwrap().is_ready());
        // 更高的版本也算够新
        let newer = discover_in(PRESETS, &paths, |_, _| Some("1.0".into()));
        assert!(newer.iter().find(|d| d.preset.id == "pi").unwrap().is_ready());

        // 版本取不到：装着，能用
        let unknown = discover_in(PRESETS, &paths, |_, _| None);
        let pi = unknown.iter().find(|d| d.preset.id == "pi").unwrap();
        assert_eq!(pi.state, DetectState::Ready);
        assert_eq!(pi.version, None);
        assert_eq!(pi.summary(), "已安装");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 要 npx 起的那几家：机器上没有 node 时提示先装
    #[test]
    fn asks_for_node_when_npx_is_needed() {
        let dir = tmp("node");
        std::fs::write(dir.join(exe("claude")), "").unwrap();
        let paths = vec![dir.clone()];
        let found = discover_in(PRESETS, &paths, |_, _| Some("2.0.0".into()));
        let claude = found.iter().find(|d| d.preset.id == "claude").unwrap();
        assert_eq!(claude.state, DetectState::NeedsNode);
        assert_eq!(claude.fix_label(), "如何安装");
        // 装上 node 之后就正常了
        std::fs::write(dir.join(exe("node")), "").unwrap();
        let found = discover_in(PRESETS, &paths, |_, _| Some("2.0.0".into()));
        assert!(found.iter().find(|d| d.preset.id == "claude").unwrap().is_ready());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `--version` 的输出五花八门：抠出数字那一段
    #[test]
    fn parses_version_strings() {
        assert_eq!(parse_version("1.4.0"), Some("1.4.0".into()));
        assert_eq!(parse_version("v2.3.1\n"), Some("2.3.1".into()));
        assert_eq!(parse_version("claude 2.3.1 (Claude Code)"), Some("2.3.1".into()));
        assert_eq!(
            parse_version("@agentclientprotocol/claude-agent-acp/0.5.2 win32-x64 node-v20.1.0"),
            Some("0.5.2".into())
        );
        // 两段也算（`opencode 1.2`）
        assert_eq!(parse_version("opencode 1.2"), Some("1.2".into()));
        assert_eq!(parse_version("0.80.4-rc1"), Some("0.80.4".into()));
        assert_eq!(parse_version("no version here"), None);
        assert_eq!(parse_version(""), None);
        // 只有一段数字的不算版本（`node 20`）
        assert_eq!(parse_version("node 20"), None);
    }

    #[test]
    fn compares_versions_by_number() {
        use std::cmp::Ordering;
        assert_eq!(version_cmp("1.4.0", "1.4.0"), Ordering::Equal);
        assert_eq!(version_cmp("0.79.0", "0.80.4"), Ordering::Less);
        assert_eq!(version_cmp("1.10.0", "1.9.9"), Ordering::Greater);
        // 段数不同：短的补 0
        assert_eq!(version_cmp("1.4", "1.4.0"), Ordering::Equal);
        assert_eq!(version_cmp("1.4", "1.4.1"), Ordering::Less);
        assert_eq!(version_cmp("2", "1.99.99"), Ordering::Greater);
    }

    /// Windows 上 npx 起的要过 `cmd /c`；其余原样
    #[test]
    fn windows_commands_go_through_cmd() {
        let claude = AgentPreset::find("claude").unwrap();
        assert!(claude.command_for(true).starts_with("cmd /c "));
        assert!(!claude.command_for(false).starts_with("cmd "));
        let opencode = AgentPreset::find("opencode").unwrap();
        assert_eq!(opencode.command_for(true), "opencode acp");
        // 预设 ID 不重复
        let mut ids: Vec<&str> = PRESETS.iter().map(|p| p.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), PRESETS.len());
        // 每一家都有安装命令和文档
        assert!(PRESETS.iter().all(|p| !p.install_hint.is_empty() && p.docs.starts_with("http")));
    }

    /// 可执行文件名的候选：Windows 上补 .cmd / .exe 等（`find_executable` 只认真的文件）
    #[test]
    fn candidate_names_cover_windows_shims() {
        let names = candidate_names("claude");
        if cfg!(windows) {
            assert!(names.contains(&"claude.cmd".to_string()), "{names:?}");
            assert!(names.contains(&"claude.exe".to_string()), "{names:?}");
        } else {
            assert_eq!(names, vec!["claude".to_string()]);
        }
        // 已经带扩展名的不再补
        assert_eq!(candidate_names("x.cmd"), vec!["x.cmd".to_string()]);
    }

    /// 找不到就是没装（不 panic、不猜）
    #[test]
    fn missing_agents_are_absent() {
        let dir = tmp("none");
        let found = discover_in(PRESETS, &[dir.clone()], |_, _| Some("1.0.0".into()));
        assert!(found.is_empty());
        assert!(find_executable("definitely-not-here", &[dir.clone()]).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 当前平台的可执行文件名（测试里建假文件用）
    fn exe(name: &str) -> String {
        if cfg!(windows) { format!("{name}.exe") } else { name.to_string() }
    }
}

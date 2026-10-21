//! 建好 worktree 之后的两件事（项目配置的 `worktree` 一栏，见「设计说明-worktree.md」第七节）：
//!
//! - `worktree.copy`：从主目录把未跟踪的文件（`.env*` 这类）复制过去——`git worktree add`
//!   只带走了已提交的内容，`.env` 缺了很多项目根本跑不起来；
//! - `worktree.setup`：在里面跑一遍初始化命令（`npm install`），输出进对话里的一张工具卡。
//!
//! 两项都在后台跑（复制大目录 / 装依赖要几十秒），失败只记一张卡片、不阻塞对话：
//! 用户照样可以在里面让 agent 干活，出了问题卡片上有原因。
//! 没配时什么都不做，界面上也不提（[`claudesktop_core::project::WorktreeConfig::is_empty`]）。

use super::ChatApp;
use crate::model::{ChatItem, ToolCard, ToolPhase};
use claudesktop_core::project::{self, WorktreeConfig};
use gpui_kit::component::notification::Notification;
use gpui_kit::*;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// 初始化命令最多等多久（装依赖偶尔卡住；超时就把已经输出的部分记下来，进程连同子孙一起结束）。
const SETUP_TIMEOUT_SECS: u64 = 900;

/// 复制过来的文件 / 目录最多记几条（多了卡片上一屏放不下）。
const COPY_LISTED: usize = 8;

impl ChatApp {
    /// 读一遍项目配置里的 `worktree` 一栏（建 worktree 前调用）。
    /// 工作目录在主仓库里时按主仓库读，读不到就当没配。
    pub(crate) fn worktree_setup_for(&self, cwd: &Path) -> WorktreeConfig {
        let root = project::root_or(cwd);
        let config = match project::load(root) {
            Some(config) => config,
            None => match claudesktop_core::git::repo_root(cwd) {
                // 工作目录是仓库里的一个子目录、配置写在仓库根
                Some(repo) => project::load(&repo).unwrap_or_default(),
                None => return WorktreeConfig::default(),
            },
        };
        config.worktree.normalized()
    }

    /// 在第 `idx` 个会话里执行 `worktree` 一栏的两件事（worktree 刚建好时调用）。
    ///
    /// `repo` 是主仓库根（复制从这儿取文件），跑在后台；复制先做，完了再跑初始化命令
    /// （命令可能要读复制过去的 `.env`）。结果都落在会话的对话流里。
    pub(crate) fn run_worktree_setup(
        &mut self,
        idx: usize,
        repo: PathBuf,
        config: WorktreeConfig,
        cx: &mut Context<Self>,
    ) {
        if config.is_empty() || idx >= self.sessions.len() {
            return;
        }
        let worktree = self.sessions[idx].cwd.clone();
        let token = self.sessions[idx].token;
        // 复制 + 初始化各记一行，边跑边显示（复制先插一张「跑着」的卡）
        if !config.copy.is_empty() {
            self.sessions[idx]
                .note(format!("把 {} 个模式匹配到的文件从主目录复制过来…", config.copy.len()));
        }
        let setup = config.setup.clone();
        let patterns = config.copy.clone();
        cx.spawn(async move |this, cx| {
            let job = {
                let (repo, worktree, patterns) = (repo.clone(), worktree.clone(), patterns.clone());
                cx.background_executor().spawn(async move {
                    let copied = copy_patterns(&repo, &worktree, &patterns);
                    let setup = match setup {
                        Some(command) => Some((command.clone(), run_setup(&worktree, &command))),
                        None => None,
                    };
                    (copied, setup)
                })
            };
            let (copied, setup) = job.await;
            let _ = this.update(cx, |app, cx| {
                // 删会话会挪下标：按令牌找回来
                let Some(idx) = app.session_index(token) else {
                    return;
                };
                if let Some(message) = copied.error_message() {
                    app.toast(Notification::warning(message), cx);
                }
                if let Some((label, _)) = copied.report() {
                    app.sessions[idx].note(label);
                    if let Some((output, result, phase)) = copied.card() {
                        // 复制成功的文件列一张只读的工具卡，点开看完整清单
                        app.sessions[idx].transcript.push(ChatItem::Tool(ToolCard {
                            id: None,
                            kind: "other".into(),
                            name: "worktree.copy".into(),
                            arg: format!("{} 个文件", copied.copied).into(),
                            result,
                            output,
                            phase,
                            diffs: Vec::new(),
                            expanded: phase == ToolPhase::Failed,
                            task: false,
                            grant: None,
                        }));
                    }
                }
                if let Some((command, run)) = setup {
                    app.sessions[idx].transcript.push(ChatItem::Tool(ToolCard {
                        id: None,
                        kind: "execute".into(),
                        name: "worktree.setup".into(),
                        arg: command.trim().to_string().into(),
                        result: run.summary.into(),
                        output: run.output.clone().into(),
                        phase: if run.ok { ToolPhase::Done } else { ToolPhase::Failed },
                        diffs: Vec::new(),
                        expanded: !run.ok,
                        task: false,
                        grant: None,
                    }));
                    if !run.ok {
                        app.toast(
                            Notification::warning(
                                "worktree 的初始化命令没跑通：结果在对话里（不影响继续对话）",
                            ),
                            cx,
                        );
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 设置页「项目」里的「现在复制一遍」：把配置里的复制规则在当前 worktree 里跑一次
    /// （配置是建的时候读的，改完不想重开会话时可以手动补）。不在 worktree 里时什么都不做。
    pub(crate) fn recopy_worktree(&mut self, cx: &mut Context<Self>) {
        let Some(meta) = self.session().worktree.clone() else {
            self.toast(Notification::warning("这个对话不在 worktree 里"), cx);
            return;
        };
        let config = self.worktree_setup_for(&meta.repo);
        if config.copy.is_empty() {
            self.toast(Notification::warning("项目配置里还没有要复制的文件（worktree.copy）"), cx);
            return;
        }
        let idx = self.active;
        self.run_worktree_setup(idx, meta.repo, WorktreeConfig { setup: None, ..config }, cx);
    }
}

/// 复制的结果：复制了几个、跳过 / 失败了几条、失败的原因。
#[derive(Debug, Default)]
pub(crate) struct Copied {
    /// 复制了几个文件
    pub copied: usize,
    /// 规则一条都没匹配到
    pub missed: Vec<String>,
    /// 没复制成的规则和原因
    pub failures: Vec<String>,
    /// 复制到的文件（相对主仓库根），卡片展开时看
    pub listing_text: Vec<String>,
}

impl Copied {
    /// 对话里那一行说明；一条规则都没匹配到时会说清楚（多半是 glob 写错了）。
    fn report(&self) -> Option<(String, String)> {
        if self.copied == 0 && self.failures.is_empty() && self.missed.is_empty() {
            return None;
        }
        let mut parts = vec![format!("复制了 {} 个文件", self.copied)];
        if !self.missed.is_empty() {
            parts.push(format!("{} 条规则没匹配到", self.missed.len()));
        }
        if !self.failures.is_empty() {
            parts.push(format!("{} 条没复制成", self.failures.len()));
        }
        let listed = self
            .missed
            .iter()
            .map(|m| format!("没匹配到：{m}"))
            .chain(self.failures.iter().map(|f| format!("没复制成：{f}")))
            .collect::<Vec<_>>()
            .join("\n");
        Some((parts.join("，"), listed))
    }

    /// 有问题时给用户的一句提示（复制失败、规则写错了）；都没问题返回 None。
    fn error_message(&self) -> Option<String> {
        if self.failures.is_empty() && self.missed.is_empty() {
            return None;
        }
        let mut parts = Vec::new();
        if !self.missed.is_empty() {
            parts.push(format!("这些规则在主目录里没匹配到：{}", self.missed.join("、")));
        }
        if !self.failures.is_empty() {
            parts.push(format!("{} 条没复制成（原因在对话里）", self.failures.len()));
        }
        Some(format!("worktree 复制：{}", parts.join("；")))
    }

    /// 复制到的文件清单（卡片展开时看），最多 [`COPY_LISTED`] 条。
    fn listing(&self) -> String {
        self.listing_text.join("\n")
    }

    /// 复制成功的文件列成一张只读的工具卡（点开看完整清单）。
    /// 一张都没复制到时返回 None（清单只记前几条，太碎就不占一张卡）。
    fn card(&self) -> Option<(SharedString, SharedString, ToolPhase)> {
        if self.copied == 0 {
            return None;
        }
        let ok = self.failures.is_empty() && self.missed.is_empty();
        Some((
            self.listing().into(),
            if ok { "已复制".into() } else { "部分没复制成".into() },
            if ok { ToolPhase::Done } else { ToolPhase::Failed },
        ))
    }
}

/// 按规则复制，返回汇总。
fn copy_patterns(repo: &Path, worktree: &Path, patterns: &[String]) -> Copied {
    let mut out = Copied::default();
    for pattern in patterns {
        let matches = project::copy_matches(repo, pattern);
        if matches.is_empty() {
            out.missed.push(pattern.clone());
            continue;
        }
        for path in matches {
            let Ok(rel) = path.strip_prefix(repo) else {
                continue;
            };
            match project::copy_into(repo, worktree, rel) {
                Ok(n) => {
                    out.copied += n;
                    if out.listing_text.len() < COPY_LISTED {
                        out.listing_text.push(rel.display().to_string());
                    }
                }
                Err(err) => out.failures.push(format!("{}（{err}）", rel.display())),
            }
        }
    }
    out
}

/// 初始化命令跑完的样子。
#[derive(Debug)]
pub(crate) struct SetupRun {
    pub ok: bool,
    /// 工具卡上的短说明
    pub summary: String,
    /// 输出（预览，见 `claudesktop_core::output_preview`）
    pub output: String,
}

/// 在 `cwd` 里跑初始化命令：走系统的 shell（Windows 是 PowerShell），不弹窗、
/// 带上和内核一样的「不打扰前台」环境变量；最多等 [`SETUP_TIMEOUT_SECS`] 秒。
fn run_setup(cwd: &Path, command: &str) -> SetupRun {
    #[cfg(windows)]
    let (mut cmd, _keep) = {
        use std::os::windows::process::CommandExt as _;
        let exe = quiet_shell();
        let mut cmd = Command::new(exe);
        cmd.args(["-NoProfile", "-NonInteractive", "-Command"])
            .arg(format!(
                "[Console]::OutputEncoding=[System.Text.Encoding]::UTF8; $OutputEncoding=[System.Text.Encoding]::UTF8; {command}"
            ))
            .creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        (cmd, ())
    };
    #[cfg(not(windows))]
    let (mut cmd, _keep) = {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(command);
        (cmd, ())
    };
    cmd.current_dir(cwd).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    // 和内核的后台命令一样：别弹浏览器 / HTML 报告（用户自己设了的不覆盖）
    for (key, value) in QUIET_ENV {
        if std::env::var_os(key).is_none() {
            cmd.env(key, value);
        }
    }
    let start = std::time::Instant::now();
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            return SetupRun {
                ok: false,
                summary: "起不来".into(),
                output: format!("这条命令起不来：{e}"),
            };
        }
    };
    // 等不了就杀掉（连同子孙），把已经读到的输出留着
    let mut timed_out = false;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
            Err(e) => {
                return SetupRun {
                    ok: false,
                    summary: "起不来".into(),
                    output: format!("等这条命令时出错：{e}"),
                };
            }
        }
        if start.elapsed().as_secs() > SETUP_TIMEOUT_SECS {
            timed_out = true;
            let _ = child.kill();
            let _ = child.wait();
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let output = child.wait_with_output().ok();
    let text = match &output {
        Some(out) => {
            let mut text = String::from_utf8_lossy(&out.stdout).to_string();
            let err = String::from_utf8_lossy(&out.stderr);
            if !err.trim().is_empty() {
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str(&err);
            }
            text
        }
        None => String::new(),
    };
    let code = output.as_ref().and_then(|o| o.status.code());
    let ok = !timed_out && output.as_ref().is_some_and(|o| o.status.success());
    let summary = if timed_out {
        format!("超时（{} 分钟）后停掉了", SETUP_TIMEOUT_SECS / 60)
    } else {
        match code {
            Some(0) => "完成".to_string(),
            Some(code) => format!("退出码 {code}"),
            None => "被结束了".to_string(),
        }
    };
    SetupRun { ok, summary, output: claudesktop_core::output_preview(&text) }
}

/// 和内核一样的「不弹窗口」环境变量（见 `claudesktop_builtin::tools`；这里只用它跑初始化，
/// 不依赖内核 crate 的私有项，值保持一致）。
const QUIET_ENV: [(&str, &str); 4] = [
    ("PLAYWRIGHT_HTML_OPEN", "never"),
    ("PW_TEST_HTML_REPORT_OPEN", "never"),
    ("BROWSER", "none"),
    ("MPLBACKEND", "Agg"),
];

/// Windows 上用哪个 shell 跑初始化：有 pwsh 用 pwsh，否则 Windows PowerShell。
#[cfg(windows)]
fn quiet_shell() -> &'static str {
    use std::sync::OnceLock;
    static SHELL: OnceLock<&'static str> = OnceLock::new();
    SHELL.get_or_init(|| {
        let has_pwsh = std::env::var_os("PATH")
            .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join("pwsh.exe").is_file()));
        if has_pwsh { "pwsh.exe" } else { "powershell.exe" }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[core::prelude::v1::test]
    fn copies_matching_untracked_files_into_the_worktree() {
        let base = std::env::temp_dir().join(format!("cd-wt-setup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let wt = base.join("wt");
        std::fs::create_dir_all(repo.join("config")).unwrap();
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(repo.join(".env"), "KEY=1").unwrap();
        std::fs::write(repo.join(".env.local"), "KEY=2").unwrap();
        std::fs::write(repo.join("config/db.json"), "{}").unwrap();

        let copied = copy_patterns(&repo, &wt, &[".env*".to_string()]);
        assert_eq!(copied.copied, 2);
        assert_eq!(std::fs::read_to_string(wt.join(".env")).unwrap(), "KEY=1");
        assert!(wt.join(".env.local").is_file());
        assert!(copied.failures.is_empty());

        // 目录整份复制（含里面的文件）
        let copied = copy_patterns(&repo, &wt, &["config".to_string()]);
        assert_eq!(copied.copied, 1);
        assert!(wt.join("config/db.json").is_file());

        // 匹配不到的规则要说出来
        let copied = copy_patterns(&repo, &wt, &["nope*".to_string()]);
        assert_eq!(copied.copied, 0);
        assert_eq!(copied.missed, vec!["nope*".to_string()]);
        assert!(copied.report().is_some());

        let _ = std::fs::remove_dir_all(&base);
    }
}

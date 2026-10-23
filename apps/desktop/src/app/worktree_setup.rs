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
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::*;
use std::path::{Path, PathBuf};

/// 初始化命令最多等多久（装依赖偶尔卡住；超时就把已经输出的部分记下来，进程连同子孙一起结束）。
const SETUP_TIMEOUT_SECS: u64 = 900;

/// 复制过来的文件 / 目录最多记几条（多了卡片上一屏放不下）。
const COPY_LISTED: usize = 8;

/// 读一遍项目配置里的 `worktree` 一栏（会跑 `git rev-parse`：别在 render 里调，菜单用
/// `Session::worktree_config` 里存的那份）。工作目录在主仓库里时按主仓库读，读不到就当没配。
pub(crate) fn worktree_config(cwd: &Path) -> WorktreeConfig {
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

/// 用户确认过的初始化命令存在哪（数据目录 `trusted_setup.json`：`[主仓库根, 命令]` 的列表）。
fn trusted_path() -> Option<PathBuf> {
    claudesktop_core::default_data_dir().ok().map(|d| d.join("trusted_setup.json"))
}

fn trusted_list() -> Vec<(String, String)> {
    trusted_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// 这条初始化命令在这个仓库里用户确认过没有。命令来自仓库里的项目配置：
/// clone 来的陌生仓库写了什么都会在本机跑，所以第一次（或命令改了之后）要用户点头。
pub(crate) fn setup_trusted(repo: &Path, command: &str) -> bool {
    let key = (repo.to_string_lossy().to_string(), command.trim().to_string());
    trusted_list().contains(&key)
}

/// 记下用户确认过（设置页「项目」里自己填的也算）。
pub(crate) fn trust_setup(repo: &Path, command: &str) {
    let key = (repo.to_string_lossy().to_string(), command.trim().to_string());
    let mut list = trusted_list();
    if list.contains(&key) {
        return;
    }
    list.push(key);
    if let Some(path) = trusted_path()
        && let Ok(text) = serde_json::to_string_pretty(&list)
    {
        let _ = std::fs::write(path, text);
    }
}

impl ChatApp {
    /// 见 [`worktree_config`]。
    pub(crate) fn worktree_setup_for(&self, cwd: &Path) -> WorktreeConfig {
        worktree_config(cwd)
    }

    /// worktree 菜单里的「运行初始化命令」：先把命令原样给用户看，确认了记下来再跑。
    pub(crate) fn ask_run_worktree_setup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let s = self.session();
        let (Some(meta), Some(command)) =
            (s.worktree.clone(), s.worktree_config.as_ref().and_then(|c| c.setup.clone()))
        else {
            return;
        };
        let token = s.token;
        let app = cx.entity();
        let description = format!(
            "这条命令写在仓库的项目配置里，会在 worktree 目录里用系统 shell 运行：\n\n{command}\n\n确认是你认识的命令再运行。"
        );
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (app, meta, command) = (app.clone(), meta.clone(), command.clone());
            alert
                .title("运行初始化命令？")
                .description(description.clone())
                .show_cancel(true)
                .ok_text("运行")
                .cancel_text("取消")
                .on_ok(move |_, _, cx| {
                    trust_setup(&meta.repo, &command);
                    app.update(cx, |this, cx| {
                        if let Some(idx) = this.session_index(token) {
                            let config =
                                WorktreeConfig { copy: Vec::new(), setup: Some(command.clone()) };
                            this.run_worktree_setup(idx, meta.repo.clone(), config, cx);
                        }
                    });
                    true
                })
        });
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
        // 没确认过的初始化命令不自动跑：说一声，菜单里确认了再跑
        let setup = config.setup.clone().filter(|command| {
            let trusted = setup_trusted(&repo, command);
            if !trusted {
                self.sessions[idx].note(format!(
                    "项目配置要在 worktree 里跑初始化命令「{command}」。它写在仓库里，第一次没自动跑：\
                     认识这条命令的话，在分支菜单里点「运行初始化命令」"
                ));
            }
            trusted
        });
        let patterns = config.copy.clone();
        cx.spawn(async move |this, cx| {
            let job = {
                let (repo, worktree, patterns) = (repo.clone(), worktree.clone(), patterns.clone());
                cx.background_executor().spawn(async move {
                    let copied = copy_patterns(&repo, &worktree, &patterns);
                    let setup =
                        setup.map(|command| (command.clone(), run_setup(&worktree, &command)));
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
                            images: Vec::new(),
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
                        images: Vec::new(),
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

/// 在 `cwd` 里跑初始化命令：和内核的命令工具同一套（系统 shell、不弹窗、带「不打扰前台」的环境变量，
/// 边跑边读输出，免得 `npm install` 这种输出多的把管道写满卡住），最多等 [`SETUP_TIMEOUT_SECS`] 秒，
/// 超时连同子孙进程一起结束。
fn run_setup(cwd: &Path, command: &str) -> SetupRun {
    let out = claudesktop_builtin::run_command(
        command,
        cwd,
        std::time::Duration::from_secs(SETUP_TIMEOUT_SECS),
    );
    let summary = match out.summary.as_str() {
        "超时" => format!("超时（{} 分钟）后停掉了", SETUP_TIMEOUT_SECS / 60),
        _ if out.ok => "完成".to_string(),
        other => other.to_string(),
    };
    SetupRun { ok: out.ok, summary, output: claudesktop_core::output_preview(&out.content) }
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

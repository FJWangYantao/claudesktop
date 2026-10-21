//! 调 git：找仓库根、建 / 查 / 删 git worktree、合并回主目录。
//!
//! 和 [`crate::plugin_git`] 共用同一套调用约定（那个模块从这里搬过来）：
//! 不弹黑框、不问密码（私有仓库要先在命令行里配好凭据），失败时把 git 的报错译成中文。
//!
//! worktree 是「一个会话在自己的分支上干活」的落点：目录放在数据目录里（不往用户仓库里塞东西），
//! 分支名 `claudesktop/<月日-时分>-<3 位随机>` 就是这次任务的句柄。合并只做最稳的那条路
//! （主目录干净、停在 base 分支上时 `git merge --no-ff`），其余情况给原因和命令让用户自己来。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};

/// worktree 的状态（会话顶栏的分支 chip 显示它）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WorktreeStatus {
    /// 有改动（含未跟踪）的文件数
    pub dirty_files: usize,
    /// 比 `base` 多几个提交
    pub ahead: usize,
    /// `base` 上有几个提交它还没有
    pub behind: usize,
}

impl WorktreeStatus {
    /// 干净且没有未合并的提交：删掉 worktree 不会丢东西。
    pub fn is_disposable(&self) -> bool {
        self.dirty_files == 0 && self.ahead == 0
    }
}

/// 仓库根：`dir` 所在的 git 仓库的**主**仓库目录（worktree 里跑就返回主仓库，不是 worktree 目录）。
/// 不在 git 仓库里返回 None。
pub fn repo_root(dir: &Path) -> Option<PathBuf> {
    if !dir.is_dir() {
        return None;
    }
    let common = git_stdout(dir, &["rev-parse", "--git-common-dir"]).ok()?;
    let common = PathBuf::from(common.trim());
    let common = if common.is_absolute() { common } else { dir.join(common) };
    // `<主仓库>/.git` → `<主仓库>`；认不出（bare 仓库等）就退回到 --show-toplevel
    if let Some(parent) = common
        .parent()
        .filter(|_| common.file_name().is_some_and(|name| name.eq_ignore_ascii_case(".git")))
    {
        return Some(normalize(parent));
    }
    git_stdout(dir, &["rev-parse", "--show-toplevel"]).ok().map(|s| normalize(Path::new(s.trim())))
}

/// 当前 HEAD 叫什么：在分支上是分支名，游离时为提交号（合并目标、领先几个提交都按它算）。
pub fn base_of(repo: &Path) -> Result<String, String> {
    if let Ok(branch) = git_stdout(repo, &["symbolic-ref", "--short", "-q", "HEAD"])
        && !branch.trim().is_empty()
    {
        return Ok(branch.trim().to_string());
    }
    let head = git_stdout(repo, &["rev-parse", "HEAD"])?;
    Ok(head.trim().to_string())
}

/// 新建 worktree：从 `repo` 当前 HEAD 拉一个 `branch` 分支检出到 `path`。
/// 同一主仓库的写操作串行（两个会话同时建 worktree 时 git 会抢 `.git/worktrees` 的锁）。
pub fn add_worktree(repo: &Path, path: &Path, branch: &str) -> Result<(), String> {
    let _guard = lock(repo);
    if path.exists() {
        return Err(format!("{} 已经存在", path.display()));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("建不了 {}：{e}", parent.display()))?;
    }
    git(repo, &["worktree", "add", "-b", branch, &path.to_string_lossy(), "HEAD"])?;
    // 数据目录在 Windows 上路径偏长：这个仓库内的文件按长路径处理（只写仓库自己的配置）
    let _ = git(path, &["config", "core.longpaths", "true"]);
    Ok(())
}

/// 把「目录没了但分支还在」的 worktree 重新检出到 `path`（用户手动删了目录 / 换了电脑）。
pub fn reattach_worktree(repo: &Path, path: &Path, branch: &str) -> Result<(), String> {
    let _guard = lock(repo);
    let _ = git(repo, &["worktree", "prune"]);
    if path.exists() {
        return Err(format!("{} 已经存在", path.display()));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("建不了 {}：{e}", parent.display()))?;
    }
    git(repo, &["worktree", "add", &path.to_string_lossy(), branch])?;
    Ok(())
}

/// worktree / 分支的现状：脏了几个文件、比 `base` 领先 / 落后几个提交。
pub fn status(
    repo: &Path,
    path: &Path,
    base: &str,
    branch: &str,
) -> Result<WorktreeStatus, String> {
    let porcelain = git_stdout(path, &["status", "--porcelain"])?;
    let dirty_files = porcelain.lines().filter(|l| !l.trim().is_empty()).count();
    // 分支没了（用户手动删了）：只报改动，不算领先
    let range = format!("{base}...{branch}");
    let (behind, ahead) = match git_stdout(repo, &["rev-list", "--left-right", "--count", &range]) {
        Ok(out) => {
            let mut parts = out.split_whitespace().map(|n| n.parse::<usize>().unwrap_or(0));
            (parts.next().unwrap_or(0), parts.next().unwrap_or(0))
        }
        Err(_) => (0, 0),
    };
    Ok(WorktreeStatus { dirty_files, ahead, behind })
}

/// 分支存不存在。
pub fn branch_exists(repo: &Path, branch: &str) -> bool {
    git(repo, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")]).is_ok()
}

/// 删 worktree 目录（`force` = 连没提交的改动一起删）。
pub fn remove_worktree(repo: &Path, path: &Path, force: bool) -> Result<(), String> {
    let _guard = lock(repo);
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    let path_text = path.to_string_lossy().to_string();
    args.push(&path_text);
    match git(repo, &args) {
        Ok(_) => Ok(()),
        // 目录已经被用户手动删了：清掉记录即可
        Err(e) if !path.exists() => {
            let _ = git(repo, &["worktree", "prune"]);
            if path.exists() { Err(e) } else { Ok(()) }
        }
        Err(e) => Err(e),
    }
}

/// 删分支（`force` = 连没合并的提交一起删）。
pub fn delete_branch(repo: &Path, branch: &str, force: bool) -> Result<(), String> {
    let _guard = lock(repo);
    git(repo, &["branch", if force { "-D" } else { "-d" }, branch]).map(|_| ())
}

/// 清掉主仓库里指向已删除目录的 worktree 记录。
pub fn prune(repo: &Path) -> Result<(), String> {
    let _guard = lock(repo);
    git(repo, &["worktree", "prune"]).map(|_| ())
}

/// 把 worktree 里没提交的改动提交掉（合并前用）；没有改动时什么都不做。
/// 返回是否真的提交了。
pub fn commit_all(path: &Path, message: &str) -> Result<bool, String> {
    let changes = git_stdout(path, &["status", "--porcelain"])?;
    if !changes.lines().any(|l| !l.trim().is_empty()) {
        return Ok(false);
    }
    git(path, &["add", "-A"])?;
    let message =
        if message.trim().is_empty() { "claudesktop: 会话改动" } else { message.trim() };
    git(path, &["commit", "-q", "-m", message])?;
    Ok(true)
}

/// 为什么现在不能合并回主目录：主目录不在 `base` 分支上，或者有没提交的改动。
pub fn merge_blocker(repo: &Path, base: &str) -> Option<String> {
    let current = base_of(repo).unwrap_or_default();
    if current != base {
        return Some(format!("主目录现在不在 {base} 分支上（停在 {current}）"));
    }
    match git_stdout(repo, &["status", "--porcelain"]) {
        Ok(out) if out.lines().any(|l| !l.trim().is_empty()) => {
            Some("主目录里有还没提交的改动".into())
        }
        _ => None,
    }
}

/// 主目录 `git merge --no-ff <branch>`；冲突就 `merge --abort` 退回原样。
/// 返回成功时的 git 输出。
pub fn merge_into_base(repo: &Path, branch: &str, base: &str) -> Result<String, String> {
    if let Some(reason) = merge_blocker(repo, base) {
        return Err(reason);
    }
    let guard = lock(repo);
    match git(repo, &["merge", "--no-ff", "--no-edit", branch]) {
        Ok(out) => {
            drop(guard);
            Ok(out)
        }
        Err(e) => {
            let _ = git(repo, &["merge", "--abort"]);
            drop(guard);
            Err(format!("{e}\n（已经在主目录里退回到合并前，先让这个会话把 {base} 合并进来再来）"))
        }
    }
}

/// 合并 / 删分支这类要在主目录里跑的命令串：拒绝合并时展示给用户，自己复制到命令行去跑。
pub fn merge_commands(repo: &Path, branch: &str) -> String {
    format!("cd \"{}\"\ngit checkout <base>\ngit merge --no-ff {branch}", repo.display())
}

/// 新分支名：`claudesktop/<月日-时分>-<3 位随机>`。`offset` 是本地时区相对 UTC 的秒数。
pub fn new_branch_name(now_ms: u64, offset_secs: i64) -> String {
    let local = (now_ms / 1000) as i64 + offset_secs;
    let days = local.div_euclid(86_400);
    let secs = local.rem_euclid(86_400);
    let (month, day) = civil_month_day(days);
    let (hour, minute) = (secs / 3600, (secs % 3600) / 60);
    format!("claudesktop/{month:02}{day:02}-{hour:02}{minute:02}-{}", suffix())
}

/// 三个随机字符（小写字母 + 数字）：同一分钟里连开几个会话时不撞名。
fn suffix() -> String {
    use std::hash::{BuildHasher, Hasher};
    const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u64(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64),
    );
    hasher.write_u32(std::process::id());
    let mut bits = hasher.finish();
    (0..3)
        .map(|_| {
            let c = CHARS[(bits % CHARS.len() as u64) as usize] as char;
            bits /= CHARS.len() as u64;
            c
        })
        .collect()
}

/// 从 1970-01-01 起的天数 → (月, 日)，不管上一年（Howard Hinnant 的 civil_from_days）。
fn civil_month_day(days: i64) -> (u32, u32) {
    let z = days + 719_468;
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (month, day)
}

/// 同一个主仓库的 git 写操作串行执行。
/// 每个仓库一把进程内长驻的锁（`Box::leak`：每个仓库一次，几十字节，够小）。
fn lock(repo: &Path) -> std::sync::MutexGuard<'static, ()> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, &'static Mutex<()>>>> = OnceLock::new();
    let locks = LOCKS.get_or_init(Mutex::default);
    let key = normalize(repo);
    let handle = *locks
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entry(key)
        .or_insert_with(|| Box::leak(Box::new(Mutex::new(()))));
    // 锁中毒（另一个线程 panic 了）不算错：git 的状态在磁盘上，继续就行
    handle.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// 规范化路径（能 canonicalize 就 canonicalize；Windows 上大小写统一到实际的那个）。
pub fn normalize(path: &Path) -> PathBuf {
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    // `\\?\C:\x` 这种前缀不展示给用户
    match path.to_string_lossy().strip_prefix(r"\\?\") {
        Some(rest) => PathBuf::from(rest),
        None => path,
    }
}

/// 两个路径是不是同一个目录（`canonicalize` 了不一样的一律算不一样）。
pub fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// 跑一条 git 命令：不弹黑框、不问密码；失败时带上 git 的报错（译成中文）。
/// 成功时返回 stdout，stdout 为空时退回 stderr（`clone` / `pull` 的进度写在那儿）。
/// 要解析输出的（`status --porcelain` 等）用 [`git_stdout`]，别把 stderr 的警告当成输出。
pub fn git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let (out, err) = run(cwd, args)?;
    Ok(if out.is_empty() { err } else { out })
}

/// 同 [`git`]，但只要 stdout（stderr 里的警告不混进来）。
pub fn git_stdout(cwd: &Path, args: &[&str]) -> Result<String, String> {
    run(cwd, args).map(|(out, _)| out)
}

/// 跑 git，成功时返回 (stdout, stderr)。
fn run(cwd: &Path, args: &[&str]) -> Result<(String, String), String> {
    let mut command = Command::new("git");
    command
        .args(args)
        .current_dir(cwd)
        // 要凭据时直接失败，别等一个看不见的输入（Git Credential Manager 也别弹窗）
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let output = command.output().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => "找不到 git：先安装 Git，并确认它在 PATH 里".to_string(),
        _ => format!("git 起不来：{e}"),
    })?;
    let text = |b: &[u8]| String::from_utf8_lossy(b).trim().to_string();
    if output.status.success() {
        Ok((text(&output.stdout), text(&output.stderr)))
    } else {
        Err(explain_git_error(&text(&output.stderr)))
    }
}

/// git 的报错 → 中文原因：常见的几种（仓库不存在 / 要登录 / 连不上 / 本地改过）说人话，
/// 其余留 git 原文的最后几行。
pub fn explain_git_error(stderr: &str) -> String {
    let lower = stderr.to_ascii_lowercase();
    let has = |needle: &str| lower.contains(needle);
    if has("could not read username")
        || has("terminal prompts disabled")
        || has("authentication failed")
    {
        return "这个仓库要登录才能访问：私有仓库请先在命令行里配好 git 凭据（装的时候不会弹窗要密码）"
            .into();
    }
    if has("repository not found")
        || has("does not appear to be a git repository")
        || has("correct access rights")
    {
        return "找不到这个仓库，或者没有访问权限：检查地址拼写；私有仓库请先在命令行里配好 git 凭据"
            .into();
    }
    if has("could not resolve host") {
        return "连不上仓库所在的网站：检查网络和地址拼写".into();
    }
    if has("connection timed out") || has("failed to connect") || has("connection refused") {
        return "连不上仓库：检查网络或代理设置".into();
    }
    if has("not possible to fast-forward") || has("diverging") || has("would be overwritten") {
        return "目录里有本地改动和这次操作冲突（会被覆盖 / 分叉了）：先在命令行里提交或处理掉这些改动"
            .into();
    }
    let tail: Vec<&str> = stderr.lines().rev().take(3).collect();
    let tail: Vec<&str> = tail.into_iter().rev().collect();
    if tail.is_empty() { "git 失败了".into() } else { tail.join("\n") }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 机器上没有 git 时这些测试跳过（不算失败）。
    fn has_git() -> bool {
        Command::new("git").arg("--version").output().is_ok()
    }

    fn run(cwd: &Path, args: &[&str]) {
        let out = Command::new("git").args(args).current_dir(cwd).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cd-git-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 临时目录里起一个提交了一个文件的仓库，返回 (顶层目录, 仓库)。
    fn repo(name: &str) -> Option<(PathBuf, PathBuf)> {
        if !has_git() {
            return None;
        }
        let base = tmp(name);
        let repo = base.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        run(&repo, &["init", "-q", "-b", "main"]);
        run(&repo, &["config", "user.email", "t@example.com"]);
        run(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("a.txt"), "一").unwrap();
        run(&repo, &["add", "."]);
        run(&repo, &["commit", "-qm", "第一版"]);
        Some((base, repo))
    }

    #[test]
    fn finds_the_main_repo_from_a_worktree() {
        let Some((base, repo)) = repo("root") else {
            return;
        };
        let canonical = normalize(&repo);
        assert_eq!(repo_root(&repo).as_deref(), Some(canonical.as_path()));
        let wt = base.join("wt");
        add_worktree(&repo, &wt, "claudesktop/x").unwrap();
        // worktree 自己也是仓库根（项目配置按它读），但主仓库要能从那儿找回来
        assert_eq!(repo_root(&wt).as_deref(), Some(canonical.as_path()));
        // 不是仓库的目录
        assert!(repo_root(&base.join("nope")).is_none());
        remove_worktree(&repo, &wt, true).unwrap();
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn creates_reports_and_removes_a_worktree() {
        let Some((base, repo)) = repo("lifecycle") else {
            return;
        };
        let wt = base.join("wt");
        let branch = new_branch_name(1_700_000_000_000, 0);
        assert!(branch.starts_with("claudesktop/"), "{branch}");
        assert_eq!(base_of(&repo).unwrap(), "main");
        add_worktree(&repo, &wt, &branch).unwrap();
        assert!(wt.join("a.txt").is_file());
        assert_eq!(base_of(&wt).unwrap(), branch);

        // 干净、没有新提交 → 可以丢
        let s = status(&repo, &wt, "main", &branch).unwrap();
        assert_eq!(s, WorktreeStatus { dirty_files: 0, ahead: 0, behind: 0 });
        assert!(s.is_disposable());

        // 改一个文件 + 加一个文件
        std::fs::write(wt.join("a.txt"), "二").unwrap();
        std::fs::write(wt.join("b.txt"), "新").unwrap();
        let s = status(&repo, &wt, "main", &branch).unwrap();
        assert_eq!(s.dirty_files, 2);

        // 不提交：带 force 才能删
        assert!(remove_worktree(&repo, &wt, false).is_err());
        commit_all(&wt, "改").unwrap();
        let s = status(&repo, &wt, "main", &branch).unwrap();
        assert_eq!((s.dirty_files, s.ahead), (0, 1));

        // 合并回主目录 → 主目录拿到改动，分支不再领先
        assert!(merge_blocker(&repo, "main").is_none());
        merge_into_base(&repo, &branch, "main").unwrap();
        assert_eq!(std::fs::read_to_string(repo.join("a.txt")).unwrap(), "二");
        assert!(repo.join("b.txt").is_file());
        assert_eq!(status(&repo, &wt, "main", &branch).unwrap().ahead, 0);

        remove_worktree(&repo, &wt, false).unwrap();
        assert!(!wt.exists());
        delete_branch(&repo, &branch, false).unwrap();
        assert!(!branch_exists(&repo, &branch));
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 主目录不在 base 分支上 / 有没提交的改动：不合并，说清原因
    #[test]
    fn refuses_to_merge_into_a_busy_main_directory() {
        let Some((base, repo)) = repo("blocked") else {
            return;
        };
        let wt = base.join("wt");
        add_worktree(&repo, &wt, "claudesktop/y").unwrap();
        std::fs::write(wt.join("a.txt"), "二").unwrap();
        commit_all(&wt, "改").unwrap();

        // 主目录有没提交的改动
        std::fs::write(repo.join("a.txt"), "本地改过").unwrap();
        let reason = merge_blocker(&repo, "main").unwrap();
        assert!(reason.contains("还没提交"), "{reason}");
        assert!(merge_into_base(&repo, "claudesktop/y", "main").is_err());
        assert_eq!(std::fs::read_to_string(repo.join("a.txt")).unwrap(), "本地改过");

        // 主目录停在别的分支上
        run(&repo, &["checkout", "-q", "-b", "other"]);
        let reason = merge_blocker(&repo, "main").unwrap();
        assert!(reason.contains("不在 main 分支"), "{reason}");
        let commands = merge_commands(&repo, "claudesktop/y");
        assert!(commands.contains("git merge --no-ff claudesktop/y"), "{commands}");

        remove_worktree(&repo, &wt, true).unwrap();
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 目录被手动删掉后重新检出；分支也没了就不再算 worktree
    #[test]
    fn reattaches_a_lost_worktree() {
        let Some((base, repo)) = repo("reattach") else {
            return;
        };
        let wt = base.join("wt");
        add_worktree(&repo, &wt, "claudesktop/z").unwrap();
        std::fs::write(wt.join("a.txt"), "二").unwrap();
        commit_all(&wt, "改").unwrap();
        std::fs::remove_dir_all(&wt).unwrap();
        assert!(!wt.exists());
        reattach_worktree(&repo, &wt, "claudesktop/z").unwrap();
        assert_eq!(std::fs::read_to_string(wt.join("a.txt")).unwrap(), "二");
        assert!(branch_exists(&repo, "claudesktop/z"));
        delete_branch(&repo, "claudesktop/不存在", true).unwrap_or_default();
        remove_worktree(&repo, &wt, true).unwrap();
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 分支名：本地时间的「月日-时分」+ 3 位随机
    #[test]
    fn branch_names_carry_the_local_time() {
        // 2023-11-14 22:13:20 UTC
        let name = new_branch_name(1_700_000_000_000, 0);
        assert!(name.starts_with("claudesktop/1114-2213-"), "{name}");
        // 东八区：第二天早上六点
        let name = new_branch_name(1_700_000_000_000, 8 * 3600);
        assert!(name.starts_with("claudesktop/1115-0613-"), "{name}");
        assert_eq!(name.rsplit('-').next().unwrap().len(), 3);
        // 同一分钟两次不一样的分支名
        assert_ne!(new_branch_name(1_700_000_000_000, 0), new_branch_name(1_700_000_000_000, 0));
    }

    #[test]
    fn explains_common_git_errors() {
        let missing = "fatal: '/tmp/nope' does not appear to be a git repository\nfatal: Could not read from remote repository.\n\nPlease make sure you have the correct access rights\nand the repository exists.";
        assert!(explain_git_error(missing).starts_with("找不到这个仓库"));
        let login =
            "fatal: could not read Username for 'https://github.com': terminal prompts disabled";
        assert!(explain_git_error(login).starts_with("这个仓库要登录"));
        assert!(
            explain_git_error("fatal: unable to access 'https://x/': Could not resolve host: x")
                .starts_with("连不上仓库所在的网站")
        );
        // 认不出的：留原文最后几行
        assert_eq!(explain_git_error("a\nb\nc\nd"), "b\nc\nd");
    }
}

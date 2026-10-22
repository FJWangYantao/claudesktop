//! 从 git 仓库装插件：克隆到插件目录、更新（`git pull`）、卸载（删掉克隆下来的目录）。
//!
//! 地址可以是 `https://…`、`ssh://…`、`git@host:路径`，或 GitHub 的简写 `作者/仓库`。
//! 一个仓库可以是一个插件，也可以是 Claude Code 的插件市场（见 [`crate::plugin::discover`]）。
//! 装好的插件和手动放进来的一样默认停用。
//!
//! 也可以只装插件市场仓库里的一个插件（[`install_subdir`]，设置页的「推荐」走这个）：
//! 稀疏克隆，只检出那个插件的目录，子路径记在 `.git/` 里（[`subdir`]），扫描时按它找插件根目录。
//!
//! 推荐项可以钉一个版本（市场清单里的 `ref`，`catalog.rs` 的 [`crate::catalog::PluginPick`]）：
//! 这时按 tag / 提交检出（[`install_at`]，`--no-checkout` 的整仓库浅克隆），装好的卡片能用
//! [`checkout`] 换到别的版本、[`version`] 看现在在哪个 tag 上。
//!
//! 都是阻塞调用（界面在后台线程跑）。git 不会弹窗要密码：私有仓库要先在命令行配好凭据。

use crate::git::{git, git_stdout};
use std::path::{Path, PathBuf};

/// 用户填的地址 → 交给 git 的地址；`作者/仓库` 补成 GitHub 的 https 地址。
pub fn normalize_url(input: &str) -> Result<String, String> {
    let url = input.trim();
    if url.is_empty() {
        return Err("先填仓库地址".into());
    }
    // 以 - 开头的会被 git 当成选项
    if url.starts_with('-') || url.chars().any(char::is_whitespace) {
        return Err("地址不对".into());
    }
    let has_scheme = ["https://", "http://", "ssh://", "git://", "file://"]
        .iter()
        .any(|s| url.to_ascii_lowercase().starts_with(s));
    if has_scheme || url.contains('@') {
        return Ok(url.to_string());
    }
    let parts: Vec<&str> = url.trim_end_matches(".git").split('/').collect();
    let ok = |s: &str| {
        !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    };
    if parts.len() == 2 && parts.iter().all(|p| ok(p)) {
        return Ok(format!("https://github.com/{}/{}.git", parts[0], parts[1]));
    }
    Err("认不出这个地址：填 https://… 的仓库地址，或 GitHub 的「作者/仓库」".into())
}

/// 克隆下来放在哪个文件夹：地址最后一段去掉 `.git`，不安全的字符换成 `-`。
pub fn folder_name(url: &str) -> Result<String, String> {
    let last = url
        .trim_end_matches('/')
        .rsplit(['/', ':'])
        .next()
        .unwrap_or_default()
        .trim_end_matches(".git");
    let name: String = last
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || "-_.".contains(c) { c } else { '-' })
        .collect();
    let name = name.trim_matches(['-', '.']).to_string();
    if name.is_empty() { Err("从地址里取不出文件夹名".into()) } else { Ok(name) }
}

/// 克隆到 `dir/<文件夹名>`（只取最新一次提交）；返回克隆下来的目录。同名文件夹已经有了就不装。
/// 失败时把克隆了一半的目录删掉。
pub fn install(dir: &Path, input: &str) -> Result<PathBuf, String> {
    let url = normalize_url(input)?;
    let dest = dir.join(folder_name(&url)?);
    if dest.exists() {
        return Err(format!("插件目录里已经有 {} 了", dest.display()));
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("建不了 {}：{e}", dir.display()))?;
    let result = git(dir, &["clone", "--depth", "1", "--", &url, &dest.to_string_lossy()]);
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&dest);
    }
    result.map(|_| dest)
}

/// 记子路径的文件（在 `.git/` 里，不进工作区，`git pull` 不受影响）
const SUBDIR_FILE: &str = "claudesktop-subdir";

/// 只装仓库里的一个子目录（插件市场里的单个插件）：稀疏克隆到 `dir/<folder>`，只检出 `subpath`。
/// 同名文件夹已经有了就不装；失败时删掉克隆了一半的目录。
pub fn install_subdir(
    dir: &Path,
    input: &str,
    subpath: &str,
    folder: &str,
) -> Result<PathBuf, String> {
    let url = normalize_url(input)?;
    let safe = |s: &str| {
        !s.is_empty()
            && !s.starts_with(['/', '-'])
            && s.split('/').all(|part| !part.is_empty() && part != "." && part != "..")
    };
    if !safe(subpath) || !safe(folder) || folder.contains('/') {
        return Err("插件路径不对".into());
    }
    let dest = dir.join(folder);
    if dest.exists() {
        return Err(format!("插件目录里已经有 {} 了", dest.display()));
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("建不了 {}：{e}", dir.display()))?;
    let dest_text = dest.to_string_lossy().to_string();
    let result = git(
        dir,
        &["clone", "--depth", "1", "--filter=blob:none", "--sparse", "--", &url, &dest_text],
    )
    .and_then(|_| git(&dest, &["sparse-checkout", "set", subpath]))
    .and_then(|_| {
        std::fs::write(dest.join(".git").join(SUBDIR_FILE), subpath)
            .map_err(|e| format!("写不了 {}：{e}", dest.display()))
    });
    if let Err(e) = result {
        let _ = std::fs::remove_dir_all(&dest);
        return Err(e);
    }
    Ok(dest)
}

/// 用 [`install_subdir`] 装的仓库里，插件在哪个子目录；整仓库装的为 None。
pub fn subdir(repo: &Path) -> Option<String> {
    let text = std::fs::read_to_string(repo.join(".git").join(SUBDIR_FILE)).ok()?;
    let text = text.trim();
    (!text.is_empty() && !text.split('/').any(|p| p == "..")).then(|| text.to_string())
}

/// 按版本装：`rev` 是 tag / 分支 / 提交（市场清单里的 `ref` / `sha`）。
/// `subpath` 给了就只检出那个子目录（同 [`install_subdir`]），否则整个仓库。
///
/// 和 [`install`] / [`install_subdir`] 的区别：不要 `--depth 1`（它只取远端默认分支的最新提交，
/// 老版本根本不在本地）。改成整仓库浅克隆 + `--no-checkout`，只下这一个版本的树，检出 `rev`。
/// `rev` 为空 = 跟最新，直接走那两条老路。
pub fn install_at(
    dir: &Path,
    input: &str,
    rev: &str,
    subpath: Option<&str>,
    folder: &str,
) -> Result<PathBuf, String> {
    let rev = check_rev(rev)?;
    // 不钉版本 = 跟最新：走 `--depth 1` 的老路，少下一大截历史
    if rev.is_empty() {
        return match subpath {
            Some(sub) => install_subdir(dir, input, sub, folder),
            None => install(dir, input),
        };
    }
    let url = normalize_url(input)?;
    let safe = |s: &str| {
        !s.is_empty()
            && !s.starts_with(['/', '-'])
            && s.split('/').all(|part| !part.is_empty() && part != "." && part != "..")
    };
    if !safe(folder) || folder.contains('/') {
        return Err("插件路径不对".into());
    }
    let sub = match subpath {
        Some(sub) if safe(sub) => Some(sub.to_string()),
        Some(_) => return Err("插件路径不对".into()),
        None => None,
    };
    let dest = dir.join(folder);
    if dest.exists() {
        return Err(format!("插件目录里已经有 {} 了", dest.display()));
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("建不了 {}：{e}", dir.display()))?;
    let dest_text = dest.to_string_lossy().to_string();
    let result =
        git(dir, &["clone", "--filter=blob:none", "--no-checkout", "--", &url, &dest_text])
            .and_then(|_| {
                // 先定好稀疏目录，再检出：否则整仓库的树都要下一遍
                if let Some(sub) = sub.as_deref() {
                    git(&dest, &["sparse-checkout", "init", "--cone"])?;
                    git(&dest, &["sparse-checkout", "set", sub])?;
                }
                checkout_rev(&dest, rev)
            })
            .and_then(|_| match sub {
                Some(sub) => std::fs::write(dest.join(".git").join(SUBDIR_FILE), sub)
                    .map_err(|e| format!("写不了 {}：{e}", dest.display())),
                None => Ok(()),
            });
    if let Err(e) = result {
        let _ = std::fs::remove_dir_all(&dest);
        return Err(format!("{e}（先确认这个版本在仓库里：git ls-remote --tags {url}）"));
    }
    Ok(dest)
}

/// 检出 `rev`：tag 优先（市场清单里的 `ref` 多是 tag），找不到再按分支 / 提交。
/// 空串 = 克隆时 HEAD 指的那个最新提交。
fn checkout_rev(repo: &Path, rev: &str) -> Result<(), String> {
    if rev.is_empty() {
        return git(repo, &["checkout", "--detach", "HEAD"]).map(|_| ());
    }
    git(repo, &["checkout", "--detach", &format!("refs/tags/{rev}")])
        .or_else(|_| git(repo, &["checkout", "--detach", rev]))
        .map(|_| ())
}

/// 版本串：不能以 `-` 开头（免得被 git 当选项），不能带空白和 `..` 这些 git 认的路径符号。
fn check_rev(rev: &str) -> Result<&str, String> {
    let rev = rev.trim();
    if rev.starts_with('-')
        || rev.contains("..")
        || rev.chars().any(|c| c.is_whitespace() || "~^:?*[\\".contains(c))
    {
        return Err("版本写的不对".into());
    }
    Ok(rev)
}

/// 现在检出的是哪个版本：打着的 tag（`v1.5.5` 这种），没打 tag 时给短提交；读不出来给 None。
pub fn version(repo: &Path) -> Option<String> {
    if let Ok(tag) = git_stdout(repo, &["describe", "--tags", "--exact-match", "HEAD"])
        && !tag.trim().is_empty()
    {
        return Some(tag.trim().to_string());
    }
    let short = git_stdout(repo, &["rev-parse", "--short", "HEAD"]).ok()?;
    let short = short.trim();
    (!short.is_empty()).then(|| short.to_string())
}

/// 这个仓库能装哪些版本：远端的 tag，新的在前（按 git 给的版本序，`v1.2.0` > `v1.1.9`）。
/// 跟最新装的是 `--depth 1` 浅克隆，本地只有最新那个提交上的 tag，所以问远端（`ls-remote`）；
/// 连不上远端时退回本地有的 tag。
pub fn versions(repo: &Path) -> Result<Vec<String>, String> {
    let lines = |out: String| -> Vec<String> {
        out.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect()
    };
    match git_stdout(repo, &["ls-remote", "--tags", "--refs", "--sort=-v:refname", "origin"]) {
        Ok(out) => Ok(lines(out)
            .iter()
            .filter_map(|l| l.split_whitespace().nth(1)?.strip_prefix("refs/tags/"))
            .map(str::to_string)
            .collect()),
        Err(_) => git_stdout(repo, &["tag", "--list", "--sort=-v:refname"]).map(lines),
    }
}

/// 浅克隆（`--depth 1` 装的）没有历史，拉东西时也只拉那一个提交。
fn is_shallow(repo: &Path) -> bool {
    git_stdout(repo, &["rev-parse", "--is-shallow-repository"]).is_ok_and(|s| s.trim() == "true")
}

/// 本地还没有 `rev` 就从远端拉下来：tag 优先，再按分支 / 提交；浅克隆只拉那一个提交。
/// 拉不到不在这里报错，接下来的 `checkout` 会说清楚。
fn fetch_rev(repo: &Path, rev: &str) {
    let has = |r: &str| {
        git_stdout(repo, &["rev-parse", "--verify", "--quiet", &format!("{r}^{{commit}}")]).is_ok()
    };
    if has(&format!("refs/tags/{rev}")) || has(rev) {
        return;
    }
    let depth: &[&str] = if is_shallow(repo) { &["--depth", "1"] } else { &[] };
    let tag = format!("refs/tags/{rev}:refs/tags/{rev}");
    let fetch = |what: &str| {
        let args: Vec<&str> = ["fetch", "--no-tags"]
            .into_iter()
            .chain(depth.iter().copied())
            .chain(["origin", what])
            .collect();
        git(repo, &args)
    };
    if fetch(&tag).is_err() {
        let _ = fetch(rev);
    }
}

/// 插件目录本来就不该改：有本地改动（或换版本后多出来的文件）就不动它，让用户卸载重装。
fn ensure_clean(repo: &Path) -> Result<(), String> {
    let dirty = git_stdout(repo, &["status", "--porcelain"])?;
    if dirty.trim().is_empty() {
        Ok(())
    } else {
        Err("插件目录里有本地改动（或者装了别的版本后新出现的文件）：先卸载再按这个版本重装".into())
    }
}

/// 换到别的版本（tag / 分支 / 提交）。本地改过的先 stash 不行——插件目录本来就不该改，
/// 直接报错让用户重装（同 [`update`]）。
pub fn checkout(repo: &Path, rev: &str) -> Result<String, String> {
    let rev = check_rev(rev)?;
    if rev.is_empty() {
        return Err("先选一个版本".into());
    }
    ensure_clean(repo)?;
    fetch_rev(repo, rev);
    git(repo, &["checkout", "--detach", &format!("refs/tags/{rev}")])
        .or_else(|_| git(repo, &["checkout", "--detach", rev]))?;
    // 稀疏克隆：换版本后子目录还在，重新写一遍免得工作区展开成整仓库
    if let Some(sub) = subdir(repo) {
        git(repo, &["sparse-checkout", "set", &sub])?;
    }
    Ok(version(repo).unwrap_or_else(|| rev.to_string()))
}

/// 拉最新的（只快进，本地改过就报错、不合并）；返回 git 的输出（「Already up to date.」等）。
/// 按版本装的、换过版本的停在某个提交上（detached HEAD），`pull` 用不了：回到远端默认分支的头。
pub fn update(repo: &Path) -> Result<String, String> {
    let result = if git_stdout(repo, &["symbolic-ref", "-q", "HEAD"]).is_ok() {
        git(repo, &["pull", "--ff-only"])
    } else {
        to_branch_head(repo)
    };
    result.map_err(|e| {
        // 本地改过的插件目录：插件这边最省事的出路是重装
        if e.starts_with("目录里有本地改动") {
            format!("{e}；或者卸载后重新安装")
        } else {
            e
        }
    })
}

/// 从某个版本回到远端默认分支（`origin/HEAD` 指的那个）的最新提交，并重新跟踪它（之后 `pull` 照常）。
fn to_branch_head(repo: &Path) -> Result<String, String> {
    ensure_clean(repo)?;
    let head =
        |repo: &Path| git_stdout(repo, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"]);
    let head = head(repo).or_else(|_| {
        git(repo, &["remote", "set-head", "origin", "--auto"])?;
        head(repo)
    })?;
    let head = head.trim();
    let branch = head.strip_prefix("origin/").unwrap_or(head);
    let refspec = format!("+refs/heads/{branch}:refs/remotes/origin/{branch}");
    if is_shallow(repo) {
        git(repo, &["fetch", "--no-tags", "--depth", "1", "origin", &refspec])?;
    } else {
        git(repo, &["fetch", "origin", &refspec])?;
    }
    git(repo, &["checkout", "-B", branch, "--track", &format!("origin/{branch}")])?;
    if let Some(sub) = subdir(repo) {
        git(repo, &["sparse-checkout", "set", &sub])?;
    }
    Ok(format!("回到 {branch} 最新的提交（{}）", version(repo).unwrap_or_default()))
}

/// 卸载：删掉克隆下来的目录。只删插件目录下、带 `.git` 的（手动放进来的不动）。
pub fn uninstall(dir: &Path, repo: &Path) -> Result<(), String> {
    let inside = repo.parent().is_some_and(|p| same_dir(p, dir));
    if !inside || !repo.join(".git").exists() {
        return Err("只能卸载从 git 地址装的插件；手动放进来的请在插件目录里自己删".into());
    }
    std::fs::remove_dir_all(repo).map_err(|e| format!("删不掉 {}：{e}", repo.display()))
}

fn same_dir(a: &Path, b: &Path) -> bool {
    crate::git::same_dir(a, b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::explain_git_error;
    use std::process::Command;

    #[test]
    fn addresses_and_folder_names() {
        assert_eq!(
            normalize_url(" acme/review-kit ").unwrap(),
            "https://github.com/acme/review-kit.git"
        );
        assert_eq!(normalize_url("https://x.com/a/b.git").unwrap(), "https://x.com/a/b.git");
        assert_eq!(normalize_url("git@github.com:a/b.git").unwrap(), "git@github.com:a/b.git");
        assert!(normalize_url("--upload-pack=evil").is_err());
        assert!(normalize_url("just-a-word").is_err());
        assert!(normalize_url("").is_err());
        assert_eq!(folder_name("https://github.com/acme/review-kit.git").unwrap(), "review-kit");
        assert_eq!(folder_name("git@github.com:acme/kit").unwrap(), "kit");
        assert_eq!(folder_name("https://x.com/a/b/").unwrap(), "b");
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

    /// 本地起一个 git 仓库当远端：装 → 能被发现、带上 repo → 远端加了提交后更新 → 卸载。
    /// 机器上没有 git 就跳过。
    #[test]
    fn install_update_uninstall_from_a_local_repo() {
        let run = |cwd: &Path, args: &[&str]| {
            let ok = Command::new("git")
                .args(args)
                .current_dir(cwd)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            assert!(ok, "git {args:?}");
        };
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let base = std::env::temp_dir().join(format!("cd-plugin-git-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let remote = base.join("remote").join("demo-kit");
        std::fs::create_dir_all(remote.join("commands")).unwrap();
        std::fs::write(remote.join("commands/hello.md"), "说你好").unwrap();
        run(&remote, &["init", "-q"]);
        run(&remote, &["-c", "user.name=t", "-c", "user.email=t@t", "add", "."]);
        run(&remote, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "one"]);

        let plugins = base.join("plugins");
        let url = format!("file://{}", remote.to_string_lossy().replace('\\', "/"));
        let dest = install(&plugins, &url).unwrap();
        assert_eq!(dest, plugins.join("demo-kit"));
        // 同名再装一次：不覆盖
        assert!(install(&plugins, &url).is_err());
        let found = crate::plugin::discover(&plugins);
        assert_eq!(found[0].commands[0].name, "hello");
        assert_eq!(found[0].repo.as_deref(), Some(dest.as_path()));

        std::fs::write(remote.join("commands/bye.md"), "说再见").unwrap();
        run(&remote, &["-c", "user.name=t", "-c", "user.email=t@t", "add", "."]);
        run(&remote, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "two"]);
        update(&dest).unwrap();
        assert_eq!(crate::plugin::discover(&plugins)[0].commands.len(), 2);

        // 手动放进来的（没有 .git）不让卸载
        std::fs::create_dir_all(plugins.join("manual")).unwrap();
        assert!(uninstall(&plugins, &plugins.join("manual")).is_err());
        uninstall(&plugins, &dest).unwrap();
        assert!(!dest.exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 只装市场仓库里的一个插件：只检出那个目录，扫出来的就是它，不带市场里别的插件。
    #[test]
    fn installs_one_plugin_out_of_a_marketplace() {
        let run = |cwd: &Path, args: &[&str]| {
            let ok = Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .current_dir(cwd)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            assert!(ok, "git {args:?}");
        };
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let base = std::env::temp_dir().join(format!("cd-plugin-sub-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let remote = base.join("market");
        let write = |p: PathBuf, t: &str| {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, t).unwrap();
        };
        write(
            remote.join(".claude-plugin/marketplace.json"),
            r#"{"name": "m", "plugins": [
                {"name": "alpha", "source": "./plugins/alpha"},
                {"name": "beta", "source": "./plugins/beta"}
            ]}"#,
        );
        write(remote.join("plugins/alpha/commands/go.md"), "出发");
        write(remote.join("plugins/beta/commands/stop.md"), "停下");
        run(&remote, &["init", "-q"]);
        run(&remote, &["add", "."]);
        run(&remote, &["commit", "-qm", "one"]);

        let plugins = base.join("plugins");
        let url = format!("file://{}", remote.to_string_lossy().replace('\\', "/"));
        assert!(install_subdir(&plugins, &url, "../x", "alpha").is_err());
        let dest = install_subdir(&plugins, &url, "plugins/alpha", "alpha").unwrap();
        assert_eq!(subdir(&dest).as_deref(), Some("plugins/alpha"));
        assert!(!dest.join("plugins/beta").exists());
        let found = crate::plugin::discover(&plugins);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "alpha");
        assert_eq!(found[0].commands[0].name, "go");
        assert_eq!(found[0].repo.as_deref(), Some(dest.as_path()));

        write(remote.join("plugins/alpha/commands/again.md"), "再来");
        run(&remote, &["add", "."]);
        run(&remote, &["commit", "-qm", "two"]);
        update(&dest).unwrap();
        assert_eq!(crate::plugin::discover(&plugins)[0].commands.len(), 2);
        uninstall(&plugins, &dest).unwrap();
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 按版本装：远端打了 tag 的旧版本，`install_at` 装下来的就是那一版的子目录，
    /// 之后能列出有哪些版本、换到别的版本、换回最新。机器上没有 git 就跳过。
    #[test]
    fn installs_and_switches_between_tagged_versions() {
        let run = |cwd: &Path, args: &[&str]| {
            let ok = Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .current_dir(cwd)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            assert!(ok, "git {args:?}");
        };
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let base = std::env::temp_dir().join(format!("cd-plugin-tags-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let remote = base.join("market");
        let write = |p: PathBuf, t: &str| {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, t).unwrap();
        };
        write(
            remote.join(".claude-plugin/marketplace.json"),
            r#"{"name": "m", "plugins": [{"name": "alpha", "source": "./plugins/alpha"}]}"#,
        );
        write(remote.join("plugins/alpha/commands/go.md"), "出发");
        run(&remote, &["init", "--initial-branch=main"]);
        run(&remote, &["add", "."]);
        run(&remote, &["commit", "-qm", "one"]);
        run(&remote, &["tag", "v1.0.0"]);
        write(remote.join("plugins/alpha/commands/again.md"), "再来");
        run(&remote, &["add", "."]);
        run(&remote, &["commit", "-qm", "two"]);
        run(&remote, &["tag", "v2.0.0"]);

        let plugins = base.join("plugins");
        let url = format!("file://{}", remote.to_string_lossy().replace('\\', "/"));
        // 装老版本：只有那一版的指令，子路径记下来了（扫描认得出这是插件而不是市场）
        let dest = install_at(&plugins, &url, "v1.0.0", Some("plugins/alpha"), "alpha").unwrap();
        assert_eq!(subdir(&dest).as_deref(), Some("plugins/alpha"));
        assert_eq!(version(&dest).as_deref(), Some("v1.0.0"));
        let found = crate::plugin::discover(&plugins);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].commands.len(), 1, "{:?}", found[0].commands);
        assert!(!dest.join("plugins/beta").exists());
        assert!(!dest.join("plugins/alpha/commands/again.md").exists());

        // 换到新版本、再换回去
        assert_eq!(versions(&dest).unwrap()[0], "v2.0.0");
        assert_eq!(checkout(&dest, "v2.0.0").unwrap(), "v2.0.0");
        assert_eq!(crate::plugin::discover(&plugins)[0].commands.len(), 2);
        assert_eq!(checkout(&dest, "v1.0.0").unwrap(), "v1.0.0");
        assert_eq!(crate::plugin::discover(&plugins)[0].commands.len(), 1);
        // 停在某个版本上点「更新」：回到 main 的头，之后远端再有新提交照常快进
        assert!(update(&dest).unwrap().contains("main"), "从版本回到分支");
        assert_eq!(crate::plugin::discover(&plugins)[0].commands.len(), 2);
        write(remote.join("plugins/alpha/commands/third.md"), "第三");
        run(&remote, &["add", "."]);
        run(&remote, &["commit", "-qm", "three"]);
        update(&dest).unwrap();
        assert_eq!(crate::plugin::discover(&plugins)[0].commands.len(), 3);
        assert!(!dest.join("plugins/beta").exists(), "回到分支后仍只检出那个子目录");
        // 本地改过：不让换（免得把改动悄悄冲掉）
        write(dest.join("plugins/alpha/commands/local.md"), "本地");
        assert!(checkout(&dest, "v2.0.0").is_err());
        // 不存在的版本：装不上，也不留半个目录
        let bad = plugins.join("beta");
        assert!(install_at(&plugins, &url, "v9.9.9", None, "beta").is_err());
        assert!(!bad.exists());
        uninstall(&plugins, &dest).unwrap();
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 跟最新装的（`--depth 1` 浅克隆）本地只有最新那个提交：版本列表要问远端，
    /// 换到老版本要先把它拉下来，换完还能「更新」回分支的头。
    #[test]
    fn shallow_install_lists_and_switches_to_remote_tags() {
        let run = |cwd: &Path, args: &[&str]| {
            let ok = Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .current_dir(cwd)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            assert!(ok, "git {args:?}");
        };
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let base = std::env::temp_dir().join(format!("cd-plugin-shallow-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let remote = base.join("market");
        let write = |p: PathBuf, t: &str| {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, t).unwrap();
        };
        write(
            remote.join(".claude-plugin/marketplace.json"),
            r#"{"name": "m", "plugins": [{"name": "alpha", "source": "./plugins/alpha"}]}"#,
        );
        write(remote.join("plugins/alpha/commands/go.md"), "出发");
        run(&remote, &["init", "--initial-branch=main"]);
        run(&remote, &["add", "."]);
        run(&remote, &["commit", "-qm", "one"]);
        run(&remote, &["tag", "v1.0.0"]);
        write(remote.join("plugins/alpha/commands/again.md"), "再来");
        run(&remote, &["add", "."]);
        run(&remote, &["commit", "-qm", "two"]);
        run(&remote, &["tag", "v2.0.0"]);
        write(remote.join("plugins/alpha/commands/third.md"), "第三");
        run(&remote, &["add", "."]);
        run(&remote, &["commit", "-qm", "three"]);

        let plugins = base.join("plugins");
        let url = format!("file://{}", remote.to_string_lossy().replace('\\', "/"));
        let dest = install_at(&plugins, &url, "", Some("plugins/alpha"), "alpha").unwrap();
        assert_eq!(crate::plugin::discover(&plugins)[0].commands.len(), 3);
        // 本地只有最新的提交，版本列表照样有远端的全部 tag
        assert_eq!(versions(&dest).unwrap(), vec!["v2.0.0".to_string(), "v1.0.0".to_string()]);
        assert_eq!(checkout(&dest, "v1.0.0").unwrap(), "v1.0.0");
        assert_eq!(crate::plugin::discover(&plugins)[0].commands.len(), 1);
        assert!(!dest.join("plugins/beta").exists());
        assert!(update(&dest).unwrap().contains("main"));
        assert_eq!(crate::plugin::discover(&plugins)[0].commands.len(), 3);
        uninstall(&plugins, &dest).unwrap();
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn marketplace_repo_lists_its_plugins() {
        let dir = std::env::temp_dir().join(format!("cd-plugin-market-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let repo = dir.join("market");
        let write = |p: PathBuf, t: &str| {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, t).unwrap();
        };
        write(
            repo.join(".claude-plugin/marketplace.json"),
            r#"{"name": "m", "plugins": [
                {"name": "alpha", "source": "./plugins/alpha", "description": "第一个"},
                {"name": "remote", "source": {"source": "github", "repo": "a/b"}}
            ]}"#,
        );
        write(repo.join("plugins/alpha/commands/go.md"), "出发");
        let found = crate::plugin::discover(&dir);
        let names: Vec<&str> = found.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["alpha", "remote"]);
        assert_eq!(found[0].description, "第一个");
        assert_eq!(found[0].commands[0].name, "go");
        assert!(!found[1].problems.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

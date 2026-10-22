//! 从 git 仓库装插件：克隆到插件目录、更新（`git pull`）、卸载（删掉克隆下来的目录）。
//!
//! 地址可以是 `https://…`、`ssh://…`、`git@host:路径`，或 GitHub 的简写 `作者/仓库`。
//! 一个仓库可以是一个插件，也可以是 Claude Code 的插件市场（见 [`crate::plugin::discover`]）。
//! 装好的插件和手动放进来的一样默认停用。
//!
//! 也可以只装插件市场仓库里的一个插件（[`install_subdir`]，设置页的「推荐」走这个）：
//! 稀疏克隆，只检出那个插件的目录，子路径记在 `.git/` 里（[`subdir`]），扫描时按它找插件根目录。
//!
//! 都是阻塞调用（界面在后台线程跑）。git 不会弹窗要密码：私有仓库要先在命令行配好凭据。

use crate::git::git;
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

/// 拉最新的（只快进，本地改过就报错、不合并）；返回 git 的输出（「Already up to date.」等）。
pub fn update(repo: &Path) -> Result<String, String> {
    git(repo, &["pull", "--ff-only"]).map_err(|e| {
        // 本地改过的插件目录：插件这边最省事的出路是重装
        if e.starts_with("目录里有本地改动") {
            format!("{e}；或者卸载后重新安装")
        } else {
            e
        }
    })
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

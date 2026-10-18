//! 从 git 仓库装插件：克隆到插件目录、更新（`git pull`）、卸载（删掉克隆下来的目录）。
//!
//! 地址可以是 `https://…`、`ssh://…`、`git@host:路径`，或 GitHub 的简写 `作者/仓库`。
//! 一个仓库可以是一个插件，也可以是 Claude Code 的插件市场（见 [`crate::plugin::discover`]）。
//! 装好的插件和手动放进来的一样默认停用。
//!
//! 都是阻塞调用（界面在后台线程跑）。git 不会弹窗要密码：私有仓库要先在命令行配好凭据。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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

/// 拉最新的（只快进，本地改过就报错、不合并）；返回 git 的输出（「Already up to date.」等）。
pub fn update(repo: &Path) -> Result<String, String> {
    git(repo, &["pull", "--ff-only"])
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
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// 跑一条 git 命令：不弹黑框、不问密码；失败时带上 git 的报错（最后几行）。
fn git(cwd: &Path, args: &[&str]) -> Result<String, String> {
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
        let out = text(&output.stdout);
        Ok(if out.is_empty() { text(&output.stderr) } else { out })
    } else {
        let err = text(&output.stderr);
        let tail: Vec<&str> = err.lines().rev().take(3).collect();
        let tail: Vec<&str> = tail.into_iter().rev().collect();
        Err(if tail.is_empty() { "git 失败了".into() } else { tail.join("\n") })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

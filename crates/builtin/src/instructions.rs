//! 项目说明：把用户级说明（`~/.claude/CLAUDE.md`）和工作目录到仓库根之间各级目录里的
//! `AGENTS.md` / `CLAUDE.md` 读进系统提示词，让模型知道项目约定（注释语言、目录结构、
//! 构建命令等）。每轮开始时重读，用户中途改了说明下一轮就生效。

use std::path::{Path, PathBuf};

/// 每级目录里认的文件名（都在就都读，内容相同只读一份）
const FILE_NAMES: [&str; 2] = ["AGENTS.md", "CLAUDE.md"];
/// 单个文件最多读进来的字符数
const FILE_MAX_CHARS: usize = 20_000;
/// 全部说明合计的字符上限：超出后靠后（更具体）的文件优先保留
const TOTAL_MAX_CHARS: usize = 40_000;

/// 用户级说明文件的默认位置：`~/.claude/CLAUDE.md`。
pub fn default_user_file() -> Option<PathBuf> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    Some(PathBuf::from(home).join(".claude").join("CLAUDE.md"))
}

/// 读到的一份说明。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instruction {
    pub path: PathBuf,
    pub text: String,
}

/// 从 `cwd` 往上到第一个含 `.git` 的目录（含）为止，返回「根 → cwd」顺序的各级目录。
/// 一直在往上都没找到仓库根时只保留 `cwd` 本身：仓库外的上级目录不是这个项目的约定。
fn dirs_up_to_root(cwd: &Path, is_root: impl Fn(&Path) -> bool) -> Vec<&Path> {
    let mut dirs: Vec<&Path> = Vec::new();
    let mut root_found = false;
    for dir in cwd.ancestors() {
        dirs.push(dir);
        if is_root(dir) {
            root_found = true;
            break;
        }
    }
    if !root_found {
        dirs.truncate(1);
    }
    dirs.reverse();
    dirs
}

/// 按「通用 → 具体」的顺序列出要读的文件：用户级，然后从仓库根往下到 `cwd`。
fn candidates(cwd: &Path, user_file: Option<&Path>) -> Vec<PathBuf> {
    let dirs = dirs_up_to_root(cwd, |dir| dir.join(".git").exists());
    let mut out: Vec<PathBuf> = user_file.map(Path::to_path_buf).into_iter().collect();
    for dir in dirs {
        out.extend(FILE_NAMES.iter().map(|name| dir.join(name)));
    }
    out
}

/// 读出所有存在且非空的说明，去重、截断。
pub fn load(cwd: &Path, user_file: Option<&Path>) -> Vec<Instruction> {
    let mut found: Vec<Instruction> = Vec::new();
    for path in candidates(cwd, user_file) {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let text = clip(text.trim(), FILE_MAX_CHARS);
        if text.is_empty() || found.iter().any(|f| f.text == text) {
            continue;
        }
        found.push(Instruction { path, text });
    }
    // 合计超限时从最通用的（最前面的）开始丢
    let mut total: usize = found.iter().map(|f| f.text.chars().count()).sum();
    while total > TOTAL_MAX_CHARS && found.len() > 1 {
        total -= found.remove(0).text.chars().count();
    }
    found
}

/// 截到 `max` 个字符，截掉了就注明。
fn clip(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((cut, _)) => format!("{}\n\n[… truncated]", &text[..cut]),
        None => text.to_string(),
    }
}

/// 读某个说明文件（聊天说明用：只有一份，没有项目里的那些）。
pub fn load_file(path: &Path) -> Vec<Instruction> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let text = clip(text.trim(), FILE_MAX_CHARS);
    if text.is_empty() {
        return Vec::new();
    }
    vec![Instruction { path: path.to_path_buf(), text }]
}

/// 聊天会话的说明：只有设置里写的「聊天说明」一份（没有项目说明、没有用户级 CLAUDE.md）。
pub fn chat_instructions(file: Option<&Path>) -> Vec<Instruction> {
    file.map(load_file).unwrap_or_default()
}

/// 拼成系统提示词里的一节；没有说明返回 None。
pub fn prompt_section(found: &[Instruction]) -> Option<String> {
    if found.is_empty() {
        return None;
    }
    let mut out = String::from(
        "# User and project instructions\n\
         The following instructions come from the user and from files in the project. \
         Follow them; they take precedence over the general guidance above. \
         Later (more specific) files take precedence over earlier ones.",
    );
    for f in found {
        out.push_str(&format!("\n\n## {}\n\n{}", f.path.display(), f.text));
    }
    Some(out)
}

/// 聊天会话那一节（用户的「聊天说明」）：只有一份，没有项目、没有文件路径。
pub fn chat_prompt_section(found: &[Instruction]) -> Option<String> {
    if found.is_empty() {
        return None;
    }
    let mut out = String::from(
        "# User instructions\n\
         The user wrote these for every chat. Follow them; they take precedence over the \
         general guidance above.",
    );
    for f in found {
        out.push_str("\n\n");
        out.push_str(&f.text);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cd-instr-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn reads_from_repo_root_down_to_cwd() {
        let root = temp("repo");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("apps/web")).unwrap();
        std::fs::write(root.join("AGENTS.md"), "根目录约定").unwrap();
        // 同内容的 CLAUDE.md（常见的软链 / 复制）只读一份
        std::fs::write(root.join("CLAUDE.md"), "根目录约定\n").unwrap();
        std::fs::write(root.join("apps/web/CLAUDE.md"), "子目录约定").unwrap();
        let user = root.join("user.md");
        std::fs::write(&user, "用户偏好").unwrap();

        let found = load(&root.join("apps/web"), Some(&user));
        let texts: Vec<&str> = found.iter().map(|f| f.text.as_str()).collect();
        assert_eq!(texts, ["用户偏好", "根目录约定", "子目录约定"]);
        let section = prompt_section(&found).unwrap();
        assert!(section.contains("子目录约定") && section.contains("AGENTS.md"), "{section}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 仓库外的上级目录不读：临时目录本身可能在某个仓库里（CI 上常把 TEMP 放进工作目录），
    /// 所以这里用假的「是不是仓库根」判断，不依赖 temp 的真实位置。
    #[test]
    fn outside_a_repo_only_reads_cwd() {
        let base = Path::new("base");
        let cwd = base.join("sub");
        let dirs = dirs_up_to_root(&cwd, |dir| dir.ends_with("repo"));
        assert_eq!(dirs, [cwd.as_path()]);

        // 找不到仓库根时，多深的 cwd 都只看自己
        let deep = base.join("a/b/c/d");
        assert_eq!(dirs_up_to_root(&deep, |_| false), [deep.as_path()]);

        // 找到仓库根：根 → cwd 各级都在，且是从通用到具体
        let dirs = dirs_up_to_root(&cwd, |dir| dir == base);
        assert_eq!(dirs, [base, cwd.as_path()]);
        assert_eq!(prompt_section(&[]), None);
    }

    /// 实际读文件的那条路：仓库根那一级和 cwd 自己那一级都读，根在前。
    #[test]
    fn load_reads_root_then_cwd() {
        let dir = temp("root-then-cwd");
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("AGENTS.md"), "上级").unwrap();
        std::fs::write(dir.join("sub/AGENTS.md"), "本级").unwrap();
        let texts =
            |cwd: &Path| -> Vec<String> { load(cwd, None).into_iter().map(|f| f.text).collect() };
        assert_eq!(texts(&dir), ["上级"]);
        assert_eq!(texts(&dir.join("sub")), ["上级", "本级"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clips_long_files() {
        let text = clip(&"字".repeat(FILE_MAX_CHARS + 10), FILE_MAX_CHARS);
        assert!(text.ends_with("[… truncated]"));
        assert_eq!(text.chars().filter(|c| *c == '字').count(), FILE_MAX_CHARS);
    }
}

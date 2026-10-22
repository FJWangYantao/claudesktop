//! 聊天的文件附件：把文件内容**内联**进用户消息（见「设计说明-聊天与Code分离.md」5.5）。
//!
//! 代码会话的附件只是把路径贴进输入框，等 agent 自己 `read_file`；聊天没有这个工具，
//! 只能把内容直接带上。这里只管「读进来能不能带、怎么拼成一段文字」，界面部分在
//! `apps/desktop/src/app/attach.rs`。
//!
//! 上限是给上下文留余量：单个 256 KB、一条消息合计 1 MB，超了让用户去代码空间。

use std::path::{Path, PathBuf};

/// 单个文本文件最多带多少字节（约 256 KB，中文约 8 万字）
pub const MAX_FILE_BYTES: usize = 256 * 1024;
/// 一条消息里的附件合计最多多少字节
pub const MAX_TOTAL_BYTES: usize = 1024 * 1024;
/// 判断「是不是文本」看的字节数：前这么多里出现 NUL 就当二进制
pub const SNIFF_BYTES: usize = 8 * 1024;

/// 一份要内联进消息的文本附件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextAttachment {
    /// 显示用的文件名（不含目录）
    pub name: String,
    /// 原始路径（拖回来的、悬停提示、去重用）
    pub path: PathBuf,
    /// 文件内容（已去掉 BOM）
    pub text: String,
}

impl TextAttachment {
    /// 字节数（按内容算，不是磁盘上的大小：去掉 BOM 后才是真带上上下文的量）
    pub fn bytes(&self) -> usize {
        self.text.len()
    }

    /// 给人看的大小，如 `12.4 KB`
    pub fn size_label(&self) -> String {
        size_label(self.bytes())
    }
}

/// 读文件失败的原因（都是给用户看的中文，界面直接 toast 出去）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachError {
    /// 读不了（不存在、没有权限、是目录……）
    Unreadable(String),
    /// 二进制 / 不是 UTF-8 文本
    NotText(String),
    /// 单个文件太大
    TooLarge { name: String, bytes: usize },
    /// 加上它就超过一条消息的合计上限
    TotalTooLarge { name: String, bytes: usize },
}

impl AttachError {
    /// 界面里 toast 的那句话。
    pub fn message(&self) -> String {
        match self {
            Self::Unreadable(why) => format!("读不了这个文件：{why}"),
            Self::NotText(name) => {
                format!("聊天里暂时只能带文本文件和图片（{name} 不是文本）")
            }
            Self::TooLarge { name, bytes } => format!(
                "{name} 太大（{}），聊天里单个文件最多带 {}；要处理大文件请在代码空间里开会话",
                size_label(*bytes),
                size_label(MAX_FILE_BYTES)
            ),
            Self::TotalTooLarge { name, bytes } => format!(
                "加上 {name}（{}）就超过一条消息 {} 的上限了；少带几个文件，或者去代码空间里开会话",
                size_label(*bytes),
                size_label(MAX_TOTAL_BYTES)
            ),
        }
    }
}

impl std::fmt::Display for AttachError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

/// 读一个文件成文本附件。按**内容**判断是不是文本（不看扩展名）：
/// 前 [`SNIFF_BYTES`] 里没有 NUL、能按 UTF-8 解码；带 BOM 的去掉。
pub fn read_text(path: &Path) -> Result<TextAttachment, AttachError> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let meta = std::fs::metadata(path).map_err(|e| AttachError::Unreadable(io_reason(e)))?;
    if meta.is_dir() {
        return Err(AttachError::Unreadable("这是一个文件夹".into()));
    }
    // 磁盘上就超了：不必读进来再判
    if meta.len() as usize > MAX_FILE_BYTES {
        return Err(AttachError::TooLarge { name, bytes: meta.len() as usize });
    }
    let bytes = std::fs::read(path).map_err(|e| AttachError::Unreadable(io_reason(e)))?;
    let head = &bytes[..bytes.len().min(SNIFF_BYTES)];
    if head.contains(&0) {
        return Err(AttachError::NotText(name));
    }
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(_) => return Err(AttachError::NotText(name)),
    };
    let text = text.strip_prefix('\u{feff}').map(str::to_string).unwrap_or(text);
    if text.len() > MAX_FILE_BYTES {
        return Err(AttachError::TooLarge { name, bytes: text.len() });
    }
    Ok(TextAttachment { name, path: path.to_path_buf(), text })
}

/// 再收一份时会不会超过合计上限（界面在加附件前先问一次，好在加之前就 toast）。
pub fn fits_total(have: &[TextAttachment], extra: &TextAttachment) -> bool {
    total_bytes(have) + extra.bytes() <= MAX_TOTAL_BYTES
}

/// 这些附件的合计字节数。
pub fn total_bytes(attachments: &[TextAttachment]) -> usize {
    attachments.iter().map(TextAttachment::bytes).sum()
}

/// 发给内核的全文：附件内容在前（各一段 `<attachment>`），用户打的字在后。
///
/// 用户一个字都没打时只留附件——不必留一堆空行。
pub fn compose(user_text: &str, attachments: &[TextAttachment]) -> String {
    if attachments.is_empty() {
        return user_text.to_string();
    }
    let mut out = String::new();
    for (i, a) in attachments.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        // 名字里的引号 / 尖括号会把标签弄坏，换成近似的全角（文件名照原样在界面上显示）
        let name = a.name.replace('"', "＂").replace('<', "＜").replace('>', "＞");
        out.push_str(&format!("<attachment name=\"{name}\">\n{}\n</attachment>\n", a.text));
    }
    if !user_text.trim().is_empty() {
        out.push('\n');
        out.push_str(user_text);
    }
    out.trim_end().to_string()
}

/// 对话里用户那条气泡显示的文字：用户打的字 + 附件名（不展开内容）。
/// 一个字都没打时只有附件名。没有附件时原样返回（不 trim，免得改掉用户写的空行）。
pub fn display_text(user_text: &str, attachments: &[TextAttachment]) -> String {
    if attachments.is_empty() {
        return user_text.to_string();
    }
    let names = attachments.iter().map(|a| a.name.as_str()).collect::<Vec<_>>();
    let text = user_text.trim();
    match text.is_empty() {
        // 只有附件时也带 📎：`strip_display` 靠它认出这一行不是用户打的字
        true => format!("📎 {}", names.join("、")),
        false => format!("{text}\n\n📎 {}", names.join("、")),
    }
}

/// 气泡里的文字 → 用户真正打的字（去掉 `display_text` 加上的那行附件名）。
/// 重发 / 换内核重跑时用：附件本身已经不在手里了，只需把用户的话捞回来。
pub fn strip_display(shown: &str) -> &str {
    match shown.rfind("\n\n📎 ") {
        Some(at) => &shown[..at],
        // 只有附件、没打字
        None if shown.starts_with("📎 ") => "",
        None => shown,
    }
}

/// 给人看的大小：`812 B` / `12.4 KB` / `1.2 MB`。
pub fn size_label(bytes: usize) -> String {
    const KB: f64 = 1024.;
    let b = bytes as f64;
    if b < KB {
        format!("{bytes} B")
    } else if b < KB * KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{:.1} MB", b / (KB * KB))
    }
}

fn io_reason(err: std::io::Error) -> String {
    match err.kind() {
        std::io::ErrorKind::NotFound => "文件不存在".into(),
        std::io::ErrorKind::PermissionDenied => "没有读取权限".into(),
        _ => err.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "claudesktop-attach-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    fn write(path: &Path, bytes: &[u8]) {
        std::fs::write(path, bytes).unwrap();
    }

    fn keep(text: &str) -> TextAttachment {
        TextAttachment {
            name: "notes.md".into(),
            path: PathBuf::from("notes.md"),
            text: text.into(),
        }
    }

    #[test]
    fn reads_utf8_text_and_strips_bom() {
        let path = temp_file("a.md");
        write(&path, "\u{feff}# 标题\n正文".as_bytes());
        let a = read_text(&path).unwrap();
        assert_eq!(a.text, "# 标题\n正文");
        assert_eq!(a.name, "a.md");
        assert!(!a.text.starts_with('\u{feff}'));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn binary_and_bad_utf8_are_refused() {
        let nul = temp_file("a.bin");
        write(&nul, b"abc\0def");
        assert!(matches!(read_text(&nul), Err(AttachError::NotText(_))));
        let bad = temp_file("b.bin");
        write(&bad, &[0xff, 0xfe, 0x41]);
        assert!(matches!(read_text(&bad), Err(AttachError::NotText(_))));
        let _ = std::fs::remove_dir_all(nul.parent().unwrap());
    }

    #[test]
    fn oversize_single_file_is_refused() {
        let path = temp_file("big.txt");
        write(&path, &vec![b'a'; MAX_FILE_BYTES + 1]);
        match read_text(&path) {
            Err(AttachError::TooLarge { bytes, .. }) => assert_eq!(bytes, MAX_FILE_BYTES + 1),
            other => panic!("{other:?}"),
        }
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn compose_puts_attachments_before_the_text() {
        let a = keep("第一行\n第二行");
        let out = compose("帮我看看", &[a]);
        assert!(out.starts_with("<attachment name=\"notes.md\">\n第一行\n第二行\n</attachment>"));
        assert!(out.ends_with("帮我看看"), "{out}");
        // 没有附件时原样
        assert_eq!(compose("你好", &[]), "你好");
        // 没有文字时只有附件，末尾不留空行
        let only = compose("   ", &[keep("x")]);
        assert!(only.ends_with("</attachment>"), "{only}");
    }

    #[test]
    fn compose_escapes_quotes_and_angles_in_names() {
        let mut a = keep("x");
        a.name = "a\"b<c>.md".into();
        let out = compose("", &[a]);
        assert!(out.starts_with("<attachment name=\"a＂b＜c＞.md\">"), "{out}");
    }

    #[test]
    fn display_text_lists_names_after_the_text() {
        let a = keep("x");
        assert_eq!(display_text("看这个", std::slice::from_ref(&a)), "看这个\n\n📎 notes.md");
        assert_eq!(display_text("  ", std::slice::from_ref(&a)), "📎 notes.md");
        // 没有附件时原样（不 trim）
        assert_eq!(display_text("看这个\n", &[]), "看这个\n");
        let mut b = keep("y");
        b.name = "b.txt".into();
        assert!(display_text("", &[a, b]).contains("notes.md、b.txt"));
    }

    #[test]
    fn strip_display_recovers_the_typed_text() {
        let a = keep("x");
        let shown = display_text("看这个", std::slice::from_ref(&a));
        assert_eq!(strip_display(&shown), "看这个");
        // 没有附件行时原样
        assert_eq!(strip_display("看这个"), "看这个");
        // 只有附件时剩下空串
        assert_eq!(strip_display(&display_text("", std::slice::from_ref(&a))), "");
    }

    #[test]
    fn totals_are_capped() {
        let half = keep(&"a".repeat(MAX_TOTAL_BYTES / 2));
        assert!(fits_total(&[], &half));
        assert!(fits_total(std::slice::from_ref(&half), &half));
        assert!(!fits_total(&[half.clone(), half.clone()], &half));
        assert_eq!(total_bytes(&[half.clone(), half]), MAX_TOTAL_BYTES);
    }

    #[test]
    fn size_labels_are_readable() {
        assert_eq!(size_label(812), "812 B");
        assert_eq!(size_label(1024), "1.0 KB");
        assert_eq!(size_label(1024 * 1024 * 3 / 2), "1.5 MB");
    }
}

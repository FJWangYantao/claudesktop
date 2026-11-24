//! 文件类工具：读、写、编辑、列目录、glob、grep。

use super::*;

/// `path` 参数为空时用工作目录
pub(super) fn dir_or_cwd(cwd: &Path, path: Option<String>) -> PathBuf {
    path.filter(|p| !p.trim().is_empty()).map_or_else(|| cwd.to_path_buf(), |p| resolve(cwd, &p))
}

/// 遍历 `root` 下的文件（遵守 .gitignore / .ignore，跳过隐藏文件；不要求是 git 仓库）
pub(super) fn walk(root: &Path) -> ignore::Walk {
    ignore::WalkBuilder::new(root).require_git(false).follow_links(false).build()
}

/// 相对 `root` 的路径，统一用 `/`
pub(super) fn rel_slash(root: &Path, path: &Path) -> String {
    path.strip_prefix(root).unwrap_or(path).to_string_lossy().replace('\\', "/")
}

pub(super) fn glob_files(
    pattern: &str,
    root: &Path,
    cwd: &Path,
    should_stop: &dyn Fn() -> bool,
) -> ToolOutput {
    if !root.is_dir() {
        return ToolOutput::fail(format!("Not a directory: {}", root.display()), "目录不存在");
    }
    let matcher = match globset::GlobBuilder::new(pattern).literal_separator(true).build() {
        Ok(g) => g.compile_matcher(),
        Err(e) => return ToolOutput::fail(format!("Invalid pattern: {e}"), "模式无效"),
    };
    // 不含 `/` 的模式（如 `*.rs`）按文件名匹配任意层级，和常见工具的习惯一致
    let by_name = !pattern.contains('/');
    let mut hits: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for entry in walk(root).flatten() {
        if should_stop() {
            return ToolOutput::fail("Cancelled", "已取消");
        }
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        let ok = if by_name {
            path.file_name().is_some_and(|n| matcher.is_match(n))
        } else {
            matcher.is_match(rel_slash(root, path))
        };
        if ok {
            let mtime = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .unwrap_or(std::time::UNIX_EPOCH);
            hits.push((mtime, path.to_path_buf()));
        }
    }
    hits.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let total = hits.len();
    if total == 0 {
        return ToolOutput::ok("No files found", "0 个文件");
    }
    let mut out = String::new();
    for (_, path) in hits.iter().take(GLOB_MAX) {
        let _ = writeln!(out, "{}", display(cwd, path));
    }
    if total > GLOB_MAX {
        let _ = writeln!(out, "… {} more files; narrow the pattern", total - GLOB_MAX);
    }
    ToolOutput::ok(out, format!("{total} 个文件"))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn grep_files(
    pattern: &str,
    root: &Path,
    include: Option<&str>,
    ignore_case: bool,
    files_only: bool,
    cwd: &Path,
    should_stop: &dyn Fn() -> bool,
) -> ToolOutput {
    let re = match regex::RegexBuilder::new(pattern).case_insensitive(ignore_case).build() {
        Ok(r) => r,
        Err(e) => return ToolOutput::fail(format!("Invalid regex: {e}"), "正则无效"),
    };
    let include = match include.map(globset::Glob::new).transpose() {
        Ok(g) => g.map(|g| g.compile_matcher()),
        Err(e) => return ToolOutput::fail(format!("Invalid include glob: {e}"), "模式无效"),
    };
    if !root.exists() {
        return ToolOutput::fail(
            format!("No such file or directory: {}", root.display()),
            "路径不存在",
        );
    }
    let mut out = String::new();
    let (mut lines, mut files, mut truncated) = (0usize, 0usize, false);
    'walk: for entry in walk(root).flatten() {
        if should_stop() {
            return ToolOutput::fail("Cancelled", "已取消");
        }
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        // include 按文件名匹配（`*.rs`），含 `/` 时按相对路径
        if let Some(m) = &include {
            let ok = match include_has_slash(m) {
                true => m.is_match(rel_slash(root, path)),
                false => path.file_name().is_some_and(|n| m.is_match(n)),
            };
            if !ok {
                continue;
            }
        }
        if entry.metadata().is_ok_and(|m| m.len() > GREP_MAX_FILE_BYTES) {
            continue;
        }
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        // 二进制文件：前 8KB 里有 NUL
        if bytes.iter().take(8192).any(|b| *b == 0) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        let mut matched = false;
        for (no, line) in text.lines().enumerate() {
            if !re.is_match(line) {
                continue;
            }
            matched = true;
            if files_only {
                break;
            }
            if lines >= GREP_MAX_LINES {
                truncated = true;
                break 'walk;
            }
            let line = truncate_chars(line.trim_end(), GREP_LINE_CHARS);
            let _ = writeln!(out, "{}:{}: {line}", display(cwd, path), no + 1);
            lines += 1;
        }
        if matched {
            files += 1;
            if files_only {
                if files > GREP_MAX_FILES {
                    truncated = true;
                    break;
                }
                let _ = writeln!(out, "{}", display(cwd, path));
            }
        }
    }
    if files == 0 {
        return ToolOutput::ok("No matches", "无匹配");
    }
    if truncated {
        out.push_str("… results truncated; narrow the pattern or path\n");
    }
    let summary = if files_only {
        format!("{} 个文件", files.min(GREP_MAX_FILES))
    } else {
        format!("{lines} 处匹配{}", if truncated { "（已截断）" } else { "" })
    };
    ToolOutput::ok(clip_output(out), summary)
}

/// GlobMatcher 不暴露原模式里有没有 `/`，看它的 glob 文本
pub(super) fn include_has_slash(m: &globset::GlobMatcher) -> bool {
    m.glob().glob().contains('/')
}

pub(super) fn resolve(cwd: &Path, p: &str) -> PathBuf {
    let p = Path::new(p.trim());
    normalize(&if p.is_absolute() { p.to_path_buf() } else { cwd.join(p) })
}

/// 词法规整（去掉 `.` / `..`），不访问文件系统。
pub(super) fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// 工作目录内显示相对路径（`/` 分隔），否则原样。
pub(super) fn display(cwd: &Path, path: &Path) -> String {
    match path.strip_prefix(normalize(cwd)) {
        Ok(rel) if rel.as_os_str().is_empty() => ".".into(),
        Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
        Err(_) => path.display().to_string(),
    }
}

pub(super) fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}

/// 过长的结果保留首尾各一半
pub(super) fn clip_output(s: String) -> String {
    let n = s.chars().count();
    if n <= OUTPUT_MAX_CHARS {
        return s;
    }
    let half = OUTPUT_MAX_CHARS / 2;
    let head: String = s.chars().take(half).collect();
    let tail: String = s.chars().skip(n - half).collect();
    format!("{head}\n… [{} chars truncated] …\n{tail}", n - OUTPUT_MAX_CHARS)
}

/// 后台任务的输出只留最近的：超了丢最前面的（日志越新越有用）。
pub(super) fn clip_output_front(s: &str) -> String {
    let n = s.chars().count();
    if n <= OUTPUT_MAX_CHARS {
        return s.to_string();
    }
    let tail: String = s.chars().skip(n - OUTPUT_MAX_CHARS).collect();
    format!("… [{} chars truncated] …\n{tail}", n - OUTPUT_MAX_CHARS)
}

pub(super) fn read_file(path: &Path, offset: usize, limit: usize) -> ToolOutput {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) => {
            return ToolOutput::fail(format!("Cannot read {}: {e}", path.display()), "读取失败");
        }
    };
    if meta.is_dir() {
        return ToolOutput::fail(
            format!("{} is a directory; use list_dir", path.display()),
            "是目录",
        );
    }
    if meta.len() > READ_MAX_BYTES {
        return ToolOutput::fail(
            format!("{} is too large ({} bytes)", path.display(), meta.len()),
            "文件过大",
        );
    }
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            return ToolOutput::fail(format!("Cannot read {}: {e}", path.display()), "读取失败");
        }
    };
    // 图片：不按文本读，交给模型看（见 `read_image`）。放在二进制判断之前——PNG 里就有 NUL
    if let Some(media_type) = image_media_type(&bytes) {
        return read_image(path, &bytes, media_type);
    }
    if bytes[..bytes.len().min(8192)].contains(&0) {
        return ToolOutput::fail(
            format!("{} looks like a binary file", path.display()),
            "二进制文件",
        );
    }
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    if total == 0 {
        return ToolOutput::ok("(empty file)", "空文件");
    }
    let start = offset - 1;
    if start >= total {
        return ToolOutput::fail(
            format!("offset {offset} is past the end ({total} lines)"),
            "超出行数",
        );
    }
    let end = (start + limit).min(total);
    let mut out = String::new();
    for (i, line) in lines[start..end].iter().enumerate() {
        let _ = writeln!(out, "{:>6}\t{}", start + i + 1, truncate_chars(line, LINE_MAX_CHARS));
    }
    if start > 0 || end < total {
        let _ = writeln!(
            out,
            "… showing lines {}-{end} of {total}; use offset to read more",
            start + 1
        );
    }
    ToolOutput::ok(clip_output(out), format!("{} 行", end - start))
}

/// 按文件开头的 magic bytes 认图片类型（扩展名不可靠，内容说了算）。
/// 只认两家接口都收的那几种；认不出就是 `None`（按文本 / 二进制处理）。
pub(super) fn image_media_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        Some("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() > 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else if bytes.starts_with(b"BM") {
        Some("image/bmp")
    } else {
        None
    }
}

/// 图片文件：不按文本返回，而是当成附件交给模型（和 pi 的 `read` 一样）。
///
/// 重编码过一遍再发（`image_prep::prepare`）：限长边、去 EXIF/GPS、BMP 转 PNG。
/// 处理不了就只回文字说明——读到一个文件总比报错强，模型至少知道那是个图片、多大。
pub(super) fn read_image(path: &Path, bytes: &[u8], media_type: &str) -> ToolOutput {
    use base64::Engine as _;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    let prepared = match claudesktop_core::image_prep::prepare(
        name,
        bytes,
        claudesktop_core::image_prep::Quality::Standard,
    ) {
        Ok(prepared) => prepared,
        Err(e) => {
            return ToolOutput::ok(
                format!(
                    "{} is an image ({media_type}, {}) but it could not be prepared for the model: \
                     {e}. Read something else, or ask the user about it.",
                    path.display(),
                    claudesktop_core::image_prep::human_size(bytes.len())
                ),
                "图片读不了",
            );
        }
    };
    let summary = format!(
        "{}×{} · {}",
        prepared.width,
        prepared.height,
        claudesktop_core::image_prep::human_size(prepared.bytes.len())
    );
    let data = base64::engine::general_purpose::STANDARD.encode(&prepared.bytes);
    let note = format!(
        "Read image file [{}] {}×{}, about {} tokens of context. The image itself is attached \
         above/below: describe and reason about what you see rather than guessing from the filename.",
        prepared.media_type,
        prepared.width,
        prepared.height,
        (prepared.approx_tokens() + 500) / 1000
    );
    ToolOutput::ok_with_images(
        note,
        summary,
        vec![OutputImage { media_type: prepared.media_type, data }],
    )
}

pub(super) fn list_dir(path: &Path) -> ToolOutput {
    let entries = match std::fs::read_dir(path) {
        Ok(e) => e,
        Err(e) => {
            return ToolOutput::fail(format!("Cannot list {}: {e}", path.display()), "列目录失败");
        }
    };
    let mut items: Vec<(bool, String)> = entries
        .flatten()
        .map(|e| {
            let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
            (is_dir, e.file_name().to_string_lossy().into_owned())
        })
        .collect();
    items.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase())));
    let total = items.len();
    let mut out = String::new();
    for (is_dir, name) in items.iter().take(LIST_MAX) {
        let _ = writeln!(out, "{name}{}", if *is_dir { "/" } else { "" });
    }
    if total > LIST_MAX {
        let _ = writeln!(out, "… {} more entries", total - LIST_MAX);
    }
    if total == 0 {
        out.push_str("(empty directory)");
    }
    ToolOutput::ok(out, format!("{total} 项"))
}

/// 按块同意时最多给多少行（各块连上下文的行数加起来）；再大就只能整段放行 / 拒绝。
pub(super) const PERMISSION_HUNK_MAX_LINES: usize = 400;

/// 按块同意的结果：放行哪几块，以及问的时候给用户看的是哪些块。
/// 写盘时按当时的文件重新分块，和问的时候对不上（文件在这期间被改过，块的编号指的已经是别的改动）就不写。
#[derive(Debug, Clone, PartialEq)]
pub struct KeepHunks {
    /// 放行的块编号（[`DiffHunk::index`]）
    pub hunks: Vec<u32>,
    /// 问的时候算出来的块
    pub asked: Vec<DiffHunk>,
}

impl KeepHunks {
    /// 用户在权限卡上勾的块 + 那张卡上列的块。
    pub fn new(hunks: Vec<u32>, asked: &PermissionHunks) -> Self {
        let asked = asked.files.iter().flat_map(|f| f.hunks.iter().cloned()).collect();
        Self { hunks, asked }
    }
}

/// 按块写盘前的核对：文件现在的块和问的时候不一样，就把整次写入拒掉。
pub(super) fn check_hunks(
    old: &str,
    new: &str,
    keep: &KeepHunks,
    path: &Path,
) -> Result<(), ToolOutput> {
    if claudesktop_core::hunks_with_context(old, new) == keep.asked {
        return Ok(());
    }
    Err(ToolOutput::fail(
        format!(
            "{} changed after the user approved some of the hunks, so nothing was written. \
             Read the file again and redo the change.",
            path.display()
        ),
        "文件在授权后又被改过，没写",
    ))
}

/// 按块写入时给模型和用户的说明：写了几块、几块没写（`None` = 全写了）。
pub(super) fn partial_note(taken: usize, skipped: usize) -> Option<String> {
    (skipped > 0).then(|| format!("只写了 {taken} 块，另有 {skipped} 块没写：用户没同意"))
}

pub(super) fn write_file(path: &Path, content: &str, keep: Option<&KeepHunks>) -> ToolOutput {
    let existed = path.exists();
    // 按块同意：只把放行的块写进去，其余块保持文件原来的内容
    let (content, note) = match keep {
        None => (content.to_string(), None),
        Some(keep) => {
            let old = std::fs::read_to_string(path).ok();
            if let Err(out) = check_hunks(old.as_deref().unwrap_or(""), content, keep, path) {
                return out;
            }
            let partial = splice_partial(old.as_deref(), content, &keep.hunks);
            if partial.text.is_empty() && old.is_none() {
                return ToolOutput::fail("Nothing was approved to write", "没有放行任何改动");
            }
            (partial.text, partial_note(partial.taken, partial.skipped))
        }
    };
    let content = content.as_str();
    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        return ToolOutput::fail(format!("Cannot create {}: {e}", parent.display()), "写入失败");
    }
    match std::fs::write(path, content) {
        Ok(()) => {
            let lines = content.lines().count();
            // 按块写入时说清楚：模型和用户都要知道只写了几块（模型不知道的话会以为整份都落盘了）
            let (for_model, for_user) = match &note {
                Some(n) => (format!(". {n}"), format!(" · {n}")),
                None => (String::new(), String::new()),
            };
            ToolOutput::ok(
                format!("Wrote {lines} lines to {}{for_model}", path.display()),
                if existed {
                    format!("覆盖 · {lines} 行{for_user}")
                } else {
                    format!("新建 · {lines} 行{for_user}")
                },
            )
        }
        Err(e) => ToolOutput::fail(format!("Cannot write {}: {e}", path.display()), "写入失败"),
    }
}

/// 按块拼出要写入的内容：`keep` 里放行的块取新内容，其余块保留旧内容
/// （块的划分在 `claudesktop_core::hunk_ranges`，和权限卡上列的一致）。
/// 块数按实际有的算：重复的、超出范围的编号不算。
pub(super) fn splice_partial(old: Option<&str>, new: &str, keep: &[u32]) -> Partial {
    let text = claudesktop_core::splice_ranges(old, new, Some(keep));
    let total = claudesktop_core::hunk_ranges(old.unwrap_or(""), new).len();
    let taken = (0..total as u32).filter(|i| keep.contains(i)).count();
    Partial { text, taken, skipped: total - taken }
}

/// 按块写盘的结果：写下去的全文和放行 / 没放行的块数。
pub(super) struct Partial {
    text: String,
    /// 实际写进文件的块
    taken: usize,
    /// 没写的块（这些改动没落到盘上）
    skipped: usize,
}

/// 一次替换的结果：新全文、替换处数、实际使用的旧 / 新片段（可能已转成 CRLF）
pub(super) struct Replaced {
    pub(super) text: String,
    pub(super) count: usize,
    pub(super) old: String,
    pub(super) new: String,
}

/// 在 `text` 里做精确替换；失败时返回给模型 / UI 的错误。
pub(super) fn replace_in(
    text: &str,
    path: &Path,
    old: &str,
    new: &str,
    all: bool,
) -> Result<Replaced, ToolOutput> {
    if old.is_empty() {
        return Err(ToolOutput::fail(
            "old_string must not be empty; use write_file to create files",
            "参数无效",
        ));
    }
    // 文件是 CRLF 而模型给的是 LF：按 CRLF 再试一次
    let crlf = |s: &str| s.replace("\r\n", "\n").replace('\n', "\r\n");
    let (old, new) = if !text.contains(old) && text.contains("\r\n") && text.contains(&crlf(old)) {
        (crlf(old), crlf(new))
    } else {
        (old.to_string(), new.to_string())
    };
    let count = text.matches(old.as_str()).count();
    if count == 0 {
        return Err(ToolOutput::fail(
            format!(
                "old_string not found in {}. Read the file and copy the text exactly.",
                path.display()
            ),
            "未找到要替换的内容",
        ));
    }
    if count > 1 && !all {
        return Err(ToolOutput::fail(
            format!(
                "old_string occurs {count} times in {}; add surrounding context or set replace_all",
                path.display()
            ),
            format!("匹配到 {count} 处"),
        ));
    }
    let text = if all { text.replace(&old, &new) } else { text.replacen(&old, &new, 1) };
    Ok(Replaced { text, count: if all { count } else { 1 }, old, new })
}

pub(super) fn edit_file(
    path: &Path,
    old: &str,
    new: &str,
    all: bool,
    keep: Option<&KeepHunks>,
) -> ToolOutput {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            return ToolOutput::fail(format!("Cannot read {}: {e}", path.display()), "读取失败");
        }
    };
    let r = match replace_in(&text, path, old, new, all) {
        Ok(r) => r,
        Err(out) => return out,
    };
    // 按块同意：只把放行的块写进去，其余块保持改之前的样子
    let (written_text, note) = match keep {
        None => (r.text.clone(), None),
        Some(keep) => {
            if let Err(out) = check_hunks(&text, &r.text, keep, path) {
                return out;
            }
            let partial = splice_partial(Some(&text), &r.text, &keep.hunks);
            (partial.text, partial_note(partial.taken, partial.skipped))
        }
    };
    match std::fs::write(path, &written_text) {
        Ok(()) => {
            let (minus, plus) = (r.old.lines().count(), r.new.lines().count());
            let written = note.map(|n| format!("（{n}）")).unwrap_or_default();
            ToolOutput::ok(
                format!(
                    "Replaced {} occurrence(s) in {}{}",
                    r.count,
                    path.display(),
                    if written.is_empty() { String::new() } else { format!(". {written}") }
                ),
                format!("替换 {} 处（−{minus} +{plus} 行）{written}", r.count),
            )
        }
        Err(e) => ToolOutput::fail(format!("Cannot write {}: {e}", path.display()), "写入失败"),
    }
}

/// 读出用于 diff 预览的旧内容：不存在 → Ok(None)；过大或不是 UTF-8 → Err(())
pub(super) fn read_for_preview(path: &Path) -> Result<Option<String>, ()> {
    match std::fs::metadata(path) {
        Err(_) => Ok(None),
        Ok(m) if m.len() > PREVIEW_MAX_BYTES => Err(()),
        Ok(_) => std::fs::read_to_string(path).map(Some).map_err(|_| ()),
    }
}

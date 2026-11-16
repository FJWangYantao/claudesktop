use super::*;
use std::collections::HashSet;
use std::io::Write as _;

const LARGE_TEXT: usize = 256 * 1024;
const PREVIEW: usize = 4 * 1024;
const MARK: &str = "\u{1}claudesktop:block:";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BlockStats {
    pub entries: usize,
    pub bytes: usize,
}

pub(crate) struct BlockCache {
    root: Option<PathBuf>,
    external: HashSet<String>,
    entries: usize,
    bytes: usize,
}

impl BlockCache {
    pub(crate) fn new(token: u64) -> Self {
        let root = claudesktop_core::default_data_dir().ok().map(|dir| {
            dir.join("display-cache").join("blocks").join(format!("{}-{token}", std::process::id()))
        });
        Self { root, external: HashSet::new(), entries: 0, bytes: 0 }
    }

    #[cfg(test)]
    fn with_root(root: PathBuf) -> Self {
        Self { root: Some(root), external: HashSet::new(), entries: 0, bytes: 0 }
    }

    fn stats(&self) -> BlockStats {
        BlockStats { entries: self.entries, bytes: self.bytes }
    }

    fn path(&self, hash: &str) -> Option<PathBuf> {
        Some(self.root.as_ref()?.join(format!("{hash}.txt")))
    }

    fn write(&mut self, text: &str) -> Result<Option<String>, String> {
        let Some(root) = self.root.as_ref() else { return Ok(None) };
        let hash = claudesktop_core::checkpoint::content_hash(text);
        let path = self.path(&hash).expect("块目录刚取过");
        if path.exists() {
            let existing = std::fs::read(&path).map_err(|err| format!("读取已有块失败：{err}"))?;
            if existing == text.as_bytes() {
                return Ok(Some(hash));
            }
            return Err("已有块的内容与哈希不符".into());
        }
        std::fs::create_dir_all(root).map_err(|err| format!("创建块目录失败：{err}"))?;
        let temporary = root.join(format!(
            "{hash}-{}-{}.tmp",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let result = (|| -> Result<(), String> {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
                .map_err(|err| format!("创建临时块失败：{err}"))?;
            file.write_all(text.as_bytes()).map_err(|err| format!("写入块失败：{err}"))?;
            file.sync_all().map_err(|err| format!("同步块失败：{err}"))?;
            drop(file);
            std::fs::rename(&temporary, &path).map_err(|err| format!("发布块失败：{err}"))?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result.map(|_| Some(hash))
    }

    fn read(&self, hash: &str) -> Result<String, String> {
        let path = self.path(hash).ok_or("块缓存目录不可用")?;
        let bytes = std::fs::read(path).map_err(|err| format!("读取块 {hash} 失败：{err}"))?;
        let text =
            String::from_utf8(bytes).map_err(|err| format!("块 {hash} 不是 UTF-8：{err}"))?;
        let actual = claudesktop_core::checkpoint::content_hash(&text);
        if actual != hash {
            return Err(format!("块 {hash} 的内容校验失败"));
        }
        Ok(text)
    }

    fn externalize(&mut self, text: &mut String) -> Result<bool, String> {
        if text.len() < LARGE_TEXT || marker(text).is_some() {
            return Ok(false);
        }
        let original = text.len();
        let Some(hash) = self.write(text)? else { return Ok(false) };
        let mut end = PREVIEW.min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let mut replacement = format!("{MARK}{hash}:{original}\n");
        replacement.push_str(&text[..end]);
        *text = replacement;
        self.external.insert(hash);
        self.entries += 1;
        self.bytes += original;
        Ok(true)
    }

    fn clear(&mut self) {
        self.external.clear();
        self.entries = 0;
        self.bytes = 0;
        if let Some(root) = &self.root {
            let _ = std::fs::remove_dir_all(root);
            if let Some(parent) = root.parent() {
                let _ = std::fs::remove_dir(parent);
            }
        }
    }
}

impl Drop for BlockCache {
    fn drop(&mut self) {
        if let Some(root) = &self.root {
            let _ = std::fs::remove_dir_all(root);
            if let Some(parent) = root.parent() {
                let _ = std::fs::remove_dir(parent);
            }
        }
    }
}

fn marker(text: &str) -> Option<(&str, usize)> {
    let (header, _) = text.strip_prefix(MARK)?.split_once('\n')?;
    let (hash, len) = header.split_once(':')?;
    if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    Some((hash, len.parse().ok()?))
}

fn for_each_text(item: &mut ChatItem, f: &mut impl FnMut(&mut String)) {
    match item {
        ChatItem::Agent(text) | ChatItem::Info { text, .. } => f(text),
        ChatItem::Thinking { text, streaming: false, .. } => f(text),
        ChatItem::Tool(card) if card.phase != ToolPhase::Running => {
            for line in card.diffs.iter_mut().flat_map(|diff| &mut diff.lines) {
                f(&mut line.text);
            }
        }
        ChatItem::User { files, .. } => {
            for attachment in Arc::make_mut(files) {
                f(&mut attachment.text);
            }
        }
        _ => {}
    }
}

impl Session {
    pub fn has_external_blocks(&self) -> bool {
        !self.blocks.external.is_empty()
    }

    pub fn block_stats(&self) -> BlockStats {
        self.blocks.stats()
    }

    pub fn externalize_completed(&mut self) -> Result<usize, String> {
        if std::env::var_os("CLAUDESKTOP_DISABLE_WARM_RECLAIM").is_some()
            || !self.display_evictable()
            || self.blocks_error.is_some()
        {
            return Ok(0);
        }
        let mut count = 0;
        let mut failure = None;
        for item in &mut self.transcript {
            for_each_text(item, &mut |text| match self.blocks.externalize(text) {
                Ok(true) => count += 1,
                Ok(false) => {}
                Err(error) => failure = Some(error),
            });
        }
        for run in self.subagents.values_mut() {
            for item in &mut run.items {
                for_each_text(item, &mut |text| match self.blocks.externalize(text) {
                    Ok(true) => count += 1,
                    Ok(false) => {}
                    Err(error) => failure = Some(error),
                });
            }
        }
        let result = if let Some(error) = failure { Err(error) } else { Ok(count) };
        if let Err(error) = result {
            self.blocks_error = Some(format!("保存大块显示数据失败：{error}"));
            return Err(error);
        }
        Ok(count)
    }

    pub fn restore_all_blocks(&mut self) -> Result<usize, String> {
        if !self.has_external_blocks() {
            self.blocks_error = None;
            return Ok(0);
        }
        let result = (|| {
            let mut full = std::collections::HashMap::new();
            for hash in &self.blocks.external {
                full.insert(hash.clone(), self.blocks.read(hash)?);
            }
            let mut restored = 0;
            let mut restore = |text: &mut String| {
                let Some((hash, len)) = marker(text) else { return };
                let Some(content) = full.get(hash) else { return };
                if content.len() == len {
                    *text = content.clone();
                    restored += 1;
                }
            };
            for item in &mut self.transcript {
                for_each_text(item, &mut restore);
            }
            for run in self.subagents.values_mut() {
                for item in &mut run.items {
                    for_each_text(item, &mut restore);
                }
            }
            if restored != self.blocks.entries {
                return Err(format!(
                    "块引用数量不符：应有 {}，恢复了 {restored}",
                    self.blocks.entries
                ));
            }
            self.blocks.clear();
            Ok(restored)
        })();
        match result {
            Ok(count) => {
                self.blocks_error = None;
                Ok(count)
            }
            Err(error) => {
                self.blocks_error = Some(format!("读取大块显示数据失败：{error}"));
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn root() -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        std::env::temp_dir().join(format!(
            "claudesktop-blocks-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn live(root: PathBuf) -> Session {
        let meta: SessionMeta = serde_json::from_value(serde_json::json!({
            "id": "block-test", "title": "长文本", "cwd": ".", "agent": "builtin:local-test",
            "created_ms": 0, "updated_ms": 0, "mode": "default"
        }))
        .unwrap();
        let mut session =
            Session::from_records(19, meta, vec![Record::user("原始消息")], &Default::default());
        session.acp_id = Some("builtin-block-test".into());
        session.ready = true;
        session.blocks = BlockCache::with_root(root);
        session
    }

    #[core::prelude::v1::test]
    fn large_display_text_roundtrips_with_a_small_preview() {
        let root = root();
        let mut session = live(root.clone());
        let long = "完整正文".repeat(200_000);
        if let ChatItem::User { files, .. } = &mut session.transcript[0] {
            *files = Arc::new(vec![claudesktop_core::attach::TextAttachment {
                name: "long.txt".into(),
                path: PathBuf::from("long.txt"),
                text: long.clone(),
            }]);
        }
        session.transcript.push(ChatItem::Agent(long.clone()));
        session.transcript.push(ChatItem::Thinking {
            text: long.clone(),
            streaming: false,
            open: false,
            started: None,
            elapsed_ms: Some(1),
        });
        session.transcript.push(ChatItem::Info { text: long.clone(), open: false });
        session.transcript.push(ChatItem::Tool(ToolCard {
            id: None,
            kind: "edit".into(),
            name: "Edit".into(),
            arg: "long.rs".into(),
            result: "完成".into(),
            output: SharedString::default(),
            content: SharedString::default(),
            phase: ToolPhase::Done,
            diffs: vec![DiffView {
                path: "long.rs".into(),
                created: false,
                added: 1,
                removed: 0,
                lines: vec![claudesktop_core::DiffLineView { tag: '+', no: 1, text: long.clone() }],
                truncated: false,
            }],
            expanded: false,
            task: false,
            grant: None,
            images: Vec::new(),
        }));
        let before = serde_json::to_value(&session.transcript).unwrap();
        let retained = session.retained_bytes();
        assert_eq!(session.externalize_completed().unwrap(), 5);
        assert!(session.retained_bytes() < retained / 10);
        assert_eq!(session.block_stats().entries, 5);
        assert!(session.blocks.root.as_ref().unwrap().exists());
        assert_eq!(session.restore_all_blocks().unwrap(), 5);
        assert_eq!(serde_json::to_value(&session.transcript).unwrap(), before);
        assert_eq!(session.retained_bytes(), retained);
        assert!(!session.has_external_blocks());
    }

    #[core::prelude::v1::test]
    fn read_failure_keeps_the_preview_and_block_file() {
        let root = root();
        let mut session = live(root.clone());
        session.transcript.push(ChatItem::Agent("长文".repeat(200_000)));
        assert_eq!(session.externalize_completed().unwrap(), 1);
        let hash = session.blocks.external.iter().next().unwrap().clone();
        let path = session.blocks.path(&hash).unwrap();
        let preview = serde_json::to_value(&session.transcript).unwrap();
        std::fs::write(&path, b"corrupted").unwrap();
        assert!(session.restore_all_blocks().is_err());
        assert!(session.has_external_blocks());
        assert!(session.blocks_error.is_some());
        assert_eq!(serde_json::to_value(&session.transcript).unwrap(), preview);
        assert!(path.exists());
        drop(session);
        assert!(!root.exists());
    }

    #[core::prelude::v1::test]
    fn write_failure_keeps_the_original_inline_text() {
        let root = root();
        std::fs::write(&root, b"not a directory").unwrap();
        let mut session = live(root.clone());
        let long = "原文".repeat(300_000);
        session.transcript.push(ChatItem::Agent(long.clone()));
        assert!(session.externalize_completed().is_err());
        assert!(matches!(&session.transcript[1], ChatItem::Agent(text) if text == &long));
        assert!(!session.has_external_blocks());
        std::fs::remove_file(root).unwrap();
    }

    #[core::prelude::v1::test]
    fn busy_sessions_are_not_externalized() {
        let root = root();
        let mut session = live(root);
        session.transcript.push(ChatItem::Agent("长文".repeat(200_000)));
        session.busy = true;
        assert_eq!(session.externalize_completed().unwrap(), 0);
        assert!(!session.has_external_blocks());
    }
}

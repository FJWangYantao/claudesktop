use super::*;
use serde::{Deserialize, Serialize};
use std::io::Write as _;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Serialize, Deserialize)]
pub(crate) struct DisplayData {
    pub transcript: Vec<ChatItem>,
    pub subagents: HashMap<String, SubagentRun>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    version: u32,
    token: u64,
    data: DisplayData,
}

pub(crate) struct DisplaySnapshot {
    path: PathBuf,
    token: u64,
}

impl DisplaySnapshot {
    pub fn new(root: &Path, token: u64) -> Arc<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Arc::new(Self {
            path: root.join(format!(
                "{}-{}-{}-{}.json",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos(),
                token,
                NEXT.fetch_add(1, Ordering::Relaxed)
            )),
            token,
        })
    }

    pub fn write(&self, data: DisplayData) -> Result<usize, (String, DisplayData)> {
        let snapshot = Snapshot { version: 1, token: self.token, data };
        let result = (|| -> anyhow::Result<usize> {
            std::fs::create_dir_all(self.path.parent().unwrap())?;
            let temporary = self.path.with_extension("tmp");
            let mut file = std::io::BufWriter::new(
                std::fs::OpenOptions::new().write(true).create_new(true).open(&temporary)?,
            );
            serde_json::to_writer(&mut file, &snapshot)?;
            file.flush()?;
            let bytes = file.get_ref().metadata()?.len() as usize;
            file.get_ref().sync_all()?;
            drop(file);
            std::fs::rename(temporary, &self.path)?;
            Ok(bytes)
        })();
        result.map_err(|err| (format!("保存界面快照失败：{err:#}"), snapshot.data))
    }

    pub fn read(&self) -> anyhow::Result<DisplayData> {
        let bytes = std::fs::read(&self.path)?;
        let snapshot: Snapshot = serde_json::from_slice(&bytes)?;
        anyhow::ensure!(snapshot.version == 1, "不支持的界面快照版本");
        anyhow::ensure!(snapshot.token == self.token, "界面快照属于另一个会话");
        Ok(snapshot.data)
    }
}

impl Drop for DisplaySnapshot {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_file(self.path.with_extension("tmp"));
    }
}

impl Session {
    pub fn display_evictable(&self) -> bool {
        self.kernel == KernelChoice::Builtin
            && self.ready
            && self.acp_id.is_some()
            && self.meta.is_some()
            && self.idle_hibernatable()
            && self.display_snapshot.is_none()
            && self.deferred_events.is_empty()
    }

    pub fn take_display(&mut self, snapshot: Arc<DisplaySnapshot>) -> DisplayData {
        self.flush_stream();
        self.cached_user_count = self.user_message_count();
        self.display_scroll = Some((self.list.logical_scroll_top(), self.list.is_following_tail()));
        self.list = new_list_state();
        self.measured = Vec::new();
        self.layout = Vec::new();
        self.stream_pending = String::new();
        self.wheel = None;
        self.measured_busy = false;
        self.history_loaded = false;
        self.history_loading = true;
        self.display_snapshot = Some(snapshot);
        DisplayData {
            transcript: std::mem::take(&mut self.transcript),
            subagents: std::mem::take(&mut self.subagents),
        }
    }

    pub fn restore_display(&mut self, mut data: DisplayData) {
        data.transcript.append(&mut self.transcript);
        self.transcript = data.transcript;
        self.subagents = data.subagents;
        self.display_snapshot = None;
        self.history_loaded = true;
        self.history_loading = false;
        self.history_error = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use claudesktop_core::{PromptImage, UserPrompt};

    fn root() -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        std::env::temp_dir().join(format!(
            "claudesktop-display-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn live() -> Session {
        let meta: SessionMeta = serde_json::from_value(serde_json::json!({
            "id": "display-test", "title": "保留的标题", "cwd": ".", "agent": "builtin:local-test",
            "created_ms": 0, "updated_ms": 0, "mode": "default"
        }))
        .unwrap();
        let mut s =
            Session::from_records(17, meta, vec![Record::user("原始消息")], &Default::default());
        s.acp_id = Some("builtin-existing-session".into());
        s.ready = true;
        s.requested = true;
        s.restored = false;
        s.thinking = Some(claudesktop_builtin::ThinkingLevel::High);
        s
    }

    #[core::prelude::v1::test]
    fn display_roundtrip_keeps_binding_ui_state_attachments_and_late_messages() {
        let root = root();
        let mut s = live();
        if let ChatItem::User { files, .. } = &mut s.transcript[0] {
            *files = Arc::new(vec![claudesktop_core::attach::TextAttachment {
                name: "原文件.txt".into(),
                path: PathBuf::from("deleted.txt"),
                text: "发送时的原文".into(),
            }]);
        }
        let image = PromptImage { media_type: "image/png".into(), data: "AQIDBA==".into() };
        s.transcript.push(ChatItem::Tool(ToolCard {
            id: Some("tool-original".into()),
            kind: "read".into(),
            name: "Read".into(),
            arg: "image.png".into(),
            result: "已读取".into(),
            output: "完整输出".into(),
            phase: ToolPhase::Done,
            diffs: Vec::new(),
            expanded: true,
            task: false,
            grant: Some(Grant { kind: GrantKind::Once, label: "允许一次".into() }),
            images: vec![thumb_from_prompt_image(&image).unwrap()],
        }));
        s.transcript.push(ChatItem::Thinking {
            text: "完整思考".repeat(200),
            streaming: false,
            open: true,
            started: None,
            elapsed_ms: Some(1234),
        });
        s.transcript.push(ChatItem::Agent("原始正文".repeat(500)));
        s.subagents.insert(
            "child".into(),
            SubagentRun {
                agent: "explore".into(),
                prompt: "原始子任务".into(),
                items: vec![ChatItem::Agent("子任务结果".into())],
            },
        );
        s.open_groups.insert(1);
        s.changes_open.insert(3);
        s.viewing_task = Some("child".into());
        s.review_pending = true;
        s.unread = true;
        s.cache_hit = Some(75);
        s.sync_list(ViewMode::Normal);
        s.list.scroll_to(ListOffset { item_ix: 1, offset_in_item: px(7.) });
        let expected = serde_json::to_value(&s.transcript).unwrap();
        let before = s.retained_bytes();
        assert!(s.display_evictable());
        let snapshot = DisplaySnapshot::new(&root, s.token);
        let data = s.take_display(snapshot.clone());
        assert!(
            s.transcript.is_empty()
                && s.subagents.is_empty()
                && s.measured.is_empty()
                && s.layout.is_empty()
        );
        assert!(s.retained_bytes() < before);
        assert!(s.history_loading && !s.history_loaded);
        assert_eq!(s.acp_id.as_deref(), Some("builtin-existing-session"));
        assert!(s.ready && s.requested && !s.restored);
        assert!(snapshot.write(data).is_ok());
        s.note("快照保存后到达的说明");
        s.queued.push(QueuedPrompt::from_prompt(UserPrompt::text("读取期间排队的消息")).unwrap());
        s.restore_display(snapshot.read().unwrap());
        assert_eq!(serde_json::to_value(&s.transcript[..4]).unwrap(), expected);
        assert!(
            matches!(s.transcript.last(), Some(ChatItem::Note(text)) if text.as_ref() == "快照保存后到达的说明")
        );
        assert_eq!(s.queued[0].text, "读取期间排队的消息");
        assert_eq!(s.acp_id.as_deref(), Some("builtin-existing-session"));
        assert!(s.ready && !s.restored && s.history_loaded && !s.history_loading);
        assert_eq!(s.thinking, Some(claudesktop_builtin::ThinkingLevel::High));
        assert_eq!(s.viewing_task.as_deref(), Some("child"));
        assert_eq!(s.subagents["child"].prompt.as_ref(), "原始子任务");
        assert!(s.open_groups.contains(&1) && s.changes_open.contains(&3));
        assert!(s.review_pending && s.unread);
        assert_eq!(s.cache_hit, Some(75));
        s.sync_list(ViewMode::Normal);
        assert_eq!(s.list.logical_scroll_top().item_ix, 1);
        assert_eq!(s.list.logical_scroll_top().offset_in_item, px(7.));
        drop(snapshot);
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
        std::fs::remove_dir(&root).unwrap();
    }

    #[core::prelude::v1::test]
    fn failed_write_restores_the_original_display_without_changing_the_session() {
        let root = root();
        std::fs::write(&root, b"not a directory").unwrap();
        let mut s = live();
        let snapshot = DisplaySnapshot::new(&root, s.token);
        let data = s.take_display(snapshot.clone());
        let (error, data) = snapshot.write(data).err().unwrap();
        assert!(error.contains("保存界面快照失败"));
        s.restore_display(data);
        assert!(s.history_loaded && s.display_snapshot.is_none());
        assert_eq!(s.acp_id.as_deref(), Some("builtin-existing-session"));
        assert!(
            matches!(&s.transcript[0], ChatItem::User { text, .. } if text.as_ref() == "原始消息")
        );
        drop(snapshot);
        std::fs::remove_file(root).unwrap();
    }

    #[core::prelude::v1::test]
    fn corrupt_snapshot_is_retained_until_session_close() {
        let root = root();
        let mut s = live();
        let snapshot = DisplaySnapshot::new(&root, s.token);
        let data = s.take_display(snapshot.clone());
        assert!(snapshot.write(data).is_ok());
        std::fs::write(&snapshot.path, b"{broken").unwrap();
        assert!(snapshot.read().is_err());
        assert!(snapshot.path.exists());
        s.history_loading = false;
        s.history_error = Some("读取失败".into());
        assert!(!s.history_loaded && s.display_snapshot.is_some());
        let path = snapshot.path.clone();
        drop(snapshot);
        drop(s);
        assert!(!path.exists());
        std::fs::remove_dir(root).unwrap();
    }

    #[core::prelude::v1::test]
    fn closing_while_a_snapshot_is_being_written_cleans_the_late_file() {
        let root = root();
        let mut s = live();
        let snapshot = DisplaySnapshot::new(&root, s.token);
        let data = s.take_display(snapshot.clone());
        assert_eq!(s.user_message_count(), 1);
        drop(s);
        assert!(snapshot.write(data).is_ok());
        let path = snapshot.path.clone();
        assert!(path.exists());
        drop(snapshot);
        assert!(!path.exists());
        std::fs::remove_dir(root).unwrap();
    }

    #[core::prelude::v1::test]
    fn sending_after_restore_takes_precedence_over_the_saved_scroll_position() {
        let root = root();
        let mut s = live();
        s.transcript.push(ChatItem::Agent("正文".into()));
        s.sync_list(ViewMode::Normal);
        s.list.scroll_to(ListOffset { item_ix: 0, offset_in_item: px(0.) });
        let snapshot = DisplaySnapshot::new(&root, s.token);
        let data = s.take_display(snapshot);
        s.restore_display(data);
        s.follow_tail();
        assert!(s.display_scroll.is_none());
        s.sync_list(ViewMode::Normal);
        assert!(s.list.is_following_tail());
    }

    #[core::prelude::v1::test]
    fn display_reclaim_excludes_inflight_and_unsaved_sessions() {
        let mut s = live();
        assert!(s.display_evictable());
        s.busy = true;
        assert!(!s.display_evictable());
        s.busy = false;
        s.compact_queued = true;
        assert!(!s.display_evictable());
        s.compact_queued = false;
        s.kernel_rewinding = true;
        assert!(!s.display_evictable());
        s.kernel_rewinding = false;
        s.meta = None;
        assert!(!s.display_evictable());
    }
}

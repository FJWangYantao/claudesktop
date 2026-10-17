//! 会话持久化：每个会话一份追加式事件日志，内核 / UI 重启后可重放恢复。
//!
//! 目录布局（`<data>/sessions/`）：
//! - `<id>.json`  —— [`SessionMeta`]，整体覆写（先写临时文件再改名）；列表只读这些小文件
//! - `<id>.jsonl` —— 每行一条 [`Record`]，只追加
//!
//! 记录的是**内核事件**而不是界面状态：恢复时把事件按序重放回 UI 的
//! 同一套映射逻辑，任何内核（ACP / 自研）都不用另写序列化。
//! 流式文本块在写入前合并，避免一个 token 一行。

use crate::KernelEvent;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// 合并中的文本超过这个长度就先落盘（降低崩溃时的丢失量）。
const COALESCE_LIMIT: usize = 4096;

/// 会话元信息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMeta {
    /// 本地会话 ID（与内核的 sessionId 无关）
    pub id: String,
    pub title: String,
    /// 会话工作目录（侧栏按它分组）
    pub cwd: PathBuf,
    /// 内核启动命令 / 标识，恢复续聊时用来判断是不是同一个 agent
    pub agent: String,
    /// 内核侧会话 ID（ACP sessionId），续聊（`session/resume` / `session/load`）用
    #[serde(default)]
    pub kernel_session_id: Option<String>,
    pub created_ms: u64,
    pub updated_ms: u64,
    /// 归档了：侧栏不列在项目 / 日期分组里，收进末尾的「已归档」
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub archived: bool,
}

/// 日志里的一条记录。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Record {
    /// 用户发出的一轮消息；`images` 为图片附件的绝对路径（资产目录里）
    User {
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<String>,
    },
    /// 内核事件（只记对话内容相关的，见 [`Record::worth_keeping`]）
    Event { event: KernelEvent },
    /// 用户对某条权限请求的应答（展示名，如「Yes」「已取消」）
    PermissionResolved { id: u64, label: String },
    /// 一轮结束时的统计：时间、模型、token 用量、这一轮用了多久
    /// （首页用量概览用；重放时挂到这一轮的回复下面）。
    /// 内核没报告模型 / 用量时对应字段为空 / 0；旧日志没有用时
    Turn {
        at_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default)]
        input_tokens: u64,
        #[serde(default)]
        output_tokens: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        elapsed_ms: Option<u64>,
        /// 这一轮结束时上下文里的 token（输入框旁的上下文用量；内核不报告时为空）
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context_tokens: Option<u64>,
    },
    /// 一段思考想了多久（思考收尾时记下；重放时挂到它前面最近的思考块上）
    Thought { elapsed_ms: u64 },
    /// 内核改了一个文件（检查点）：改之前 / 之后的内容存成 blob（[`SessionStore::save_blob`]），
    /// 这里只记哈希，None = 文件不存在。它前面最近的一条 `User` 就是它所属的那一轮
    FileChanged {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        before: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after: Option<String>,
    },
    /// 回退到第 `turn` 条用户消息（从 0 数，按当时的有效记录）之前。`conversation` 为真时
    /// 从那条消息起的记录作废（见 [`effective`]）；`files` 是还原了几个文件，`prompt` 是那条消息
    /// （重放时显示成一行备注）
    Rewind { turn: usize, conversation: bool, files: usize, prompt: String },
}

impl Record {
    /// 只持久化重放时有意义的事件；状态 / 连接 / fs 诊断等不记（文件改动另记成 `FileChanged`）。
    pub fn worth_keeping(event: &KernelEvent) -> bool {
        matches!(
            event,
            KernelEvent::AgentMessageChunk { .. }
                | KernelEvent::AgentThoughtChunk { .. }
                | KernelEvent::ToolCallStarted { .. }
                | KernelEvent::ToolCallUpdated { .. }
                | KernelEvent::PlanReplaced { .. }
                | KernelEvent::PermissionAuto { .. }
                | KernelEvent::PermissionRequest { .. }
                | KernelEvent::PromptFailed { .. }
                | KernelEvent::Compacting
                | KernelEvent::Compacted { .. }
                | KernelEvent::CompactFailed { .. }
                | KernelEvent::TurnEnded { .. }
        )
    }
}

/// 会话存储目录。
#[derive(Debug, Clone)]
pub struct SessionStore {
    dir: PathBuf,
}

impl SessionStore {
    /// 默认位置：`CLAUDESKTOP_DATA_DIR`，否则 Windows `%APPDATA%\claudesktop`，
    /// 其他平台 `$XDG_DATA_HOME/claudesktop` 或 `~/.local/share/claudesktop`。
    pub fn open_default() -> io::Result<Self> {
        Self::open(default_data_dir()?.join("sessions"))
    }

    pub fn open(dir: impl Into<PathBuf>) -> io::Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// 所有会话的元信息，最近更新的在前；损坏的文件跳过。
    pub fn list(&self) -> Vec<SessionMeta> {
        let mut metas: Vec<SessionMeta> = fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .filter_map(|e| serde_json::from_slice(&fs::read(e.path()).ok()?).ok())
            .collect();
        metas.sort_by_key(|m| std::cmp::Reverse(m.updated_ms));
        metas
    }

    /// 新建会话：生成 ID、写元信息，返回可追加的日志。
    pub fn create(
        &self,
        title: &str,
        cwd: PathBuf,
        agent: &str,
    ) -> io::Result<(SessionMeta, SessionLog)> {
        let now = now_ms();
        let meta = SessionMeta {
            id: new_id(now),
            title: title.to_string(),
            cwd,
            agent: agent.to_string(),
            kernel_session_id: None,
            created_ms: now,
            updated_ms: now,
            archived: false,
        };
        self.save_meta(&meta)?;
        let log = self.append_log(&meta.id)?;
        Ok((meta, log))
    }

    /// 覆写元信息（原子：临时文件 + 改名）。
    pub fn save_meta(&self, meta: &SessionMeta) -> io::Result<()> {
        let path = self.meta_path(&meta.id);
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(meta)?)?;
        fs::rename(&tmp, &path)
    }

    /// 打开已有会话的日志用于继续追加。
    pub fn append_log(&self, id: &str) -> io::Result<SessionLog> {
        let file = OpenOptions::new().create(true).append(true).open(self.log_path(id))?;
        Ok(SessionLog { file, pending: None })
    }

    /// 读出全部记录；末尾半行（写到一半崩溃）及无法解析的行跳过。
    pub fn load(&self, id: &str) -> io::Result<Vec<Record>> {
        let file = File::open(self.log_path(id))?;
        Ok(BufReader::new(file)
            .lines()
            .map_while(Result::ok)
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str(&l).ok())
            .collect())
    }

    pub fn delete(&self, id: &str) -> io::Result<()> {
        let _ = fs::remove_file(self.log_path(id));
        // 图片附件、检查点目录一并清掉（没有时目录不存在，忽略）
        let _ = fs::remove_dir_all(self.assets_dir(id));
        let _ = fs::remove_dir_all(self.checkpoints_dir(id));
        fs::remove_file(self.meta_path(id))
    }

    /// 会话的图片附件目录：`<dir>/<id>.assets/`
    fn assets_dir(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.assets"))
    }

    /// 保存一张图片附件（`ext` 为扩展名，如 `png`），返回文件绝对路径。
    pub fn save_image(&self, id: &str, ext: &str, bytes: &[u8]) -> io::Result<PathBuf> {
        let dir = self.assets_dir(id);
        fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}.{ext}", new_id(now_ms())));
        fs::write(&path, bytes)?;
        Ok(path)
    }

    /// 会话的检查点目录：`<dir>/<id>.checkpoints/<sha256>`，文件内容按哈希存一份
    fn checkpoints_dir(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.checkpoints"))
    }

    /// 存一份文件内容（同样的内容只存一次），返回它的哈希。
    pub fn save_blob(&self, id: &str, text: &str) -> io::Result<String> {
        let hash = crate::checkpoint::content_hash(text);
        let dir = self.checkpoints_dir(id);
        let path = dir.join(&hash);
        if !path.exists() {
            fs::create_dir_all(&dir)?;
            let tmp = dir.join(format!("{hash}.tmp"));
            fs::write(&tmp, text)?;
            fs::rename(&tmp, &path)?;
        }
        Ok(hash)
    }

    /// 取回 [`SessionStore::save_blob`] 存的内容。
    pub fn load_blob(&self, id: &str, hash: &str) -> io::Result<String> {
        fs::read_to_string(self.checkpoints_dir(id).join(hash))
    }

    fn meta_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    fn log_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.jsonl"))
    }
}

/// 一个会话的追加式日志。连续的同类文本块先合并在内存里，
/// 遇到其他记录、超过 [`COALESCE_LIMIT`] 或 drop 时落盘。
pub struct SessionLog {
    file: File,
    pending: Option<KernelEvent>,
}

impl SessionLog {
    pub fn append(&mut self, record: Record) -> io::Result<()> {
        if let Record::Event { event } = &record {
            if let Some(merged) = self.try_merge(event) {
                if merged >= COALESCE_LIMIT {
                    self.flush()?;
                }
                return Ok(());
            }
            if is_chunk(event) {
                self.flush()?;
                self.pending = Some(event.clone());
                return Ok(());
            }
        }
        self.flush()?;
        self.write_line(&record)
    }

    /// 把合并中的文本块写出去。
    pub fn flush(&mut self) -> io::Result<()> {
        if let Some(event) = self.pending.take() {
            self.write_line(&Record::Event { event })?;
        }
        Ok(())
    }

    /// 与合并中的块同类则并入，返回合并后长度。
    fn try_merge(&mut self, event: &KernelEvent) -> Option<usize> {
        use KernelEvent::{AgentMessageChunk as M, AgentThoughtChunk as T};
        match (self.pending.as_mut()?, event) {
            (M { text: buf }, M { text }) | (T { text: buf }, T { text }) => {
                buf.push_str(text);
                Some(buf.len())
            }
            _ => None,
        }
    }

    fn write_line(&mut self, record: &Record) -> io::Result<()> {
        let mut line = serde_json::to_vec(record)?;
        line.push(b'\n');
        // 一次 write 写整行，崩溃时最多丢最后半行（load 会跳过）
        self.file.write_all(&line)
    }
}

impl Drop for SessionLog {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

/// 折算回退：返回仍然有效的记录。遇到回退对话的 `Rewind` 时，把已收下的记录截到第 `turn` 条
/// 用户消息之前，`Rewind` 本身留着（重放时显示成备注）。日志只追加，读的时候才折算。
pub fn effective(records: &[Record]) -> Vec<Record> {
    let mut out: Vec<Record> = Vec::with_capacity(records.len());
    for record in records {
        if let Record::Rewind { turn, conversation: true, .. } = record
            && let Some(cut) = out
                .iter()
                .enumerate()
                .filter(|(_, r)| matches!(r, Record::User { .. }))
                .nth(*turn)
                .map(|(i, _)| i)
        {
            out.truncate(cut);
        }
        out.push(record.clone());
    }
    out
}

fn is_chunk(event: &KernelEvent) -> bool {
    matches!(event, KernelEvent::AgentMessageChunk { .. } | KernelEvent::AgentThoughtChunk { .. })
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// 时间戳 + 进程号 + 计数器：同一进程内单调、跨进程不撞。
fn new_id(now: u64) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{now:011x}-{:x}-{n}", std::process::id())
}

/// 用户数据根目录（会话、权限规则等都放在这里）：`CLAUDESKTOP_DATA_DIR`，否则
/// Windows `%APPDATA%\claudesktop`，其他平台 XDG 数据目录。
pub fn default_data_dir() -> io::Result<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDESKTOP_DATA_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
    };
    base.map(|b| b.join("claudesktop"))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "找不到用户数据目录"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(name: &str) -> SessionStore {
        let dir = std::env::temp_dir().join(format!("claudesktop-test-{name}-{}", now_ms()));
        let _ = fs::remove_dir_all(&dir);
        SessionStore::open(dir).unwrap()
    }

    fn chunk(text: &str) -> Record {
        Record::Event { event: KernelEvent::AgentMessageChunk { text: text.into() } }
    }

    #[test]
    fn archived_flag_round_trips_and_old_metas_still_load() {
        // 老的元信息文件没有 archived：读成没归档
        let old = r#"{"id":"s","title":"t","cwd":".","agent":"a","created_ms":1,"updated_ms":2}"#;
        let meta: SessionMeta = serde_json::from_str(old).unwrap();
        assert!(!meta.archived);
        // 没归档不写这个字段，旧版本读新文件也不受影响
        assert!(!serde_json::to_string(&meta).unwrap().contains("archived"));
        let store = temp_store("archive");
        let (mut meta, _log) = store.create("t", ".".into(), "a").unwrap();
        meta.archived = true;
        store.save_meta(&meta).unwrap();
        assert!(store.list().iter().any(|m| m.id == meta.id && m.archived));
    }

    #[test]
    fn roundtrip_and_coalesce() {
        let store = temp_store("roundtrip");
        let (meta, mut log) = store.create("标题", "C:/proj".into(), "mock").unwrap();
        log.append(Record::User { text: "你好".into(), images: Vec::new() }).unwrap();
        for piece in ["你", "好", "！"] {
            log.append(chunk(piece)).unwrap();
        }
        log.append(Record::Event { event: KernelEvent::AgentThoughtChunk { text: "想".into() } })
            .unwrap();
        log.append(Record::Event {
            event: KernelEvent::TurnEnded { stop_reason: "end_turn".into() },
        })
        .unwrap();
        drop(log);

        let records = store.load(&meta.id).unwrap();
        assert_eq!(
            records,
            vec![
                Record::User { text: "你好".into(), images: Vec::new() },
                chunk("你好！"),
                Record::Event { event: KernelEvent::AgentThoughtChunk { text: "想".into() } },
                Record::Event { event: KernelEvent::TurnEnded { stop_reason: "end_turn".into() } },
            ]
        );
        let _ = fs::remove_dir_all(store.dir());
    }

    #[test]
    fn list_sorted_and_tolerates_garbage() {
        let store = temp_store("list");
        let (mut a, _) = store.create("a", "C:/a".into(), "mock").unwrap();
        let (b, _) = store.create("b", "C:/b".into(), "mock").unwrap();
        a.updated_ms = b.updated_ms + 10;
        a.title = "a2".into();
        store.save_meta(&a).unwrap();
        fs::write(store.dir().join("broken.json"), b"{not json").unwrap();
        // 末尾半行：应被跳过而不是让整个会话读不出来
        let mut log = store.append_log(&b.id).unwrap();
        log.append(Record::User { text: "x".into(), images: Vec::new() }).unwrap();
        drop(log);
        OpenOptions::new()
            .append(true)
            .open(store.dir().join(format!("{}.jsonl", b.id)))
            .unwrap()
            .write_all(b"{\"t\":\"user\",\"te")
            .unwrap();

        let titles: Vec<_> = store.list().into_iter().map(|m| m.title).collect();
        assert_eq!(titles, vec!["a2", "b"]);
        assert_eq!(store.load(&b.id).unwrap().len(), 1);
        // 统计记录可以往返
        let turn = Record::Turn {
            at_ms: 5,
            model: None,
            input_tokens: 1,
            output_tokens: 2,
            elapsed_ms: None,
            context_tokens: None,
        };
        let line = serde_json::to_string(&turn).unwrap();
        assert_eq!(line, r#"{"t":"turn","at_ms":5,"input_tokens":1,"output_tokens":2}"#);
        assert_eq!(serde_json::from_str::<Record>(&line).unwrap(), turn);
        let timed = Record::Turn {
            at_ms: 5,
            model: None,
            input_tokens: 1,
            output_tokens: 2,
            elapsed_ms: Some(1500),
            context_tokens: Some(3),
        };
        let line = serde_json::to_string(&timed).unwrap();
        assert!(line.ends_with(r#""elapsed_ms":1500,"context_tokens":3}"#));
        assert_eq!(serde_json::from_str::<Record>(&line).unwrap(), timed);
        let thought = Record::Thought { elapsed_ms: 1200 };
        let line = serde_json::to_string(&thought).unwrap();
        assert_eq!(line, r#"{"t":"thought","elapsed_ms":1200}"#);
        assert_eq!(serde_json::from_str::<Record>(&line).unwrap(), thought);

        store.delete(&a.id).unwrap();
        assert_eq!(store.list().len(), 1);
        let _ = fs::remove_dir_all(store.dir());
    }

    #[test]
    fn only_conversation_events_are_kept() {
        assert!(Record::worth_keeping(&KernelEvent::TurnEnded { stop_reason: "end_turn".into() }));
        assert!(!Record::worth_keeping(&KernelEvent::Status("x".into())));
        assert!(!Record::worth_keeping(&KernelEvent::FsRead { path: "a".into(), ok: true }));
    }

    #[test]
    fn saves_images_and_parses_legacy_user_records() {
        let store = temp_store("images");
        let (meta, _) = store.create("图", "C:/x".into(), "mock").unwrap();
        let path = store.save_image(&meta.id, "png", b"png-bytes").unwrap();
        assert!(path.is_absolute());
        assert!(path.to_string_lossy().ends_with(".png"));
        // 旧日志没有 images 字段：能解析，默认为空
        let legacy = r#"{"t":"user","text":"旧记录"}"#;
        assert_eq!(
            serde_json::from_str::<Record>(legacy).unwrap(),
            Record::User { text: "旧记录".into(), images: Vec::new() }
        );
        // 新记录带附件路径，可往返
        let record = Record::User {
            text: "看图".into(),
            images: vec![path.to_string_lossy().into_owned()],
        };
        let line = serde_json::to_string(&record).unwrap();
        assert_eq!(serde_json::from_str::<Record>(&line).unwrap(), record);
        // 没有附件时不写 images 键（日志不膨胀）
        let plain =
            serde_json::to_string(&Record::User { text: "纯文本".into(), images: Vec::new() })
                .unwrap();
        assert!(!plain.contains("images"));
        let _ = fs::remove_dir_all(store.dir());
    }
}

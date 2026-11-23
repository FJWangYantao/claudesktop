use super::{LogRecord, RequestLog};
use chrono::{Local, NaiveDate};
use serde::Deserialize;
use std::borrow::Cow;
use std::collections::HashSet;
use std::fs::{self, File, Metadata};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

const CHECKPOINT_BYTES: usize = 32;
const READ_BUFFER_BYTES: u64 = 64 * 1024;
const MAX_SHARED_STRINGS: usize = 4096;
const MAX_SHARED_STRING_LEN: usize = 256;

#[derive(Clone, Default)]
pub struct LogCursor {
    files: Vec<FileCursor>,
    since_ms: Option<u64>,
    oldest_ms: Option<u64>,
}

#[derive(Clone)]
struct FileCursor {
    path: PathBuf,
    stamp: FileStamp,
    position: Position,
}

#[derive(Clone, PartialEq, Eq)]
struct FileStamp {
    id: Option<(u64, u128)>,
    created: Option<SystemTime>,
    modified: Option<SystemTime>,
    len: u64,
}

impl FileStamp {
    fn new(file: &File, metadata: &Metadata) -> Self {
        Self {
            id: file_id(file, metadata),
            created: metadata.created().ok(),
            modified: metadata.modified().ok(),
            len: metadata.len(),
        }
    }
}

#[cfg(windows)]
fn file_id(file: &File, _: &Metadata) -> Option<(u64, u128)> {
    use std::os::windows::io::{AsRawHandle, RawHandle};

    #[repr(C)]
    struct FileIdInfo {
        volume: u64,
        id: [u8; 16],
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFileInformationByHandleEx(
            handle: RawHandle,
            class: i32,
            info: *mut FileIdInfo,
            size: u32,
        ) -> i32;
    }

    const FILE_ID_INFO: i32 = 18;
    let mut info = FileIdInfo { volume: 0, id: [0; 16] };
    // info 是 FILE_ID_INFO 的完整可写缓冲，file 在调用期间保持打开。
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FILE_ID_INFO,
            &mut info,
            size_of::<FileIdInfo>() as u32,
        )
    };
    (ok != 0).then(|| (info.volume, u128::from_ne_bytes(info.id)))
}

#[cfg(unix)]
fn file_id(_: &File, metadata: &Metadata) -> Option<(u64, u128)> {
    use std::os::unix::fs::MetadataExt;
    Some((metadata.dev(), metadata.ino().into()))
}

#[cfg(not(any(unix, windows)))]
fn file_id(_: &File, _: &Metadata) -> Option<(u64, u128)> {
    None
}

#[derive(Clone, Default)]
struct Position {
    offset: u64,
    tail: [u8; CHECKPOINT_BYTES],
    tail_len: usize,
    unterminated_record: bool,
}

impl Position {
    fn still_matches(&self, file: &mut File) -> io::Result<bool> {
        let mut bytes = [0; CHECKPOINT_BYTES];
        file.seek(SeekFrom::Start(self.offset - self.tail_len as u64))?;
        file.read_exact(&mut bytes[..self.tail_len])?;
        Ok(bytes[..self.tail_len] == self.tail[..self.tail_len])
    }
}

struct LogFile {
    path: PathBuf,
    file: File,
    stamp: FileStamp,
}

/// 一次读盘的结果。正常追加只带新增记录，日志来源变化时带全量记录。
pub struct LogUpdate {
    cursor: LogCursor,
    logs: Vec<RequestLog>,
    replace: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogChange {
    Unchanged,
    Appended { start: usize },
    Rebuilt,
}

impl LogCursor {
    pub fn read(&self, dir: &Path, since_ms: Option<u64>) -> io::Result<LogUpdate> {
        let mut files = Vec::new();
        for (_, path) in log_paths(dir, since_ms)? {
            let file = match File::open(&path) {
                Ok(file) => file,
                Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
                Err(err) => return Err(err),
            };
            let metadata = file.metadata()?;
            let stamp = FileStamp::new(&file, &metadata);
            files.push(LogFile { path, file, stamp });
        }
        let resume = self.since_ms <= since_ms && self.can_resume(&mut files)?;
        let mut logs = Vec::new();
        let mut line = Vec::new();
        let mut strings = Strings::default();
        let mut next = Vec::with_capacity(files.len());
        for (i, LogFile { path, mut file, stamp }) in files.into_iter().enumerate() {
            let old = resume.then(|| self.files.get(i)).flatten();
            let mut position = old.map(|entry| entry.position.clone()).unwrap_or_default();
            if old.is_none_or(|entry| stamp.len != entry.stamp.len) {
                read_file(
                    &mut file,
                    stamp.len,
                    since_ms,
                    &mut position,
                    &mut line,
                    &mut strings,
                    &mut logs,
                )?;
            }
            next.push(FileCursor { path, stamp, position });
        }
        logs.sort_by_key(|log| log.at_ms);
        Ok(LogUpdate {
            cursor: Self { files: next, since_ms, oldest_ms: self.oldest_ms },
            logs,
            replace: !resume || self.files.is_empty(),
        })
    }

    fn can_resume(&self, files: &mut [LogFile]) -> io::Result<bool> {
        if files.len() < self.files.len() {
            return Ok(false);
        }
        for (i, (old, new)) in self.files.iter().zip(files).enumerate() {
            if old.path != new.path
                || old.stamp.id.is_none()
                || old.stamp.id != new.stamp.id
                || old.stamp.created != new.stamp.created
            {
                return Ok(false);
            }
            if old.stamp == new.stamp {
                continue;
            }
            // 只有末尾文件可以补读，保证同一时刻的记录仍按文件和行的顺序排列。
            if i + 1 != self.files.len()
                || old.stamp.len >= new.stamp.len
                || old.position.unterminated_record
                || !old.position.still_matches(&mut new.file)?
            {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

impl LogUpdate {
    /// 补读保留已有下标，即使新记录的发出时间早于某些旧记录。
    pub fn apply(mut self, logs: &mut Vec<RequestLog>) -> (LogCursor, LogChange) {
        if self.replace {
            self.cursor.oldest_ms = self.logs.first().map(|log| log.at_ms);
            *logs = self.logs;
            return (self.cursor, LogChange::Rebuilt);
        }
        let mut expired = false;
        if let Some((oldest, since)) = self.cursor.oldest_ms.zip(self.cursor.since_ms)
            && oldest < since
        {
            logs.retain(|log| log.at_ms >= since);
            self.cursor.oldest_ms = logs.iter().map(|log| log.at_ms).min();
            expired = true;
        }
        let start = logs.len();
        if let Some(first) = self.logs.first() {
            self.cursor.oldest_ms =
                Some(self.cursor.oldest_ms.map_or(first.at_ms, |old| old.min(first.at_ms)));
            logs.append(&mut self.logs);
        }
        let change = if expired {
            LogChange::Rebuilt
        } else if logs.len() > start {
            LogChange::Appended { start }
        } else {
            LogChange::Unchanged
        };
        (self.cursor, change)
    }
}

#[derive(Default, Deserialize)]
struct LogText<'a>(#[serde(borrow)] Cow<'a, str>);

#[derive(Default)]
struct Strings(HashSet<Arc<str>>);

impl Strings {
    fn share(&mut self, LogText(value): LogText<'_>, previous: Option<&Arc<str>>) -> Arc<str> {
        if value.len() > MAX_SHARED_STRING_LEN {
            return Arc::from(value.as_ref());
        }
        if let Some(shared) = previous.filter(|shared| shared.as_ref() == value.as_ref()) {
            return shared.clone();
        }
        if let Some(shared) = self.0.get(value.as_ref()) {
            return shared.clone();
        }
        let text: Arc<str> = Arc::from(value.as_ref());
        if self.0.len() < MAX_SHARED_STRINGS {
            self.0.insert(text.clone());
        }
        text
    }

    fn share_log(
        &mut self,
        log: LogRecord<LogText<'_>>,
        previous: Option<&RequestLog>,
    ) -> RequestLog {
        let LogRecord {
            at_ms,
            req_id,
            attempt,
            session,
            turn,
            purpose,
            protocol,
            model,
            host,
            path,
            method,
            body_bytes,
            messages,
            tools,
            status,
            error,
            ttfb_ms,
            first_token_ms,
            total_ms,
            input,
            output,
            reasoning,
            cache_read,
            finish_reason,
            cancelled,
            body,
        } = log;
        RequestLog {
            at_ms,
            req_id,
            attempt,
            session: session.map(|s| self.share(s, previous.and_then(|log| log.session.as_ref()))),
            turn,
            purpose: self.share(purpose, previous.map(|log| &log.purpose)),
            protocol: self.share(protocol, previous.map(|log| &log.protocol)),
            model: model.map(|s| self.share(s, previous.and_then(|log| log.model.as_ref()))),
            host: self.share(host, previous.map(|log| &log.host)),
            path: self.share(path, previous.map(|log| &log.path)),
            method: self.share(method, previous.map(|log| &log.method)),
            body_bytes,
            messages,
            tools,
            status,
            error,
            ttfb_ms,
            first_token_ms,
            total_ms,
            input,
            output,
            reasoning,
            cache_read,
            finish_reason: finish_reason
                .map(|s| self.share(s, previous.and_then(|log| log.finish_reason.as_ref()))),
            cancelled,
            body,
        }
    }
}

fn log_paths(dir: &Path, since_ms: Option<u64>) -> io::Result<Vec<(NaiveDate, PathBuf)>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    let since_day = since_ms.and_then(|ms| {
        chrono::DateTime::from_timestamp_millis(ms as i64)
            .map(|t| t.with_timezone(&Local).date_naive() - chrono::Duration::days(1))
    });
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "jsonl")
            && let Some(date) = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .and_then(|stem| NaiveDate::parse_from_str(stem, "%Y-%m-%d").ok())
            && since_day.is_none_or(|day| date >= day)
            && !entry.file_type()?.is_dir()
        {
            files.push((date, path));
        }
    }
    files.sort();
    Ok(files)
}

fn read_file(
    file: &mut File,
    len: u64,
    since_ms: Option<u64>,
    position: &mut Position,
    line: &mut Vec<u8>,
    strings: &mut Strings,
    logs: &mut Vec<RequestLog>,
) -> io::Result<()> {
    file.seek(SeekFrom::Start(position.offset))?;
    let remaining = len - position.offset;
    let capacity = remaining.min(READ_BUFFER_BYTES) as usize;
    let mut reader = BufReader::with_capacity(capacity, file.take(remaining));
    while position.offset < len {
        line.clear();
        let bytes = reader.read_until(b'\n', line)?;
        if bytes == 0 {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
        }
        let record = std::str::from_utf8(line)
            .ok()
            .and_then(|line| serde_json::from_str::<LogRecord<LogText<'_>>>(line).ok());
        let terminated = line.ends_with(b"\n");
        position.unterminated_record = !terminated && record.is_some();
        if let Some(log) = record.filter(|log| since_ms.is_none_or(|since| log.at_ms >= since)) {
            logs.push(strings.share_log(log, logs.last()));
        }
        if !terminated {
            if position.offset + bytes as u64 != len {
                return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
            }
            break;
        }
        let tail = &line[bytes.saturating_sub(CHECKPOINT_BYTES)..];
        position.tail[..tail.len()].copy_from_slice(tail);
        position.tail_len = tail.len();
        position.offset += bytes as u64;
    }
    Ok(())
}

/// 读 `since_ms` 之后（None = 全部）的请求日志，按时间从早到晚。读坏的行跳过。
pub fn read_logs(dir: &Path, since_ms: Option<u64>) -> Vec<RequestLog> {
    let mut logs = Vec::new();
    let mut line = Vec::new();
    let mut strings = Strings::default();
    for (_, path) in log_paths(dir, since_ms).unwrap_or_default() {
        if let Ok(mut file) = File::open(path)
            && let Ok(metadata) = file.metadata()
        {
            let _ = read_file(
                &mut file,
                metadata.len(),
                since_ms,
                &mut Position::default(),
                &mut line,
                &mut strings,
                &mut logs,
            );
        }
    }
    logs.sort_by_key(|log| log.at_ms);
    logs
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn fixture(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cd-gateway-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn append(path: &Path, bytes: &[u8]) {
        fs::OpenOptions::new().append(true).open(path).unwrap().write_all(bytes).unwrap();
    }

    #[test]
    fn shares_borrowed_and_escaped_log_strings_without_changing_decoding() {
        let mut strings = Strings::default();
        let plain = r#"{"at_ms":1,"session":"会话","model":"model/甲","purpose":"turn","error":"故障","body":"bodies/1"}"#;
        let escaped = r#"{"at_ms":2,"session":"\u4f1a\u8bdd","model":"model\/甲","purpose":"turn","finish_reason":"end_turn","extra":{"ignored":[1,null]}}"#;
        let parsed = serde_json::from_str::<LogRecord<LogText<'_>>>(plain).unwrap();
        assert!(matches!(parsed.purpose.0, Cow::Borrowed("turn")));
        assert!(matches!(&parsed.model.as_ref().unwrap().0, Cow::Borrowed("model/甲")));
        let first = strings.share_log(parsed, None);
        assert_eq!(first, serde_json::from_str::<RequestLog>(plain).unwrap());
        let parsed = serde_json::from_str::<LogRecord<LogText<'_>>>(escaped).unwrap();
        assert!(matches!(&parsed.model.as_ref().unwrap().0, Cow::Owned(_)));
        let second = strings.share_log(parsed, Some(&first));
        assert_eq!(second, serde_json::from_str::<RequestLog>(escaped).unwrap());
        assert!(Arc::ptr_eq(first.model.as_ref().unwrap(), second.model.as_ref().unwrap()));
        assert!(Arc::ptr_eq(first.session.as_ref().unwrap(), second.session.as_ref().unwrap()));

        for invalid in [
            r#"{"purpose":null}"#,
            r#"{"model":123}"#,
            r#"{"model":"a","model":"b"}"#,
            r#"{"model":"a"} false"#,
        ] {
            assert!(serde_json::from_str::<LogRecord<LogText<'_>>>(invalid).is_err());
            assert!(serde_json::from_str::<RequestLog>(invalid).is_err());
        }
    }

    #[test]
    fn resumes_complete_lines_and_keeps_timestamp_order() {
        let dir = fixture("log-resume");
        let old = dir.join("2026-11-18.jsonl");
        let today = dir.join("2026-11-19.jsonl");
        fs::write(&old, b"{\"at_ms\":10,\"req_id\":1}\n").unwrap();
        let mut initial = vec![b' '; READ_BUFFER_BYTES as usize - 8];
        initial.extend_from_slice(b"{\"at_ms\":20,\"req_id\":2}\r\ninvalid\n\xff\n");
        fs::write(&today, [initial.as_slice(), b"{\"at_ms\":15,\"req_id\":"].concat()).unwrap();

        let mut logs = Vec::new();
        let (cursor, _) = LogCursor::default().read(&dir, None).unwrap().apply(&mut logs);
        assert_eq!(cursor.files[1].position.offset, initial.len() as u64);
        let unchanged = cursor.read(&dir, None).unwrap();
        assert!(!unchanged.replace && unchanged.logs.is_empty());
        assert_eq!(unchanged.apply(&mut logs).1, LogChange::Unchanged);
        assert!(Arc::ptr_eq(&logs[0].purpose, &logs[1].purpose));

        append(&today, b"3}\n{\"at_ms\":20,\"req_id\":4}\n");
        let tomorrow = dir.join("2026-11-20.jsonl");
        fs::write(&tomorrow, b"{\"at_ms\":20,\"req_id\":5}\n").unwrap();
        let update = cursor.read(&dir, None).unwrap();
        assert!(!update.replace);
        assert_eq!(update.logs.len(), 3);
        let (cursor, changed) = update.apply(&mut logs);
        assert_eq!(changed, LogChange::Appended { start: 2 });
        assert_eq!(logs.iter().filter_map(|log| log.req_id).collect::<Vec<_>>(), [1, 2, 3, 4, 5]);
        let mut sorted = logs.clone();
        sorted.sort_by_key(|log| log.at_ms);
        assert_eq!(sorted, read_logs(&dir, None));

        let update = cursor.read(&dir, Some(18)).unwrap();
        assert!(!update.replace && update.logs.is_empty());
        let (cursor, changed) = update.apply(&mut logs);
        assert_eq!(changed, LogChange::Rebuilt);
        assert_eq!(logs, read_logs(&dir, Some(18)));
        let update = cursor.read(&dir, None).unwrap();
        assert!(update.replace);
        update.apply(&mut logs);
        assert_eq!(logs, read_logs(&dir, None));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reopens_changed_sources_without_mixing_old_and_new_rows() {
        let dir = fixture("log-replace");
        let first = dir.join("2026-11-18.jsonl");
        let second = dir.join("2026-11-19.jsonl");
        fs::write(&first, b"{\"at_ms\":10}\n").unwrap();
        fs::write(&second, b"{\"at_ms\":20}").unwrap();
        let mut logs = Vec::new();
        let (cursor, _) = LogCursor::default().read(&dir, None).unwrap().apply(&mut logs);
        assert_eq!(logs.len(), 2);
        append(&second, b"\n{\"at_ms\":21}\n");
        let update = cursor.read(&dir, None).unwrap();
        assert!(update.replace);
        let (cursor, _) = update.apply(&mut logs);
        assert_eq!(logs.len(), 3);

        fs::write(&second, b"{\"at_ms\":30}\n{\"at_ms\":31}\n{\"at_ms\":32}\n").unwrap();
        let update = cursor.read(&dir, None).unwrap();
        assert!(update.replace);
        let (cursor, _) = update.apply(&mut logs);
        assert_eq!(logs, read_logs(&dir, None));

        fs::rename(&second, dir.join("old.txt")).unwrap();
        fs::write(&second, b"{\"at_ms\":50}\n").unwrap();
        let update = cursor.read(&dir, None).unwrap();
        assert!(update.replace);
        let (cursor, _) = update.apply(&mut logs);
        assert_eq!(logs, read_logs(&dir, None));
        assert_eq!(logs.len(), 2);

        fs::remove_file(first).unwrap();
        let update = cursor.read(&dir, None).unwrap();
        assert!(update.replace);
        let (cursor, _) = update.apply(&mut logs);
        assert_eq!(logs, read_logs(&dir, None));
        assert_eq!(logs.len(), 1);

        fs::remove_dir_all(&dir).unwrap();
        cursor.read(&dir, None).unwrap().apply(&mut logs);
        assert!(logs.is_empty());
    }
}

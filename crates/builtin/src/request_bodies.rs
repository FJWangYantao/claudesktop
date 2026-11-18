//! 网关存下的请求原文：每个请求发给服务商的请求体（系统提示词、消息、工具定义）和收到的响应
//! （流式响应就是原样的 SSE），用量页的「请求明细」里点「原文」查看。
//!
//! ```text
//! gateway/bodies/<日期>/<发出时刻>-<序号>.req.json.gz   请求体（原样）
//!                      <发出时刻>-<序号>.resp.gz        响应（最多 RESPONSE_MAX 字节）
//! ```
//!
//! 日志那一行的 `body` 记着「`bodies/<日期>/<发出时刻>-<序号>`」。只存请求体，不存请求头（key 在头里）。
//! 每次请求都带着整段历史，体积涨得快：gzip 压缩，比请求日志留得短（[`KEEP_DAYS`] 天 / [`KEEP_BYTES`]）。
//! 设置里可以关掉（[`set_enabled`]），关了只是不再存，已经存的照样能看。

use chrono::NaiveDate;
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// `gateway/` 下放原文的子目录
const DIR: &str = "bodies";
/// 响应最多存多少字节（一轮回复很少超过这个；超了截断并注明）
pub(crate) const RESPONSE_MAX: usize = 2 << 20;
/// 原文留多少天 / 最多多大
const KEEP_DAYS: i64 = 7;
const KEEP_BYTES: u64 = 500 << 20;

static ENABLED: AtomicBool = AtomicBool::new(true);
/// 同一毫秒里的几个请求靠它分开
static SEQ: AtomicU64 = AtomicU64::new(0);

/// 存不存原文（设置里的开关，默认存）。
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

pub(crate) fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// 存一次请求的原文，返回记进日志的 ID；存不下返回 None（不影响转发和记日志）。
pub(crate) fn save(
    dir: &Path,
    date: NaiveDate,
    at_ms: u64,
    request: &[u8],
    response: &[u8],
) -> Option<String> {
    let id = format!(
        "{DIR}/{}/{at_ms}-{}",
        date.format("%Y-%m-%d"),
        SEQ.fetch_add(1, Ordering::Relaxed)
    );
    let base = dir.join(&id);
    std::fs::create_dir_all(base.parent()?).ok()?;
    write_gz(&with_suffix(&base, ".req.json.gz"), request).ok()?;
    write_gz(&with_suffix(&base, ".resp.gz"), response).ok()?;
    Some(id)
}

/// 读回来的原文。
#[derive(Debug, Clone, PartialEq)]
pub struct Saved {
    /// 请求体：是 JSON 就排好版，否则原样
    pub request: String,
    /// 响应（流式的是原样的 SSE）；没存到为空
    pub response: String,
    /// 响应超过 [`RESPONSE_MAX`] 被截断了
    pub truncated: bool,
}

#[derive(Clone)]
pub struct Source(PathBuf);

pub struct Request {
    pub text: String,
    pub formatted: bool,
}

pub struct Response {
    pub text: String,
    pub truncated: bool,
}

impl Source {
    /// 同一份日志上的请求与响应可单独读取。
    pub fn new(dir: &Path, id: &str) -> Result<Self, String> {
        let mut parts = id.split('/');
        if parts.next() != Some(DIR)
            || !parts.all(|part| {
                !part.is_empty() && part != "." && part != ".." && !part.contains(['\\', ':'])
            })
            || !id.contains('/')
        {
            return Err("原文的位置不对".into());
        }
        Ok(Self(dir.join(id)))
    }

    pub fn request(&self) -> Result<Request, String> {
        let bytes = read_gz(&with_suffix(&self.0, ".req.json.gz"))
            .map_err(|_| "原文已经清理掉了（只留最近 7 天）".to_string())?;
        if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes)
            && let Ok(text) = serde_json::to_string_pretty(&value)
        {
            return Ok(Request { text, formatted: true });
        }
        Ok(Request { text: decode_text(bytes), formatted: false })
    }

    pub fn response(&self) -> Response {
        let bytes = read_gz(&with_suffix(&self.0, ".resp.gz")).unwrap_or_default();
        let truncated = bytes.len() >= RESPONSE_MAX;
        Response { text: decode_text(bytes), truncated }
    }
}

fn decode_text(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes)
        .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned())
}

/// 按日志里的 ID 读完整原文。
pub fn load(dir: &Path, id: &str) -> Result<Saved, String> {
    let source = Source::new(dir, id)?;
    let request = source.request()?.text;
    let Response { text: response, truncated } = source.response();
    Ok(Saved { request, response, truncated })
}

pub fn find(text: &str, query: &str, before: bool, from: usize) -> Option<std::ops::Range<usize>> {
    if query.is_empty() {
        return None;
    }
    let from = from.min(text.len());
    let pattern = regex::bytes::RegexBuilder::new(&regex::escape(query))
        .unicode(false)
        .case_insensitive(true)
        .build()
        .ok()?;
    if before {
        pattern
            .find_iter(&text.as_bytes()[..from])
            .last()
            .or_else(|| pattern.find_iter(text.as_bytes()).last())
    } else {
        pattern.find_at(text.as_bytes(), from).or_else(|| pattern.find(text.as_bytes()))
    }
    .map(|hit| hit.range())
}

/// 删掉过期的（按日期目录），总量还超就从最旧的日期删（当天的不删）。
pub(crate) fn prune(gateway_dir: &Path, today: NaiveDate) {
    let Ok(entries) = std::fs::read_dir(gateway_dir.join(DIR)) else {
        return;
    };
    let mut days: Vec<(NaiveDate, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let date = NaiveDate::parse_from_str(e.file_name().to_str()?, "%Y-%m-%d").ok()?;
            Some((date, e.path()))
        })
        .collect();
    days.sort();
    let cutoff = today - chrono::Duration::days(KEEP_DAYS);
    days.retain(|(date, path)| {
        let old = *date < cutoff;
        if old {
            let _ = std::fs::remove_dir_all(path);
        }
        !old
    });
    let size = |path: &Path| -> u64 {
        std::fs::read_dir(path)
            .map(|es| es.flatten().filter_map(|e| e.metadata().ok()).map(|m| m.len()).sum())
            .unwrap_or(0)
    };
    let sizes: Vec<u64> = days.iter().map(|(_, p)| size(p)).collect();
    let mut total: u64 = sizes.iter().sum();
    for ((date, path), n) in days.iter().zip(sizes) {
        if total <= KEEP_BYTES || *date >= today {
            break;
        }
        if std::fs::remove_dir_all(path).is_ok() {
            total -= n;
        }
    }
}

/// 全部删掉（「清空请求日志」时一起）。
pub(crate) fn clear(gateway_dir: &Path) {
    let _ = std::fs::remove_dir_all(gateway_dir.join(DIR));
}

fn with_suffix(base: &Path, suffix: &str) -> PathBuf {
    let mut name = base.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

fn write_gz(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let mut encoder = GzEncoder::new(std::fs::File::create(path)?, Compression::fast());
    encoder.write_all(data)?;
    encoder.finish().map(drop)
}

fn read_gz(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    GzDecoder::new(std::fs::File::open(path)?).read_to_end(&mut out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_and_request_are_read_independently() {
        let dir = std::env::temp_dir().join(format!("cd-bodies-split-{}", std::process::id()));
        let today = NaiveDate::from_ymd_opt(2026, 10, 19).unwrap();
        let id = save(&dir, today, 1, b"bad JSON \xff", &vec![b'a'; RESPONSE_MAX]).unwrap();
        let source = Source::new(&dir, &id).unwrap();
        let response_path = with_suffix(&source.0, ".resp.gz");
        let saved = std::fs::read(&response_path).unwrap();
        std::fs::remove_file(&response_path).unwrap();
        let request = source.request().unwrap();
        assert_eq!(request.text, "bad JSON \u{fffd}");
        assert!(!request.formatted);
        assert!(source.response().text.is_empty());
        std::fs::write(&response_path, saved).unwrap();
        std::fs::remove_file(with_suffix(&source.0, ".req.json.gz")).unwrap();
        assert!(source.request().is_err());
        let response = source.response();
        assert!(response.truncated);
        assert_eq!(response.text.as_bytes(), vec![b'a'; RESPONSE_MAX]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn search_wraps_and_matches_literal_utf8_by_byte_offset() {
        let text = "头 Aa[界] 尾 aa[界]";
        let first = text.find("Aa[").unwrap();
        let last = text.find("aa[").unwrap();
        let query = "AA[界]";
        assert_eq!(find(text, query, false, 0), Some(first..first + query.len()));
        assert_eq!(find(text, query, false, first + query.len()), Some(last..text.len()));
        assert_eq!(find(text, query, false, text.len()), Some(first..first + query.len()));
        assert_eq!(find(text, query, true, last), Some(first..first + query.len()));
        assert_eq!(find(text, query, true, first), Some(last..text.len()));
        assert_eq!(find(text, "缺", false, 0), None);
    }

    #[test]
    fn saves_loads_and_prunes() {
        let dir = std::env::temp_dir().join(format!("cd-bodies-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let today = NaiveDate::from_ymd_opt(2026, 10, 19).unwrap();
        let id = save(&dir, today, 1, br#"{"model":"m","messages":[]}"#, b"data: x\n\n").unwrap();
        assert!(id.starts_with("bodies/2026-10-19/1-"));
        let saved = load(&dir, &id).unwrap();
        assert!(saved.request.contains("\"model\": \"m\""));
        assert_eq!(saved.response, "data: x\n\n");
        assert!(!saved.truncated);
        assert!(load(&dir, "bodies/../../etc/passwd").is_err());

        let old = today - chrono::Duration::days(KEEP_DAYS + 1);
        let old_id = save(&dir, old, 2, b"{}", b"").unwrap();
        prune(&dir, today);
        assert!(load(&dir, &old_id).is_err());
        assert!(load(&dir, &id).is_ok());
        clear(&dir);
        assert!(load(&dir, &id).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

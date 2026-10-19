//! 首页用量概览：从会话日志汇总每轮的时间、模型、token。
//!
//! 每轮结束时日志里写一条 [`Record::Turn`]；这之前的旧日志没有它，
//! 用户消息的时间就退回会话的最后更新时间、模型从内核标识里猜。

use claudesktop_core::{Record, SessionMeta};
use std::collections::{HashMap, HashSet};

/// 窗口用到这个比例，自研内核就会在下次请求前自动压缩（和内核同一个数，见
/// [`claudesktop_builtin::AUTO_COMPACT_PERCENT`]）。
pub const AUTO_COMPACT_PERCENT: u64 = claudesktop_builtin::AUTO_COMPACT_PERCENT;

/// 热力图：7 行（周一到周日）× 26 列（周），最后一列是本周。
pub const HEAT_ROWS: usize = 7;
pub const HEAT_COLS: usize = 26;

/// 一轮对话（一条用户消息）。
#[derive(Clone, Debug, PartialEq)]
pub struct TurnStat {
    /// 本地会话 ID（统计会话数用）
    pub session: String,
    pub at_ms: u64,
    pub model: Option<String>,
    pub tokens: u64,
}

/// 从一个会话的日志里取出每轮统计。
pub fn collect(meta: &SessionMeta, records: &[Record]) -> Vec<TurnStat> {
    let fallback_model = meta
        .agent
        .strip_prefix("builtin:")
        .and_then(|rest| rest.rsplit_once('#'))
        .map(|(_, model)| model.to_string());
    let mut turns = Vec::new();
    // 已发出、还没等到 Turn 记录的用户消息数
    let mut pending = 0usize;
    for record in records {
        match record {
            Record::User { .. } => pending += 1,
            Record::Turn { at_ms, model, input_tokens, output_tokens, .. } if pending > 0 => {
                pending -= 1;
                turns.push(TurnStat {
                    session: meta.id.clone(),
                    at_ms: *at_ms,
                    model: model.clone(),
                    tokens: input_tokens + output_tokens,
                });
            }
            _ => {}
        }
    }
    for _ in 0..pending {
        turns.push(TurnStat {
            session: meta.id.clone(),
            at_ms: meta.updated_ms,
            model: fallback_model.clone(),
            tokens: 0,
        });
    }
    turns
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModelRow {
    pub name: String,
    pub turns: usize,
    pub tokens: u64,
    /// 用过它的会话数
    pub sessions: usize,
    /// 最近一次用它的时刻
    pub last_ms: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Overview {
    pub sessions: usize,
    pub messages: usize,
    pub tokens: u64,
    pub active_days: usize,
    /// 本地时间的小时 0..24
    pub peak_hour: Option<u32>,
    /// 按轮数从多到少
    pub models: Vec<ModelRow>,
    /// 行优先，0..=4；未来的日子为 None
    pub heat: Vec<Option<u8>>,
    /// 每格那天的轮数（与 `heat` 一一对应）
    pub heat_turns: Vec<usize>,
    /// 热力图第一格是哪天（[`day_of`] 的天序号）；第 `row` 行第 `col` 列 = `heat_first + col * 7 + row`
    pub heat_first: i64,
}

/// 本地日历日的天序号（1970-01-01 = 0）。
pub fn day_of(ms: u64, offset_secs: i64) -> i64 {
    (ms as i64 / 1000 + offset_secs).div_euclid(86_400)
}

/// 汇总。`range_days` = None 表示全部；`offset_secs` 是本地时区相对 UTC 的偏移。
/// 热力图不受范围影响，总是最近 26 周。
pub fn overview(
    turns: &[TurnStat],
    now_ms: u64,
    offset_secs: i64,
    range_days: Option<u64>,
) -> Overview {
    let day_of = |ms: u64| day_of(ms, offset_secs);
    let hour_of = |ms: u64| hour_of(ms, offset_secs);
    let today = day_of(now_ms);
    let in_range = in_range(turns, now_ms, offset_secs, range_days);

    let sessions: HashSet<&str> = in_range.iter().map(|t| t.session.as_str()).collect();
    let days: HashSet<i64> = in_range.iter().map(|t| day_of(t.at_ms)).collect();
    let mut hours = [0usize; 24];
    let mut models: HashMap<&str, (ModelRow, HashSet<&str>)> = HashMap::new();
    for t in &in_range {
        hours[hour_of(t.at_ms) as usize] += 1;
        if let Some(name) = t.model.as_deref() {
            let (row, sessions) = models.entry(name).or_insert_with(|| {
                let row = ModelRow {
                    name: name.to_string(),
                    turns: 0,
                    tokens: 0,
                    sessions: 0,
                    last_ms: 0,
                };
                (row, HashSet::new())
            });
            row.turns += 1;
            row.tokens += t.tokens;
            row.last_ms = row.last_ms.max(t.at_ms);
            sessions.insert(&t.session);
            row.sessions = sessions.len();
        }
    }
    let peak_hour = (0..24u32)
        .filter(|h| hours[*h as usize] > 0)
        .max_by_key(|h| (hours[*h as usize], std::cmp::Reverse(*h)));
    let mut models: Vec<ModelRow> = models.into_values().map(|(row, _)| row).collect();
    models.sort_by(|a, b| {
        b.turns.cmp(&a.turns).then(b.tokens.cmp(&a.tokens)).then(a.name.cmp(&b.name))
    });

    // 热力图：一列一周，周一在上
    let first = today - weekday(today) as i64 - (HEAT_COLS as i64 - 1) * 7;
    let mut per_day: HashMap<i64, usize> = HashMap::new();
    for t in turns {
        *per_day.entry(day_of(t.at_ms)).or_default() += 1;
    }
    let max = (0..(HEAT_COLS * HEAT_ROWS) as i64)
        .filter_map(|i| per_day.get(&(first + i)))
        .copied()
        .max()
        .unwrap_or(0);
    let mut heat = Vec::with_capacity(HEAT_ROWS * HEAT_COLS);
    let mut heat_turns = Vec::with_capacity(HEAT_ROWS * HEAT_COLS);
    for row in 0..HEAT_ROWS as i64 {
        for col in 0..HEAT_COLS as i64 {
            let day = first + col * 7 + row;
            heat_turns.push(per_day.get(&day).copied().unwrap_or(0));
            heat.push(if day > today {
                None
            } else {
                let n = per_day.get(&day).copied().unwrap_or(0);
                // 按当期最大值分 4 档
                Some(if n == 0 { 0 } else { (n * 4).div_ceil(max).clamp(1, 4) as u8 })
            });
        }
    }

    Overview {
        sessions: sessions.len(),
        messages: in_range.len(),
        tokens: in_range.iter().map(|t| t.tokens).sum(),
        active_days: days.len(),
        peak_hour,
        models,
        heat,
        heat_turns,
        heat_first: first,
    }
}

fn hour_of(ms: u64, offset_secs: i64) -> u32 {
    ((ms as i64 / 1000 + offset_secs).rem_euclid(86_400) / 3600) as u32
}

/// 时间范围内的轮（`range_days` = None 是全部）。
fn in_range(
    turns: &[TurnStat],
    now_ms: u64,
    offset_secs: i64,
    range_days: Option<u64>,
) -> Vec<&TurnStat> {
    let today = day_of(now_ms, offset_secs);
    turns
        .iter()
        .filter(|t| range_days.is_none_or(|d| today - day_of(t.at_ms, offset_secs) < d as i64))
        .collect()
}

/// 趋势图的一根柱子：一天，或者（时间跨度长时）一周。
#[derive(Clone, Debug, PartialEq)]
pub struct DayRow {
    /// [`day_of`] 的天序号（按周时是那周的周一）
    pub day: i64,
    pub turns: usize,
    pub tokens: u64,
}

/// 一个会话的汇总（用量页的会话排行）。
#[derive(Clone, Debug, PartialEq)]
pub struct SessionRow {
    /// [`TurnStat::session`]
    pub session: String,
    pub turns: usize,
    pub tokens: u64,
    pub last_ms: u64,
}

/// 用量页的筛选：点趋势图的一天、模型排行的一行、会话排行的一行，整页都只看它。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Filter<'a> {
    pub day: Option<i64>,
    pub model: Option<&'a str>,
    /// [`TurnStat::session`]
    pub session: Option<&'a str>,
}

impl Filter<'_> {
    fn keeps(&self, t: &TurnStat, offset_secs: i64) -> bool {
        self.keeps_except_day(t) && self.day.is_none_or(|d| day_of(t.at_ms, offset_secs) == d)
    }

    fn keeps_except_day(&self, t: &TurnStat) -> bool {
        self.model.is_none_or(|m| t.model.as_deref() == Some(m))
            && self.session.is_none_or(|s| t.session == s)
    }
}

/// 时间跨度超过这么多天，趋势图按周画（全部范围下几百根细柱子看不清）
pub const WEEKLY_AFTER_DAYS: i64 = 62;

/// 用量页一整页的数据。
#[derive(Clone, Debug, PartialEq)]
pub struct Board {
    pub turns: usize,
    pub tokens: u64,
    pub sessions: usize,
    pub active_days: usize,
    /// 上一个同样长的时段的（轮数, token），算涨跌用；范围是「全部」或只看某天时没有
    pub prev: Option<(usize, u64)>,
    /// 趋势图：从早到晚连续排（没对话的日子也有一根 0），不受「某天」的筛选影响
    pub series: Vec<DayRow>,
    /// `series` 一根是一周
    pub weekly: bool,
    /// 按轮数从多到少
    pub models: Vec<ModelRow>,
    /// 按 token 从多到少（一样多按轮数、最近活动）
    pub session_rows: Vec<SessionRow>,
    /// 星期（周一 = 0）× 本地时间的小时，各多少轮
    pub heat: [[usize; 24]; 7],
}

/// 汇总时间范围里、筛选留下的轮。`range_days` = None 是全部。
pub fn board(
    turns: &[TurnStat],
    now_ms: u64,
    offset_secs: i64,
    range_days: Option<u64>,
    filter: Filter,
) -> Board {
    let today = day_of(now_ms, offset_secs);
    let in_range = in_range(turns, now_ms, offset_secs, range_days);
    let picked: Vec<&TurnStat> =
        in_range.iter().copied().filter(|t| filter.keeps(t, offset_secs)).collect();

    let mut sessions: HashMap<&str, SessionRow> = HashMap::new();
    let mut models: HashMap<&str, (ModelRow, HashSet<&str>)> = HashMap::new();
    let mut days: HashSet<i64> = HashSet::new();
    let mut heat = [[0usize; 24]; 7];
    for t in &picked {
        let d = day_of(t.at_ms, offset_secs);
        days.insert(d);
        heat[weekday(d)][hour_of(t.at_ms, offset_secs) as usize] += 1;
        let row = sessions.entry(&t.session).or_insert_with(|| SessionRow {
            session: t.session.clone(),
            turns: 0,
            tokens: 0,
            last_ms: 0,
        });
        row.turns += 1;
        row.tokens += t.tokens;
        row.last_ms = row.last_ms.max(t.at_ms);
        if let Some(name) = t.model.as_deref() {
            let (row, seen) = models.entry(name).or_insert_with(|| {
                let row = ModelRow {
                    name: name.to_string(),
                    turns: 0,
                    tokens: 0,
                    sessions: 0,
                    last_ms: 0,
                };
                (row, HashSet::new())
            });
            row.turns += 1;
            row.tokens += t.tokens;
            row.last_ms = row.last_ms.max(t.at_ms);
            seen.insert(&t.session);
            row.sessions = seen.len();
        }
    }
    let mut models: Vec<ModelRow> = models.into_values().map(|(row, _)| row).collect();
    models.sort_by(|a, b| {
        b.turns.cmp(&a.turns).then(b.tokens.cmp(&a.tokens)).then(a.name.cmp(&b.name))
    });
    let mut session_rows: Vec<SessionRow> = sessions.into_values().collect();
    session_rows.sort_by(|a, b| {
        b.tokens.cmp(&a.tokens).then(b.turns.cmp(&a.turns)).then(b.last_ms.cmp(&a.last_ms))
    });

    let prev = match (range_days, filter.day) {
        (Some(n), None) => {
            let n = n as i64;
            let (mut turns_n, mut tokens) = (0, 0);
            for t in turns.iter().filter(|t| filter.keeps_except_day(t)) {
                let ago = today - day_of(t.at_ms, offset_secs);
                if (n..2 * n).contains(&ago) {
                    turns_n += 1;
                    tokens += t.tokens;
                }
            }
            Some((turns_n, tokens))
        }
        _ => None,
    };

    // 趋势：范围内连续的日子；「全部」从有记录的第一天起，跨度长了按周
    let trend: Vec<&TurnStat> =
        in_range.iter().copied().filter(|t| filter.keeps_except_day(t)).collect();
    let first = match range_days {
        Some(n) => today - n as i64 + 1,
        None => trend.iter().map(|t| day_of(t.at_ms, offset_secs)).min().unwrap_or(today),
    };
    let weekly = today - first >= WEEKLY_AFTER_DAYS;
    let bucket = |d: i64| if weekly { d - weekday(d) as i64 } else { d };
    let (start, step) = (bucket(first), if weekly { 7 } else { 1 });
    let mut series: Vec<DayRow> = (0..=(today - start) / step)
        .map(|i| DayRow { day: start + i * step, turns: 0, tokens: 0 })
        .collect();
    for t in &trend {
        let i = ((bucket(day_of(t.at_ms, offset_secs)) - start) / step) as usize;
        if let Some(row) = series.get_mut(i) {
            row.turns += 1;
            row.tokens += t.tokens;
        }
    }

    Board {
        turns: picked.len(),
        tokens: picked.iter().map(|t| t.tokens).sum(),
        sessions: session_rows.len(),
        active_days: days.len(),
        prev,
        series,
        weekly,
        models,
        session_rows,
        heat,
    }
}

/// 周一 = 0 … 周日 = 6（1970-01-01 是周四）。
pub fn weekday(day: i64) -> usize {
    (day + 3).rem_euclid(7) as usize
}

pub const WEEKDAYS: [&str; 7] = ["周一", "周二", "周三", "周四", "周五", "周六", "周日"];

/// 「10月12日 周日」；不是 `today` 那一年的带上年份。
pub fn date_label(day: i64, today: i64) -> String {
    let date = |d: i64| {
        chrono::NaiveDate::from_ymd_opt(1970, 1, 1).unwrap_or_default() + chrono::Duration::days(d)
    };
    use chrono::Datelike as _;
    let (d, now) = (date(day), date(today));
    let md = format!("{}月{}日 {}", d.month(), d.day(), WEEKDAYS[weekday(day)]);
    if d.year() == now.year() { md } else { format!("{}年{md}", d.year()) }
}

/// 「凌晨 3 点 / 上午 9 点 / 中午 12 点 / 下午 3 点 / 晚上 9 点」
pub fn hour_label(h: u32) -> String {
    match h {
        0..=5 => format!("凌晨 {h} 点"),
        6..=11 => format!("上午 {h} 点"),
        12 => "中午 12 点".into(),
        13..=17 => format!("下午 {} 点", h - 12),
        _ => format!("晚上 {} 点", h - 12),
    }
}

/// 1,234 / 12.3K / 4.5M
pub fn compact(n: u64) -> String {
    match n {
        0..10_000 => {
            let s = n.to_string();
            if s.len() > 3 { format!("{},{}", &s[..s.len() - 3], &s[s.len() - 3..]) } else { s }
        }
        10_000..1_000_000 => format!("{:.1}K", n as f64 / 1e3),
        1_000_000..1_000_000_000 => format!("{:.1}M", n as f64 / 1e6),
        _ => format!("{:.1}B", n as f64 / 1e9),
    }
}

/// 输入框旁的上下文用量（圆环 + 百分比，悬停看这张卡的细节）。
#[derive(Clone, Debug, PartialEq)]
pub struct ContextGauge {
    /// 占窗口的百分比（1–100，有用量就至少 1）；不知道窗口大小时为 None
    pub percent: Option<u32>,
    /// 上下文里现在的 token
    pub used: u64,
    /// 模型的上下文窗口；端点没报告时为 None
    pub window: Option<u64>,
    /// 到自动压缩（窗口的 [`AUTO_COMPACT_PERCENT`]）还剩多少 token：不知道窗口、
    /// 或已经过了阈值时为 None
    pub until_compact: Option<u64>,
    /// 显示的字：`9%`；不知道窗口时 `上下文 23.4K`
    pub label: String,
}

/// 已用 `used` token、窗口 `window` 时上下文用量怎么显示。
pub fn context_gauge(used: u64, window: Option<u64>) -> ContextGauge {
    match window.filter(|w| *w > 0) {
        Some(w) => {
            let percent = ((used.saturating_mul(100) + w / 2) / w).clamp(1, 100) as u32;
            ContextGauge {
                percent: Some(percent),
                used,
                window: Some(w),
                // 自研内核在窗口的这个比例上自动压缩（见 claudesktop-builtin 的 compact 模块）
                until_compact: (w * AUTO_COMPACT_PERCENT / 100).checked_sub(used),
                label: format!("{percent}%"),
            }
        }
        None => ContextGauge {
            percent: None,
            used,
            window: None,
            until_compact: None,
            label: format!("上下文 {}", short_tokens(used)),
        },
    }
}

/// 窗口大小多是整千：256.0K → 256K。悬停卡里的数字用它。
pub fn short_tokens(n: u64) -> String {
    compact(n).replace(".0K", "K").replace(".0M", "M")
}

/// 侧栏按日期分组的组名，下标即 [`day_bucket`] 的返回值。
pub const DAY_BUCKETS: [&str; 4] = ["今天", "昨天", "最近 7 天", "更早"];

/// 最后活动在 `updated_ms` 的会话落在哪个日期组（按本地日历日算，`offset_secs` 见 [`local_offset_secs`]）。
pub fn day_bucket(updated_ms: u64, now_ms: u64, offset_secs: i64) -> usize {
    let day_of = |ms: u64| (ms as i64 / 1000 + offset_secs).div_euclid(86_400);
    match day_of(now_ms) - day_of(updated_ms) {
        i64::MIN..=0 => 0,
        1 => 1,
        2..=6 => 2,
        _ => 3,
    }
}

/// 命中缓存的比例（%）：没报告（0）或没有输入时为 None。
pub fn cache_hit_percent(cache_read: u64, input: u64) -> Option<u32> {
    (cache_read > 0 && input > 0)
        .then(|| ((cache_read.min(input) * 100 + input / 2) / input) as u32)
}

/// 流式里正在收到、端点还没报过用量的那部分输出，估算它占多少 token。
///
/// 端点（尤其 ACP agent）通常要等一轮结束才给 `UsageUpdated`，这之前状态行上的数字靠估算，
/// 每收到一块正文 / 思考就涨一点：ASCII 按 4 字节 ≈ 1 token（同 [`crate::usage`] 里压缩的估法），
/// 中文这类宽字符按 1 字符 ≈ 1 token。计数留着原始量、最后才算除法，免得每块都取整丢成 0。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Streamed {
    ascii_bytes: u64,
    wide_chars: u64,
}

impl Streamed {
    /// 又收到一块文字。
    pub fn push(&mut self, text: &str) {
        for c in text.chars() {
            if c.is_ascii() {
                self.ascii_bytes += 1;
            } else {
                self.wide_chars += 1;
            }
        }
    }

    /// 估出来的 token 数。
    pub fn tokens(&self) -> u64 {
        self.ascii_bytes / 4 + self.wide_chars
    }

    /// 端点数过这批输出了，重新开始估。
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

/// 首页底部那句话：token 换算成《善恶的彼岸》（按一本约 8 万 token）。
pub fn book_line(tokens: u64) -> String {
    const BOOK: u64 = 80_000;
    match tokens {
        0 => "还没有 token 用量记录（自研内核每轮都会上报，ACP agent 一般不报）。".into(),
        t if t < BOOK => {
            format!("你的 Token 用量大约是一本《善恶的彼岸》的 {}%。", (t * 100 / BOOK).max(1))
        }
        t => format!("你的 Token 用量相当于 {} 本《善恶的彼岸》。", compact(t / BOOK)),
    }
}

/// 本地时区相对 UTC 的秒数。
pub fn local_offset_secs() -> i64 {
    use chrono::Offset as _;
    chrono::Local::now().offset().fix().local_minus_utc() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 86_400_000;

    fn meta(agent: &str) -> SessionMeta {
        SessionMeta {
            id: "s".into(),
            title: String::new(),
            cwd: ".".into(),
            agent: agent.into(),
            kernel_session_id: None,
            kernel_sessions: Vec::new(),
            created_ms: 0,
            updated_ms: 99,
            archived: false,
            task: false,
        }
    }

    fn user() -> Record {
        Record::user("x")
    }

    fn turn(at_ms: u64, model: Option<&str>, tokens: u64) -> Record {
        Record::Turn {
            at_ms,
            model: model.map(Into::into),
            input_tokens: tokens,
            output_tokens: 0,
            elapsed_ms: None,
            context_tokens: None,
        }
    }

    #[core::prelude::v1::test]
    fn collects_turns_and_falls_back_for_old_logs() {
        let records = vec![user(), turn(10, Some("m1"), 5), user(), user()];
        let turns = collect(&meta("builtin:http://x/v1#gem"), &records);
        assert_eq!(turns.len(), 3);
        assert_eq!(
            (turns[0].at_ms, turns[0].model.as_deref(), turns[0].tokens),
            (10, Some("m1"), 5)
        );
        // 没有 Turn 记录的：会话更新时间 + 从内核标识里取的模型
        assert_eq!((turns[2].at_ms, turns[2].model.as_deref()), (99, Some("gem")));
        assert_eq!(collect(&meta("node mock.mjs"), &[user()])[0].model, None);
    }

    #[core::prelude::v1::test]
    fn overview_ranges_peak_and_heat() {
        // 2026-10-14（周三）12:00 UTC
        let now = 1_791_979_200_000;
        let t = |session: &str, at_ms: u64, model: Option<&str>, tokens: u64| TurnStat {
            session: session.into(),
            at_ms,
            model: model.map(Into::into),
            tokens,
        };
        let turns = vec![
            t("a", now, Some("m1"), 100),
            t("a", now - 3_600_000, Some("m1"), 50),
            t("b", now - 10 * DAY, Some("m2"), 7),
            t("c", now - 400 * DAY, None, 0),
        ];
        let all = overview(&turns, now, 0, None);
        assert_eq!((all.sessions, all.messages, all.tokens, all.active_days), (3, 4, 157, 3));
        assert_eq!(all.peak_hour, Some(12));
        assert_eq!(
            all.models[0],
            ModelRow { name: "m1".into(), turns: 2, tokens: 150, sessions: 1, last_ms: now }
        );
        let week = overview(&turns, now, 0, Some(7));
        assert_eq!((week.sessions, week.messages, week.tokens), (1, 2, 150));
        // 东八区：12:00 UTC 是晚上 8 点
        assert_eq!(overview(&turns, now, 8 * 3600, None).peak_hour, Some(20));

        // 今天（周三）在最后一列第 3 行，最满；周四之后是未来
        let heat = &all.heat;
        assert_eq!(heat.len(), HEAT_ROWS * HEAT_COLS);
        let cell = |row: usize, col: usize| heat[row * HEAT_COLS + col];
        assert_eq!(cell(2, HEAT_COLS - 1), Some(4));
        assert_eq!(cell(3, HEAT_COLS - 1), None);
        assert_eq!(cell(0, 0), Some(0));
        // 10 天前（上上周日）有 1 轮：2/最大 2 → 1 轮是 2 档
        assert_eq!(cell(6, HEAT_COLS - 3), Some(2));
        // 格子对得回日子
        let today = day_of(now, 0);
        assert_eq!(all.heat_first + (HEAT_COLS as i64 - 1) * 7 + 2, today);
    }

    #[core::prelude::v1::test]
    fn board_filters_trends_and_compares() {
        // 2026-10-14（周三）12:00 UTC
        let now = 1_791_979_200_000;
        let t = |session: &str, at_ms: u64, model: Option<&str>, tokens: u64| TurnStat {
            session: session.into(),
            at_ms,
            model: model.map(Into::into),
            tokens,
        };
        let turns = vec![
            t("a", now, Some("m1"), 100),
            t("a", now - 3_600_000, Some("m2"), 50),
            t("a", now - 2 * 3_600_000, Some("m2"), 1),
            t("b", now - DAY, Some("m2"), 400),
            t("b", now - 8 * DAY, Some("m2"), 30),
            t("c", now - 100 * DAY, None, 9),
        ];
        let today = day_of(now, 0);
        let week = board(&turns, now, 0, Some(7), Filter::default());
        assert_eq!((week.turns, week.tokens, week.sessions, week.active_days), (4, 551, 2, 2));
        // 上一个 7 天里只有 8 天前那一轮
        assert_eq!(week.prev, Some((1, 30)));
        // 7 根柱子，最后一根是今天
        assert_eq!(week.series.len(), 7);
        assert_eq!((week.series[6].day, week.series[6].turns), (today, 3));
        assert_eq!(week.series[5].tokens, 400);
        assert!(!week.weekly);
        // 周三中午 12 点那格
        assert_eq!(week.heat[2][12], 1);
        assert_eq!(week.models[0].name, "m2");
        assert_eq!(week.session_rows[0].session, "b");

        // 只看今天：数字跟着变，趋势还是整段；没有涨跌
        let day = board(&turns, now, 0, Some(7), Filter { day: Some(today), ..Default::default() });
        assert_eq!((day.turns, day.sessions, day.prev), (3, 1, None));
        assert_eq!(day.series, week.series);
        // 只看一个模型：趋势也只算它
        let m1 = board(&turns, now, 0, Some(7), Filter { model: Some("m1"), ..Default::default() });
        assert_eq!((m1.turns, m1.series[5].tokens, m1.prev), (1, 0, Some((0, 0))));
        let b = board(&turns, now, 0, None, Filter { session: Some("b"), ..Default::default() });
        assert_eq!((b.turns, b.tokens), (2, 430));
        assert!(!b.weekly);

        // 全部：从第一轮那天起，100 天按周画，每周一根、从周一起
        let all = board(&turns, now, 0, None, Filter::default());
        assert!(all.weekly);
        assert!(all.series.iter().all(|w| weekday(w.day) == 0));
        assert_eq!(all.series.iter().map(|w| w.turns).sum::<usize>(), 6);
        assert_eq!(all.series.last().map(|w| w.turns), Some(4));

        assert_eq!(date_label(today, today), "10月14日 周三");
        assert_eq!(date_label(today - 300, today), "2025年12月18日 周四");
    }

    #[core::prelude::v1::test]
    fn labels() {
        assert_eq!(hour_label(0), "凌晨 0 点");
        assert_eq!(hour_label(12), "中午 12 点");
        assert_eq!(hour_label(15), "下午 3 点");
        assert_eq!(hour_label(21), "晚上 9 点");
        assert_eq!(compact(4194), "4,194");
        assert_eq!(compact(299_400_000), "299.4M");
        assert_eq!(compact(12_345), "12.3K");
        assert!(book_line(40_000).contains("50%"));
        assert!(book_line(800_000).contains("10 本"));
    }

    #[core::prelude::v1::test]
    fn day_bucket_follows_local_calendar_days() {
        const H: u64 = 3_600_000;
        // 东八区 2026-10-15 00:30（UTC 10-14 16:30）
        let now = 1_791_995_400_000;
        let cn = 8 * 3600;
        assert_eq!(day_bucket(now - H / 4, now, cn), 0);
        // 一小时前是本地的昨天 23:30，不是「今天」
        assert_eq!(day_bucket(now - H, now, cn), 1);
        assert_eq!(day_bucket(now - 3 * 24 * H, now, cn), 2);
        assert_eq!(day_bucket(now - 30 * 24 * H, now, cn), 3);
        // 时钟回拨 / 刚建的会话也算今天
        assert_eq!(day_bucket(now + H, now, cn), 0);
    }

    #[core::prelude::v1::test]
    fn cache_hit_needs_a_report() {
        assert_eq!(cache_hit_percent(0, 1000), None);
        assert_eq!(cache_hit_percent(805, 1000), Some(81));
        assert_eq!(cache_hit_percent(5, 0), None);
    }

    #[core::prelude::v1::test]
    fn context_gauge_rounds_and_clamps() {
        let g = context_gauge(23_400, Some(256_000));
        assert_eq!(g.percent, Some(9));
        assert_eq!(g.label, "9%");
        assert_eq!((g.used, g.window), (23_400, Some(256_000)));
        // 到自动压缩（窗口的八成 = 204.8K）还剩 181.4K
        assert_eq!(g.until_compact, Some(204_800 - 23_400));
        // 刚开头也至少 1%；超出窗口（换了个小模型）按满算
        assert_eq!(context_gauge(300, Some(1_000_000)).percent, Some(1));
        assert_eq!(context_gauge(300_000, Some(128_000)).percent, Some(100));
        // 已经过了自动压缩线：没有「还剩多少」
        assert_eq!(context_gauge(210_000, Some(256_000)).until_compact, None);
        // 不知道窗口：只说用了多少
        let g = context_gauge(4_194, None);
        assert_eq!((g.percent, g.label.as_str()), (None, "上下文 4,194"));
        assert_eq!((g.used, g.until_compact), (4_194, None));
        assert_eq!(context_gauge(5, Some(0)).percent, None);
    }
}

//! 首页用量概览：从会话日志汇总每轮的时间、模型、token。
//!
//! 每轮结束时日志里写一条 [`Record::Turn`]；这之前的旧日志没有它，
//! 用户消息的时间就退回会话的最后更新时间、模型从内核标识里猜。

use claudesktop_core::{Record, SessionMeta};
use std::collections::{HashMap, HashSet};

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
}

/// 汇总。`range_days` = None 表示全部；`offset_secs` 是本地时区相对 UTC 的偏移。
/// 热力图不受范围影响，总是最近 26 周。
pub fn overview(
    turns: &[TurnStat],
    now_ms: u64,
    offset_secs: i64,
    range_days: Option<u64>,
) -> Overview {
    let day_of = |ms: u64| (ms as i64 / 1000 + offset_secs).div_euclid(86_400);
    let hour_of = |ms: u64| ((ms as i64 / 1000 + offset_secs).rem_euclid(86_400) / 3600) as u32;
    let today = day_of(now_ms);
    let in_range: Vec<&TurnStat> = turns
        .iter()
        .filter(|t| range_days.is_none_or(|d| today - day_of(t.at_ms) < d as i64))
        .collect();

    let sessions: HashSet<&str> = in_range.iter().map(|t| t.session.as_str()).collect();
    let days: HashSet<i64> = in_range.iter().map(|t| day_of(t.at_ms)).collect();
    let mut hours = [0usize; 24];
    let mut models: HashMap<&str, ModelRow> = HashMap::new();
    for t in &in_range {
        hours[hour_of(t.at_ms) as usize] += 1;
        if let Some(name) = t.model.as_deref() {
            let row = models.entry(name).or_insert_with(|| ModelRow {
                name: name.to_string(),
                turns: 0,
                tokens: 0,
            });
            row.turns += 1;
            row.tokens += t.tokens;
        }
    }
    let peak_hour = (0..24u32)
        .filter(|h| hours[*h as usize] > 0)
        .max_by_key(|h| (hours[*h as usize], std::cmp::Reverse(*h)));
    let mut models: Vec<ModelRow> = models.into_values().collect();
    models.sort_by(|a, b| {
        b.turns.cmp(&a.turns).then(b.tokens.cmp(&a.tokens)).then(a.name.cmp(&b.name))
    });

    // 热力图：1970-01-01 是周四，周一为 0
    let weekday = |day: i64| (day + 3).rem_euclid(7);
    let first = today - weekday(today) - (HEAT_COLS as i64 - 1) * 7;
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
    for row in 0..HEAT_ROWS as i64 {
        for col in 0..HEAT_COLS as i64 {
            let day = first + col * 7 + row;
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
    }
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

/// 输入框旁的上下文用量。
#[derive(Debug, PartialEq)]
pub struct ContextGauge {
    /// 占窗口的百分比（1–100，有用量就至少 1）；不知道窗口大小时为 None
    pub percent: Option<u32>,
    /// 显示的字：`9%`；不知道窗口时 `上下文 23.4K`
    pub label: String,
    /// 悬停提示：具体用了多少
    pub tooltip: String,
}

/// 已用 `used` token、窗口 `window` 时上下文用量怎么显示。
pub fn context_gauge(used: u64, window: Option<u64>) -> ContextGauge {
    // 窗口大小多是整千：256.0K → 256K
    let short = |n: u64| compact(n).replace(".0K", "K").replace(".0M", "M");
    match window.filter(|w| *w > 0) {
        Some(w) => {
            let percent = ((used.saturating_mul(100) + w / 2) / w).clamp(1, 100) as u32;
            ContextGauge {
                percent: Some(percent),
                label: format!("{percent}%"),
                tooltip: format!("上下文已用 {} / {} token", short(used), short(w)),
            }
        }
        None => ContextGauge {
            percent: None,
            label: format!("上下文 {}", short(used)),
            tooltip: format!("上下文已用 {} token（端点没报告窗口大小）", short(used)),
        },
    }
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
            created_ms: 0,
            updated_ms: 99,
            archived: false,
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
        assert_eq!(all.models[0], ModelRow { name: "m1".into(), turns: 2, tokens: 150 });
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
        assert_eq!(g.tooltip, "上下文已用 23.4K / 256K token");
        // 刚开头也至少 1%；超出窗口（换了个小模型）按满算
        assert_eq!(context_gauge(300, Some(1_000_000)).percent, Some(1));
        assert_eq!(context_gauge(300_000, Some(128_000)).percent, Some(100));
        // 不知道窗口：只说用了多少
        let g = context_gauge(4_194, None);
        assert_eq!((g.percent, g.label.as_str()), (None, "上下文 4,194"));
        assert_eq!(context_gauge(5, Some(0)).percent, None);
    }
}

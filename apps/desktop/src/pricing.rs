//! 模型单价表与按网关日志估算费用（首页「请求」页签用）。
//!
//! 单价按每百万 token 计，分输入、输出、缓存命中三档；币种只有美元和人民币，两种分开加总，不换算。
//! 表存在设置的 `model_prices`（没改过时用 [`default_prices`]），设置页里是一个多行文本框，
//! 一行一个模型：`名称 输入 输出 [缓存] [x倍数]`，数字前写 `$` 或 `¥`（不写算美元），`#` 开头是注释。
//! `x2` 表示这个模型分峰谷价：写的是谷价，落在峰时段的请求按 2 倍算（见 [`PeakWindow`]）。
//!
//! 峰时段单独占一行：`@peak 周几 时段… [时区]`，如 `@peak 1-5 09:00-12:00 14:00-18:00 +08:00`
//! （周几 1 = 周一 … 7 = 周日，`*` 是每天；时段左闭右开；时区不写就按本地时间）。
//!
//! 模型名按「包含」匹配、取最长的那条：`claude-sonnet-4` 能对上 `anthropic/claude-sonnet-4.5`。
//! 比较前小写、把 `.` 换成 `-`。

use chrono::{DateTime, FixedOffset, Local, Utc};
use claudesktop_builtin::gateway::RequestLog;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Currency {
    #[default]
    Usd,
    Cny,
}

impl Currency {
    pub fn symbol(self) -> &'static str {
        match self {
            Self::Usd => "$",
            Self::Cny => "¥",
        }
    }
}

/// 一个模型的单价（每百万 token）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Price {
    /// 模型名里包含这一段就算它
    pub model: String,
    pub currency: Currency,
    /// 输入价（分峰谷价的模型是谷价）
    pub input: f64,
    pub output: f64,
    /// 缓存命中的输入；None = 按普通输入算
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<f64>,
    /// 峰价是谷价的几倍；None（或 ≤ 1）= 不分峰谷。表里写 `x2`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_factor: Option<f64>,
}

impl Price {
    fn factor(&self) -> Option<f64> {
        self.peak_factor.filter(|f| *f > 1.0)
    }
}

/// 峰谷价的时段（本地时间或指定的时区）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PeakWindow {
    /// 星期，1 = 周一 … 7 = 周日
    pub days: Vec<u8>,
    /// 当天的时段，`[起, 止)`，从 0 点算的分钟数
    pub ranges: Vec<(u16, u16)>,
    /// 按哪个时区算（分钟偏移，北京时间 = 480）；None = 本地时间
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub utc_offset: Option<i32>,
}

impl PeakWindow {
    /// 这个时刻落不落在窗口里。
    fn contains(&self, at: DateTime<Utc>) -> bool {
        // (星期几 1..=7, 从 0 点算的分钟数)
        let (day, m) = match self.utc_offset {
            Some(off) => {
                let Some(tz) = FixedOffset::east_opt(off * 60) else {
                    return false;
                };
                let t = at.with_timezone(&tz);
                (weekday(&t), minutes_of(&t))
            }
            None => {
                let t = at.with_timezone(&Local);
                (weekday(&t), minutes_of(&t))
            }
        };
        self.days.contains(&day) && self.ranges.iter().any(|&(a, b)| m >= a && m < b)
    }

    /// 写回文本框的样子：`@peak 1-5 09:00-12:00 14:00-18:00 +08:00`
    fn to_spec(&self) -> String {
        let days = compress_days(&self.days);
        let ranges: Vec<String> =
            self.ranges.iter().map(|&(a, b)| format!("{}-{}", hm(a), hm(b))).collect();
        let tz = match self.utc_offset {
            Some(off) => {
                let sign = if off < 0 { '-' } else { '+' };
                let off = off.abs();
                format!(" {sign}{:02}:{:02}", off / 60, off % 60)
            }
            None => String::new(),
        };
        format!("@peak {days} {}{tz}", ranges.join(" "))
    }
}

/// 1..=7 的星期集合 → `1-5` / `3` / `*`。
fn compress_days(days: &[u8]) -> String {
    let mut days: Vec<u8> = days.iter().copied().filter(|d| (1..=7).contains(d)).collect();
    days.sort_unstable();
    days.dedup();
    match days.as_slice() {
        [1, 2, 3, 4, 5, 6, 7] => "*".into(),
        [one] => one.to_string(),
        [first, .., last] if *last - *first == days.len() as u8 - 1 => format!("{first}-{last}"),
        many => many.iter().map(u8::to_string).collect::<Vec<_>>().join(","),
    }
}

fn hm(minutes: u16) -> String {
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

/// 星期几，1 = 周一 … 7 = 周日。
fn weekday<T: chrono::TimeZone>(t: &DateTime<T>) -> u8 {
    u8::try_from(chrono::Datelike::weekday(t).number_from_monday()).unwrap_or(1)
}

/// 从 0 点算的分钟数。
fn minutes_of<T: chrono::TimeZone>(t: &DateTime<T>) -> u16 {
    let hour = u16::try_from(chrono::Timelike::hour(t)).unwrap_or(0);
    let minute = u16::try_from(chrono::Timelike::minute(t)).unwrap_or(0);
    hour * 60 + minute
}

/// 内置的参考价（2026 年 10 月各家官网标价；只留当前在售的型号，可能还会变，设置页可改）。
pub fn default_prices() -> Vec<Price> {
    let usd = |model: &str, input, output, cache| Price {
        model: model.into(),
        currency: Currency::Usd,
        input,
        output,
        cache_read: Some(cache),
        peak_factor: None,
    };
    let cny = |model: &str, input, output, cache| Price {
        currency: Currency::Cny,
        ..usd(model, input, output, cache)
    };
    // 分峰谷价的（DeepSeek）：表里写谷价，倍率 2
    let peak = |p: Price| Price { peak_factor: Some(2.0), ..p };
    vec![
        // Anthropic：缓存命中是输入价的 0.1 倍
        usd("claude-opus-5-5", 4.0, 20.0, 0.2),
        usd("claude-opus-5", 5.0, 25.0, 0.5),
        usd("claude-opus-4-5", 5.0, 25.0, 0.5),
        usd("claude-opus-4-6", 5.0, 25.0, 0.5),
        usd("claude-opus-4-7", 5.0, 25.0, 0.5),
        usd("claude-opus-4-8", 5.0, 25.0, 0.5),
        usd("claude-sonnet-4", 3.0, 15.0, 0.3),
        usd("claude-sonnet-5", 2.0, 10.0, 0.2),
        usd("claude-sonnet-5-5", 2.0, 10.0, 0.1),
        usd("claude-haiku-4-5", 1.0, 5.0, 0.1),
        // ≤ 10 万 token 的档；更长的输入是 0.5 / 2.5
        usd("claude-haiku-5-5", 0.1, 0.5, 0.01),
        // OpenAI：缓存命中是输入价的 0.1 倍
        usd("gpt-5", 1.25, 10.0, 0.125),
        usd("gpt-5-1", 1.25, 10.0, 0.125),
        usd("gpt-5-2", 1.75, 14.0, 0.175),
        usd("gpt-5-4", 2.5, 15.0, 0.25),
        usd("gpt-5-5", 5.0, 30.0, 0.5),
        usd("gpt-5-6-sol", 4.0, 20.0, 0.4),
        usd("gpt-5-6-terra", 2.0, 12.0, 0.2),
        usd("gpt-5-6-luna", 0.2, 1.2, 0.02),
        usd("gpt-5-mini", 0.25, 2.0, 0.025),
        usd("gpt-5-nano", 0.05, 0.4, 0.005),
        usd("gpt-5-4-mini", 0.75, 4.5, 0.075),
        usd("gpt-5-4-nano", 0.2, 1.25, 0.02),
        // 官方没有给 gpt-5-pro 的缓存价：命中按输入价算
        Price { cache_read: None, ..usd("gpt-5-pro", 15.0, 120.0, 15.0) },
        usd("gpt-5-3-codex", 1.75, 14.0, 0.175),
        usd("gpt-4-1", 2.0, 8.0, 0.5),
        usd("gpt-4-1-mini", 0.4, 1.6, 0.1),
        usd("gpt-4-1-nano", 0.1, 0.4, 0.025),
        // Google：缓存命中是输入价的 0.1 倍（2.5 这代是 0.1）
        usd("gemini-2-5-pro", 1.25, 10.0, 0.125),
        usd("gemini-2-5-flash", 0.3, 2.5, 0.03),
        usd("gemini-3-1-pro", 2.0, 12.0, 0.2),
        usd("gemini-3-8-flash", 0.75, 3.75, 0.075),
        // 国内（人民币计价）；DeepSeek 的谷价是峰价的一半
        peak(cny("deepseek-flash", 1.0, 4.0, 0.02)),
        // 官方说老名字 `deepseek-v4-flash` / `deepseek-v4.1-flash` 也按 Flash 价格计费；
        // 写成 `deepseek-v4` 是因为按包含匹配，v4-pro 那条更长会赢
        peak(cny("deepseek-v4", 1.0, 4.0, 0.02)),
        peak(cny("deepseek-v4-pro", 4.5, 13.5, 0.15)),
        usd("kimi-k3", 3.0, 15.0, 0.3),
        usd("kimi-k2-7-code", 0.95, 4.0, 0.19),
        usd("kimi-k2-6", 0.95, 4.0, 0.16),
        cny("glm-4-7", 2.0, 8.0, 0.4),
        cny("glm-5", 4.0, 18.0, 1.0),
        cny("glm-5-2", 8.0, 28.0, 2.0),
        cny("glm-5-3", 8.0, 28.0, 2.0),
        cny("glm-5-3-flash", 0.8, 2.8, 0.23),
    ]
}

/// 内置的峰时段：DeepSeek 的北京时间工作日上午和下午（其余时间、周末、法定节假日算谷价）。
pub fn default_peaks() -> Vec<PeakWindow> {
    vec![PeakWindow {
        days: vec![1, 2, 3, 4, 5],
        ranges: vec![(9 * 60, 12 * 60), (14 * 60, 18 * 60)],
        utc_offset: Some(8 * 60),
    }]
}

/// 单价表 + 峰时段（设置页那个文本框的完整内容）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Table {
    pub prices: Vec<Price>,
    pub peaks: Vec<PeakWindow>,
}

impl Table {
    pub fn defaults() -> Self {
        Self { prices: default_prices(), peaks: default_peaks() }
    }
}

fn normalize(name: &str) -> String {
    name.trim().to_lowercase().replace('.', "-")
}

/// 模型对应的单价：名字里包含的条目中最长的那条。
pub fn find<'a>(prices: &'a [Price], model: &str) -> Option<&'a Price> {
    let model = normalize(model);
    prices
        .iter()
        .filter(|p| !p.model.trim().is_empty() && model.contains(&normalize(&p.model)))
        .max_by_key(|p| normalize(&p.model).len())
}

/// 这个时刻是不是峰时（任一窗口命中就算）。
pub fn is_peak(peaks: &[PeakWindow], at_ms: u64) -> bool {
    let Some(at) = DateTime::from_timestamp_millis(at_ms as i64) else {
        return false;
    };
    peaks.iter().any(|w| w.contains(at))
}

/// 这个模型的这次请求是不是峰价；不分峰谷的模型是 None。
pub fn peak_of(p: &Price, peaks: &[PeakWindow], r: &RequestLog) -> Option<bool> {
    p.factor().map(|_| is_peak(peaks, r.at_ms))
}

/// 这次请求的倍率（谷价 1、峰价 `peak_factor`）。
fn factor(p: &Price, peaks: &[PeakWindow], at_ms: u64) -> f64 {
    match p.factor() {
        Some(f) if is_peak(peaks, at_ms) => f,
        _ => 1.0,
    }
}

/// 一次请求的费用。`input` 含缓存命中的部分（网关日志的口径）。
pub fn request_cost(p: &Price, r: &RequestLog, peaks: &[PeakWindow]) -> f64 {
    let cached = r.cache_read.min(r.input);
    let fresh = r.input - cached;
    let f = factor(p, peaks, r.at_ms);
    (fresh as f64 * p.input
        + cached as f64 * p.cache_read.unwrap_or(p.input)
        + r.output as f64 * p.output)
        * f
        / 1e6
}

/// 一批请求的估算费用：两种币种分开加，另记缓存省下的钱、按峰价算的请求数和对不上单价的模型。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cost {
    pub usd: f64,
    pub cny: f64,
    /// 缓存命中比按原价少花的（同样分币种）
    pub saved_usd: f64,
    pub saved_cny: f64,
    /// 按峰价算的请求数（分峰谷价的模型才计）
    pub peak: usize,
    /// 没有单价的模型（去重，按出现顺序）
    pub unpriced: Vec<String>,
}

impl Cost {
    pub fn of<'a>(
        prices: &[Price],
        peaks: &[PeakWindow],
        logs: impl IntoIterator<Item = &'a RequestLog>,
    ) -> Self {
        let mut cost = Self::default();
        for r in logs {
            let Some(model) = r.model.as_deref() else {
                continue;
            };
            if r.input == 0 && r.output == 0 {
                continue;
            }
            let Some(p) = find(prices, model) else {
                if !cost.unpriced.iter().any(|m| m == model) {
                    cost.unpriced.push(model.to_string());
                }
                continue;
            };
            let f = factor(p, peaks, r.at_ms);
            if f > 1.0 {
                cost.peak += 1;
            }
            let saved =
                r.cache_read.min(r.input) as f64 * (p.input - p.cache_read.unwrap_or(p.input)) * f
                    / 1e6;
            let (total, saved_total) = match p.currency {
                Currency::Usd => (&mut cost.usd, &mut cost.saved_usd),
                Currency::Cny => (&mut cost.cny, &mut cost.saved_cny),
            };
            *total += request_cost(p, r, peaks);
            *saved_total += saved.max(0.0);
        }
        cost
    }

    /// `$1.23` / `¥0.45` / `$1.23 + ¥0.45`；都没有时 None。
    pub fn label(&self) -> Option<String> {
        let parts = self.parts();
        (!parts.is_empty()).then(|| parts.join(" + "))
    }

    /// 各币种的金额，美元在前；没花钱的币种不列。
    pub fn parts(&self) -> Vec<String> {
        amounts(self.usd, self.cny)
    }

    pub fn saved_label(&self) -> Option<String> {
        let parts = amounts(self.saved_usd, self.saved_cny);
        (!parts.is_empty()).then(|| parts.join(" + "))
    }
}

fn amounts(usd: f64, cny: f64) -> Vec<String> {
    [(usd, Currency::Usd), (cny, Currency::Cny)]
        .into_iter()
        .filter(|(v, _)| *v > 0.0)
        .map(|(v, c)| money(v, c))
        .collect()
}

/// 金额：不到一分钱给到四位小数，其余两位。
pub fn money(v: f64, c: Currency) -> String {
    let s = c.symbol();
    if v > 0.0 && v < 0.01 { format!("{s}{v:.4}") } else { format!("{s}{v:.2}") }
}

/// 单价表 → 设置页文本框里的文字。
pub fn to_text(t: &Table) -> String {
    let num = |v: f64, c: Currency| {
        // 去掉多余的 0：3.00 → 3、0.125 → 0.125
        let n = format!("{v:.4}");
        let n = n.trim_end_matches('0').trim_end_matches('.');
        format!("{}{n}", c.symbol())
    };
    let mut lines: Vec<String> = t.peaks.iter().map(PeakWindow::to_spec).collect();
    lines.extend(t.prices.iter().map(|p| {
        let mut line =
            format!("{} {} {}", p.model, num(p.input, p.currency), num(p.output, p.currency));
        if let Some(c) = p.cache_read {
            line.push(' ');
            line.push_str(&num(c, p.currency));
        }
        if let Some(f) = p.peak_factor {
            line.push_str(&format!(" x{}", trim_num(f)));
        }
        line
    }));
    lines.join("\n")
}

fn trim_num(v: f64) -> String {
    let n = format!("{v:.4}");
    n.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// 设置页文本框 → 单价表。错了的行报第几行、为什么。
pub fn parse_text(text: &str) -> Result<Table, String> {
    let mut table = Table::default();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let bad = |why: &str| format!("单价表第 {} 行{why}：{line}", i + 1);
        let mut words = line.split_whitespace();
        let first = words.next().unwrap_or_default();
        // 峰时段行
        if let Some(spec) = first.strip_prefix("@peak") {
            if !spec.is_empty() {
                return Err(bad("「@peak」后面要空格"));
            }
            let mut rest: Vec<&str> = words.collect();
            let tz = rest.last().and_then(|s| parse_tz(s));
            if tz.is_some() {
                rest.pop();
            }
            let Some((days, ranges)) = rest.split_first() else {
                return Err(bad("要写星期和时段"));
            };
            if ranges.is_empty() {
                return Err(bad("要写时段，如 09:00-12:00"));
            }
            table.peaks.push(PeakWindow {
                days: parse_days(days).ok_or_else(|| bad("星期写成 1-5、3 或 *"))?,
                ranges: ranges
                    .iter()
                    .map(|r| parse_range(r).ok_or_else(|| bad("时段写成 09:00-12:00")))
                    .collect::<Result<Vec<_>, _>>()?,
                utc_offset: tz,
            });
            continue;
        }
        // 单价行
        let model = first.to_string();
        let mut currency = None;
        let mut nums = Vec::new();
        let mut peak_factor = None;
        for w in words {
            if peak_factor.is_none()
                && let Some(f) =
                    w.strip_prefix('x').or_else(|| w.strip_prefix('×')).filter(|f| !f.is_empty())
            {
                let f: f64 = f.parse().map_err(|_| bad("倍率写成 x2 这样"))?;
                if !f.is_finite() || f < 1.0 {
                    return Err(bad("倍率要不小于 1"));
                }
                peak_factor = Some(f);
                continue;
            }
            // 不带符号的数跟着同一行里写了符号的那个币种
            let (c, n) = if let Some(n) = w.strip_prefix('$') {
                (Some(Currency::Usd), n)
            } else if let Some(n) = w.strip_prefix('¥').or_else(|| w.strip_prefix('￥')) {
                (Some(Currency::Cny), n)
            } else {
                (None, w)
            };
            if let Some(c) = c {
                if currency.is_some_and(|x| x != c) {
                    return Err(bad("混用了 $ 和 ¥"));
                }
                currency = Some(c);
            }
            let v: f64 = n.parse().map_err(|_| bad("有不是数字的单价"))?;
            if !v.is_finite() || v < 0.0 {
                return Err(bad("有负数单价"));
            }
            nums.push(v);
        }
        if !(2..=3).contains(&nums.len()) {
            return Err(bad("要写 输入、输出 两个单价（缓存可选）"));
        }
        table.prices.push(Price {
            model,
            currency: currency.unwrap_or_default(),
            input: nums[0],
            output: nums[1],
            cache_read: nums.get(2).copied(),
            peak_factor,
        });
    }
    Ok(table)
}

/// `1-5` / `3` / `1,3,5` / `*`
fn parse_days(s: &str) -> Option<Vec<u8>> {
    if s == "*" {
        return Some((1..=7).collect());
    }
    let mut days = Vec::new();
    for part in s.split(',') {
        match part.split_once('-') {
            Some((a, b)) => {
                let a: u8 = a.parse().ok()?;
                let b: u8 = b.parse().ok()?;
                if !(1..=7).contains(&a) || !(1..=7).contains(&b) || a > b {
                    return None;
                }
                days.extend(a..=b);
            }
            None => {
                let d: u8 = part.parse().ok()?;
                if !(1..=7).contains(&d) {
                    return None;
                }
                days.push(d);
            }
        }
    }
    days.sort_unstable();
    days.dedup();
    (!days.is_empty()).then_some(days)
}

/// `09:00-12:00` → (540, 720)
fn parse_range(s: &str) -> Option<(u16, u16)> {
    let (a, b) = s.split_once('-')?;
    let (a, b) = (parse_hm(a)?, parse_hm(b)?);
    (a < b).then_some((a, b))
}

fn parse_hm(s: &str) -> Option<u16> {
    let (h, m) = s.split_once(':')?;
    let (h, m): (u16, u16) = (h.parse().ok()?, m.parse().ok()?);
    ((0..24).contains(&h) && m < 60).then_some(h * 60 + m)
}

/// `+08:00` → 480（分钟）
fn parse_tz(s: &str) -> Option<i32> {
    let (sign, rest) = match s.strip_prefix('+') {
        Some(rest) => (1, rest),
        None => (-1, s.strip_prefix('-')?),
    };
    let (h, m) = rest.split_once(':')?;
    let (h, m): (i32, i32) = (h.parse().ok()?, m.parse().ok()?);
    ((0..=14).contains(&h) && m < 60).then_some(sign * (h * 60 + m))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log(model: &str, input: u64, output: u64, cache_read: u64) -> RequestLog {
        RequestLog { model: Some(model.into()), input, output, cache_read, ..RequestLog::default() }
    }

    #[test]
    fn finds_the_longest_matching_entry() {
        let prices = default_prices();
        assert_eq!(find(&prices, "claude-opus-4-5-20251101").unwrap().input, 5.0);
        assert_eq!(find(&prices, "claude-opus-5-5").unwrap().input, 4.0);
        // 点和大小写不影响；带服务商前缀也认
        assert_eq!(find(&prices, "anthropic/Claude-Sonnet-4.5").unwrap().output, 15.0);
        assert_eq!(find(&prices, "gpt-4.1-mini").unwrap().input, 0.4);
        assert_eq!(find(&prices, "gpt-5-4-mini").unwrap().input, 0.75);
        // DeepSeek 的老名字按 Flash 计价，v4-pro 不会被 v4 吞掉
        assert_eq!(find(&prices, "deepseek-v4.1-flash").unwrap().input, 1.0);
        assert_eq!(find(&prices, "deepseek-v4-pro").unwrap().input, 4.5);
        assert!(find(&prices, "qwen-max").is_none());
    }

    #[test]
    fn costs_split_cache_and_currencies() {
        let prices = default_prices();
        let peaks = default_peaks();
        // 100 万输入里 80 万命中缓存：0.2 × 3 + 0.8 × 0.3 + 0.1 × 15 = 2.34
        let r = log("claude-sonnet-4-5", 1_000_000, 100_000, 800_000);
        let cost = Cost::of(
            &prices,
            &peaks,
            [&r, &log("kimi-k2-6", 1_000_000, 0, 0), &log("qwen", 5, 5, 0)],
        );
        assert!((cost.usd - 3.29).abs() < 1e-9);
        assert!((cost.saved_usd - 0.8 * 2.7).abs() < 1e-9);
        assert_eq!(cost.unpriced, vec!["qwen".to_string()]);
        assert_eq!((cost.peak, cost.cny), (0, 0.0));
        assert_eq!(cost.label().unwrap(), "$3.29");
        assert_eq!(money(0.00123, Currency::Usd), "$0.0012");
        assert_eq!(Cost::default().label(), None);
    }

    /// 2026-10-19 是周一：北京时间 10:00 峰价、20:00 谷价、周末全天谷价。
    #[test]
    fn peak_hours_multiply_by_the_factor() {
        let prices = default_prices();
        let peaks = default_peaks();
        let at = |s: &str| {
            chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M")
                .unwrap()
                .and_local_timezone(FixedOffset::east_opt(8 * 3600).unwrap())
                .unwrap()
                .timestamp_millis() as u64
        };
        let p = find(&prices, "deepseek-flash").unwrap();
        let mut peak = log("deepseek-flash", 1_000_000, 0, 0);
        peak.at_ms = at("2026-10-19 10:00");
        let mut off = peak.clone();
        off.at_ms = at("2026-10-19 20:00");
        let mut sunday = peak.clone();
        sunday.at_ms = at("2026-10-25 10:00");
        assert_eq!(request_cost(p, &peak, &peaks), 2.0);
        assert_eq!(request_cost(p, &off, &peaks), 1.0);
        assert_eq!(request_cost(p, &sunday, &peaks), 1.0);
        assert_eq!(peak_of(p, &peaks, &peak), Some(true));
        assert_eq!(peak_of(p, &peaks, &off), Some(false));
        // 不分峰谷的模型：没有峰谷标记，倍率 1
        let other = find(&prices, "claude-sonnet-5").unwrap();
        assert_eq!(peak_of(other, &peaks, &peak), None);
        let mixed = vec![peak.clone(), off.clone()];
        let cost = Cost::of(&prices, &peaks, &mixed);
        assert_eq!(cost.peak, 1);
        assert!((cost.cny - 3.0).abs() < 1e-9);
        // 时段改了也立刻生效
        peak.at_ms = at("2026-10-19 13:00");
        let evening = vec![PeakWindow {
            days: vec![1],
            ranges: vec![(12 * 60 + 30, 13 * 60 + 30)],
            utc_offset: Some(480),
        }];
        assert_eq!(request_cost(p, &peak, &evening), 2.0);
        assert_eq!(request_cost(p, &off, &evening), 1.0);
    }

    #[test]
    fn text_round_trips_and_reports_bad_lines() {
        let table = Table::defaults();
        assert_eq!(parse_text(&to_text(&table)).unwrap(), table);
        assert!(to_text(&table).contains("gpt-5 $1.25 $10 $0.125"));
        assert!(to_text(&table).contains("deepseek-flash ¥1 ¥4 ¥0.02 x2"));
        assert!(to_text(&table).contains("@peak 1-5 09:00-12:00 14:00-18:00 +08:00"));
        let t = parse_text("# 注释\n\nmy-model 1 2\nqwen ¥2.4 ¥9.6 ¥0.48 x3").unwrap();
        assert_eq!(t.prices[0].currency, Currency::Usd);
        assert_eq!(t.prices[0].cache_read, None);
        assert_eq!(t.prices[0].peak_factor, None);
        assert_eq!((t.prices[1].currency, t.prices[1].cache_read), (Currency::Cny, Some(0.48)));
        assert_eq!(t.prices[1].peak_factor, Some(3.0));
        // 峰时段：多行合并、本地时区不写就是 None
        let t = parse_text("@peak * 22:00-23:30\n@peak 6-7 00:00-01:00 -05:00").unwrap();
        assert_eq!(t.peaks.len(), 2);
        assert_eq!(t.peaks[0].days, (1..=7).collect::<Vec<u8>>());
        assert_eq!(t.peaks[0].ranges, vec![(22 * 60, 23 * 60 + 30)]);
        assert_eq!(t.peaks[0].utc_offset, None);
        assert_eq!(t.peaks[1].utc_offset, Some(-300));
        assert!(parse_text("m 1").unwrap_err().contains("第 1 行"));
        assert!(parse_text("a 1 2\nm $1 ¥2").unwrap_err().contains("第 2 行"));
        assert!(parse_text("m x 2").unwrap_err().contains("不是数字"));
        assert!(parse_text("m 1 2 x0.5").unwrap_err().contains("倍率"));
        assert!(parse_text("@peak 1-5").unwrap_err().contains("要写时段"));
        assert!(parse_text("@peak 8 09:00-10:00").unwrap_err().contains("星期"));
        assert!(parse_text("@peak 1 12:00-09:00").unwrap_err().contains("时段"));
        assert!(parse_text("@peak 1 09:00-10:00 +9").unwrap_err().contains("时段"));
    }
}

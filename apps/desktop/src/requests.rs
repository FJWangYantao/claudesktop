//! 首页「请求」页签的数据：把用量网关的日志（[`RequestLog`]，每个请求一行）按「会话的第几轮」归组，
//! 再汇总成几个总数。日志里的会话是内核会话 ID，靠 `SessionMeta::kernel_sessions` 对回侧栏里的会话。
//!
//! 一轮的成本口径分两个数：**累计输入**（这一轮各次请求的输入相加，是真正计费的量）和
//! **上下文**（最后一次对话请求的输入，即此刻上下文多大）。工具循环一轮发好几次请求，每次都带着整个上下文，
//! 所以累计输入通常是上下文的好几倍。

use crate::pricing::{self, Cost, PeakWindow, Price};
use claudesktop_builtin::gateway::RequestLog;
use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap};
use std::ops::Range;
use std::sync::Arc;

/// 日志对回到的本地会话。
#[derive(Clone, Debug, PartialEq)]
pub struct SessionRef {
    pub title: String,
    /// `Session::token`（点了切过去）
    pub token: u64,
}

/// (主会话内核 ID, 第几轮)，未打标记的请求用 (用途#请求 ID, 时刻)。
pub type TurnKey = (Arc<str>, u64);

/// 一轮里的全部请求。
#[derive(Debug, PartialEq)]
pub struct TurnRow {
    pub key: TurnKey,
    pub at_ms: u64,
    model_request: usize,
    indices: RequestIndices,
    pub input: u64,
    pub context: u64,
    pub output: u64,
    pub cache_read: u64,
    /// 第一个请求发出到最后一个结束
    pub elapsed_ms: u64,
    /// 重试了几次（第 2 次及以后的尝试）
    pub retries: usize,
    /// 子任务发出的请求数
    pub subagents: usize,
    /// 失败的请求数（被用户停掉的不算）
    pub failed: usize,
    pub cancelled: bool,
}

#[derive(Debug, PartialEq)]
enum RequestIndices {
    Consecutive(Range<usize>),
    Interleaved(Vec<usize>),
}

impl RequestIndices {
    fn iter(&self) -> impl DoubleEndedIterator<Item = usize> + '_ {
        let (range, sparse) = match self {
            Self::Consecutive(range) => (range.clone(), &[][..]),
            Self::Interleaved(indices) => (0..0, indices.as_slice()),
        };
        range.chain(sparse.iter().copied())
    }

    fn insert(&mut self, index: usize, logs: &[RequestLog]) -> bool {
        match self {
            Self::Consecutive(range)
                if range.end == index && logs[index].at_ms >= logs[index - 1].at_ms =>
            {
                range.end += 1;
                return true;
            }
            Self::Consecutive(range) => *self = Self::Interleaved(range.clone().collect()),
            Self::Interleaved(_) => {}
        }
        let Self::Interleaved(indices) = self else { unreachable!() };
        let time = logs[index].at_ms;
        if indices.last().is_some_and(|&i| logs[i].at_ms > time) {
            let at = indices.partition_point(|&i| logs[i].at_ms <= time);
            indices.insert(at, index);
            false
        } else {
            indices.push(index);
            true
        }
    }
}

impl TurnRow {
    fn new(key: TurnKey, index: usize, log: &RequestLog) -> Self {
        let mut row = Self {
            key,
            at_ms: log.at_ms,
            model_request: index,
            indices: RequestIndices::Consecutive(index..index + 1),
            input: 0,
            context: 0,
            output: 0,
            cache_read: 0,
            elapsed_ms: 0,
            retries: 0,
            subagents: 0,
            failed: 0,
            cancelled: false,
        };
        row.add(log);
        if conversation(log) {
            row.context = log.input;
        }
        row
    }

    fn add(&mut self, log: &RequestLog) {
        self.input += log.input;
        self.output += log.output;
        self.cache_read += log.cache_read;
        self.retries += usize::from(log.attempt.is_some_and(|a| a > 1));
        self.subagents += usize::from(log.purpose.as_ref() == "subagent");
        self.failed += usize::from(failed(log));
        self.cancelled |= log.cancelled;
        let end = log.at_ms.saturating_add(log.total_ms);
        self.elapsed_ms = self.elapsed_ms.max(end - self.at_ms);
    }

    fn insert(&mut self, index: usize, logs: &[RequestLog]) {
        let log = &logs[index];
        if log.at_ms < self.at_ms {
            self.elapsed_ms += self.at_ms - log.at_ms;
            self.at_ms = log.at_ms;
        }
        self.add(log);
        let appended = self.indices.insert(index, logs);
        let chosen = &logs[self.model_request];
        if (conversation(log), log.at_ms) >= (conversation(chosen), chosen.at_ms) {
            self.model_request = index;
        }
        if conversation(log) && log.input > 0 {
            self.context = if appended {
                log.input
            } else {
                self.requests(logs).rev().find(|r| conversation(r) && r.input > 0).unwrap().input
            };
        }
    }

    fn first(&self) -> usize {
        self.indices.iter().next().unwrap()
    }

    pub fn session<'a>(&self, names: &'a HashMap<String, SessionRef>) -> Option<&'a SessionRef> {
        names.get(self.key.0.as_ref())
    }

    pub fn purpose<'a>(&self, logs: &'a [RequestLog]) -> &'a str {
        &logs[self.first()].purpose
    }

    pub fn model<'a>(&self, logs: &'a [RequestLog]) -> Option<&'a str> {
        logs[self.model_request].model.as_deref()
    }

    pub fn request_count(&self) -> usize {
        match &self.indices {
            RequestIndices::Consecutive(range) => range.len(),
            RequestIndices::Interleaved(indices) => indices.len(),
        }
    }

    pub fn requests<'a>(
        &'a self,
        logs: &'a [RequestLog],
    ) -> impl DoubleEndedIterator<Item = &'a RequestLog> {
        self.indices.iter().map(|i| &logs[i])
    }
}

/// 筛选后所有轮的总数。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Summary {
    pub turns: usize,
    pub requests: usize,
    pub input: u64,
    pub output: u64,
    pub retries: usize,
    pub failed: usize,
    /// 成功的对话请求首个内容块的中位数
    pub first_token_ms: Option<u64>,
}

/// 请求算失败：没连上，或者上游回了非 2xx（停掉的另算）。
pub fn failed(r: &RequestLog) -> bool {
    !r.cancelled && (r.status == 0 || !(200..300).contains(&r.status))
}

#[derive(Default)]
pub struct Groups {
    pub rows: Vec<TurnRow>,
    first_turn: HashMap<Arc<str>, (u64, usize)>,
    other_turns: HashMap<TurnKey, usize>,
    order: BTreeMap<(Reverse<u64>, usize), usize>,
}

impl Groups {
    pub fn extend(&mut self, logs: &[RequestLog], start: usize) {
        for (index, log) in logs.iter().enumerate().skip(start) {
            let fallback;
            let (session, turn) = match (&log.session, log.turn) {
                (Some(session), Some(turn)) => {
                    (session.split_once('/').map_or(session.as_ref(), |(root, _)| root), turn)
                }
                _ => {
                    fallback = format!("{}#{}", log.purpose, log.req_id.unwrap_or(0));
                    (fallback.as_str(), log.at_ms)
                }
            };
            let next = self.rows.len();
            let (key, row_index) = match self.first_turn.get_key_value(session) {
                Some((key, &(first, row))) if first == turn => (key.clone(), row),
                Some((key, _)) => {
                    let row = *self.other_turns.entry((key.clone(), turn)).or_insert(next);
                    (key.clone(), row)
                }
                None => {
                    let key = log
                        .session
                        .as_ref()
                        .filter(|s| s.as_ref() == session)
                        .cloned()
                        .unwrap_or_else(|| session.into());
                    self.first_turn.insert(key.clone(), (turn, next));
                    (key, next)
                }
            };
            if let Some(row) = self.rows.get_mut(row_index) {
                let old_time = row.at_ms;
                let old_first = row.first();
                row.insert(index, logs);
                if row.at_ms != old_time {
                    self.order.remove(&(Reverse(old_time), old_first));
                    self.order.insert((Reverse(row.at_ms), row.first()), row_index);
                }
            } else {
                self.rows.push(TurnRow::new((key, turn), index, log));
                self.order.insert((Reverse(log.at_ms), index), row_index);
            }
        }
    }

    pub fn newest_first(&self) -> impl Iterator<Item = (usize, &TurnRow)> {
        self.order.values().map(|&i| (i, &self.rows[i]))
    }
}

/// 汇总。
pub fn summarize(rows: &[&TurnRow], logs: &[RequestLog]) -> Summary {
    let mut firsts: Vec<u64> = rows
        .iter()
        .flat_map(|row| row.requests(logs))
        .filter(|q| conversation(q) && !failed(q))
        .filter_map(|q| q.first_token_ms)
        .collect();
    let first_token_ms = (!firsts.is_empty()).then(|| {
        let middle = firsts.len() / 2;
        *firsts.select_nth_unstable(middle).1
    });
    Summary {
        turns: rows.len(),
        requests: rows.iter().map(|r| r.request_count()).sum(),
        input: rows.iter().map(|r| r.input).sum(),
        output: rows.iter().map(|r| r.output).sum(),
        retries: rows.iter().map(|r| r.retries).sum(),
        failed: rows.iter().map(|r| r.failed).sum(),
        first_token_ms,
    }
}

pub struct Costs {
    pub total: Cost,
    pub models: HashMap<String, Cost>,
    pub sessions: HashMap<u64, Cost>,
}

pub fn costs(
    rows: &[&TurnRow],
    logs: &[RequestLog],
    names: &HashMap<String, SessionRef>,
    prices: &[Price],
    peaks: &[PeakWindow],
) -> Costs {
    let mut total = Cost::default();
    let mut models = HashMap::new();
    let mut sessions: HashMap<u64, Cost> = HashMap::new();
    for row in rows {
        let mut session_cost = row.session(names).map(|s| sessions.entry(s.token).or_default());
        for log in row.requests(logs) {
            let Some(model) = log.model.as_deref() else { continue };
            let (price, cost) = models
                .entry(model)
                .or_insert_with(|| (pricing::find(prices, model), Cost::default()));
            if log.input == 0 && log.output == 0 {
                continue;
            }
            let charge = price.map(|p| pricing::charge(p, log, peaks));
            total.add(model, charge);
            cost.add(model, charge);
            if let Some(cost) = session_cost.as_deref_mut() {
                cost.add(model, charge);
            }
        }
    }
    let models = models.into_iter().map(|(model, (_, cost))| (model.to_owned(), cost)).collect();
    Costs { total, models, sessions }
}

/// 主对话的请求（自研内核的 turn、外部 agent 的请求），不含子任务、压缩、列模型。
fn conversation(r: &RequestLog) -> bool {
    matches!(r.purpose.as_ref(), "turn" | "acp")
}

/// 用途的叫法。
pub fn purpose_label(purpose: &str) -> &'static str {
    match purpose {
        "turn" => "对话",
        "acp" => "ACP agent",
        "subagent" => "子任务",
        "compact" => "压缩上下文",
        "auto" => "自动模式判定",
        "suggest" => "输入补全",
        "models" => "列模型",
        _ => "其他",
    }
}

/// 毫秒数：`820 毫秒` / `4.1 秒` / `2 分 05 秒`。
pub fn ms_label(ms: u64) -> String {
    match ms {
        0..1_000 => format!("{ms} 毫秒"),
        1_000..60_000 => format!("{:.1} 秒", ms as f64 / 1000.0),
        _ => format!("{} 分 {:02} 秒", ms / 60_000, ms / 1000 % 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(session: &str, turn: u64, purpose: &str, at_ms: u64, input: u64) -> RequestLog {
        RequestLog {
            at_ms,
            session: Some(session.into()),
            turn: Some(turn),
            purpose: purpose.into(),
            model: Some("m1".into()),
            status: 200,
            attempt: Some(1),
            total_ms: 100,
            input,
            output: 10,
            first_token_ms: Some(at_ms % 1000),
            ..RequestLog::default()
        }
    }

    #[test]
    fn groups_requests_into_turns() {
        let mut retry = req("k1", 1, "turn", 1_100, 0);
        retry.status = 429;
        retry.input = 0;
        let mut again = req("k1", 1, "turn", 1_200, 500);
        again.attempt = Some(2);
        let logs = vec![
            req("k1", 1, "turn", 1_000, 400),
            retry,
            again,
            // 子任务的请求归到主会话的同一轮
            req("k1/call_1", 1, "subagent", 1_300, 50),
            req("k1", 1, "turn", 1_500, 700),
            req("k1", 2, "turn", 5_000, 900),
            RequestLog {
                at_ms: 6_000,
                purpose: "models".into(),
                status: 200,
                ..RequestLog::default()
            },
        ];
        let names =
            HashMap::from([("k1".to_string(), SessionRef { title: "修 bug".into(), token: 7 })]);
        let mut groups = Groups::default();
        groups.extend(&logs, 0);
        let rows: Vec<_> = groups.newest_first().map(|(_, row)| row).collect();
        assert_eq!(rows.len(), 3);
        // 最新的在前
        assert_eq!(rows[0].purpose(&logs), "models");
        assert_eq!(rows[0].session(&names), None);
        let first = rows[2];
        assert_eq!(first.key, ("k1".into(), 1));
        assert_eq!(first.session(&names).unwrap().title, "修 bug");
        assert_eq!(first.indices, RequestIndices::Consecutive(0..5));
        assert!(first.requests(&logs).zip(&logs).all(|(a, b)| std::ptr::eq(a, b)));
        // 累计输入是各次相加，上下文是最后一次对话请求的
        assert_eq!((first.input, first.context), (1_650, 700));
        assert_eq!((first.retries, first.failed, first.subagents), (1, 1, 1));
        assert_eq!(first.elapsed_ms, 600);

        let s = summarize(&rows, &logs);
        assert_eq!((s.turns, s.requests, s.failed, s.retries), (3, 7, 1, 1));
        assert_eq!(s.input, 1_650 + 900);
        // 成功的对话请求的首字中位数（这里各请求的首字毫秒取 at_ms 的零头）
        assert_eq!(s.first_token_ms, Some(200));
    }

    #[test]
    fn fees_keep_request_order_and_model_and_session_totals() {
        let mut logs = vec![
            req("k1", 1, "turn", 1000, 1_000_000),
            req("k1/child", 1, "subagent", 1001, 1_000_000),
            req("k1", 1, "turn", 1002, 1_000_000),
            req("k2", 1, "turn", 2000, 1_000_000),
            req("k3", 1, "turn", 3000, 0),
            req("k4", 1, "turn", 4000, 1_000_000),
        ];
        logs[0].model = Some("provider/DeepSeek-V4.Pro".into());
        logs[0].cache_read = 2_000_000;
        logs[1].model = Some("unpriced-2".into());
        logs[2].model = logs[0].model.clone();
        logs[3].model = Some("unpriced-1".into());
        logs[4].model = Some("no-usage".into());
        logs[4].output = 0;
        logs[5].model = None;
        let names = ["k1", "k2", "k3", "k4"]
            .into_iter()
            .enumerate()
            .map(|(i, name)| (name.into(), SessionRef { title: name.into(), token: i as u64 }))
            .collect();
        let mut groups = Groups::default();
        groups.extend(&logs, 0);
        let rows: Vec<_> = groups.newest_first().map(|(_, row)| row).collect();
        let prices = pricing::default_prices();
        let peaks = pricing::default_peaks();
        let totals = costs(&rows, &logs, &names, &prices, &peaks);
        let all = rows.iter().flat_map(|row| row.requests(&logs));
        assert_eq!(totals.total, Cost::of(&prices, &peaks, all));
        assert_eq!(totals.total.unpriced, ["unpriced-1", "unpriced-2"]);
        assert_eq!(totals.models["no-usage"], Cost::default());
        assert_eq!(totals.sessions[&3], Cost::default());
        for (model, cost) in totals.models {
            let matching = rows
                .iter()
                .flat_map(|row| row.requests(&logs))
                .filter(|log| log.model.as_deref() == Some(&model));
            assert_eq!(cost, Cost::of(&prices, &peaks, matching));
        }
        for row in &rows {
            let token = row.session(&names).unwrap().token;
            assert_eq!(totals.sessions[&token], Cost::of(&prices, &peaks, row.requests(&logs)));
        }
    }

    #[test]
    fn late_requests_update_only_their_turn_and_preserve_time_order() {
        let mut logs = vec![
            req("k2", 1, "turn", 1_000, 100),
            req("k1", 1, "turn", 1_100, 200),
            req("k3", 1, "turn", 1_200, 300),
        ];
        let mut groups = Groups::default();
        groups.extend(&logs, 0);
        let old_k3 = groups.rows[2].first();
        let start = logs.len();
        let mut late = req("k1/child", 1, "subagent", 1_000, 50);
        late.total_ms = 900;
        logs.extend([
            late,
            req("k1", 1, "turn", 1_050, 150),
            req("k1", 1, "turn", 1_100, 0),
            req("k1/child", 1, "subagent", 1_300, 25),
            req("k4", 1, "turn", 1_400, 400),
        ]);
        logs[5].model = None;
        groups.extend(&logs, start);
        let actual: Vec<_> = groups.newest_first().map(|(_, row)| row).collect();
        assert_eq!(
            actual.iter().map(|row| row.key.0.as_ref()).collect::<Vec<_>>(),
            ["k4", "k3", "k2", "k1"]
        );
        assert_eq!(groups.rows[2].first(), old_k3);
        let row = actual[3];
        assert_eq!(row.indices.iter().collect::<Vec<_>>(), [3, 4, 1, 5, 6]);
        assert_eq!(row.purpose(&logs), "subagent");
        assert_eq!(row.model(&logs), None);
        assert_eq!((row.input, row.context, row.elapsed_ms, row.subagents), (425, 200, 900, 2));

        let mut sorted = logs.clone();
        sorted.sort_by_key(|r| r.at_ms);
        let mut full = Groups::default();
        full.extend(&sorted, 0);
        let expected: Vec<_> = full.newest_first().map(|(_, row)| row).collect();
        for (a, b) in actual.iter().zip(&expected) {
            assert_eq!(a.key, b.key);
            assert!(a.requests(&logs).eq(b.requests(&sorted)));
        }
        assert_eq!(summarize(&actual, &logs), summarize(&expected, &sorted));
        let prices = pricing::default_prices();
        let peaks = pricing::default_peaks();
        let names = HashMap::from([("k1".into(), SessionRef { title: "k1".into(), token: 1 })]);
        let a = costs(&actual, &logs, &names, &prices, &peaks);
        let b = costs(&expected, &sorted, &names, &prices, &peaks);
        assert_eq!((a.total, a.models, a.sessions), (b.total, b.models, b.sessions));
    }

    #[test]
    fn labels() {
        assert_eq!(ms_label(820), "820 毫秒");
        assert_eq!(ms_label(4_120), "4.1 秒");
        assert_eq!(ms_label(125_000), "2 分 05 秒");
        assert_eq!(purpose_label("subagent"), "子任务");
    }
}

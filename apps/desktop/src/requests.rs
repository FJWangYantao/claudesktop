//! 首页「请求」页签的数据：把用量网关的日志（[`RequestLog`]，每个请求一行）按「会话的第几轮」归组，
//! 再汇总成几个总数。日志里的会话是内核会话 ID，靠 `SessionMeta::kernel_sessions` 对回侧栏里的会话。
//!
//! 一轮的成本口径分两个数：**累计输入**（这一轮各次请求的输入相加，是真正计费的量）和
//! **上下文**（最后一次对话请求的输入，即此刻上下文多大）。工具循环一轮发好几次请求，每次都带着整个上下文，
//! 所以累计输入通常是上下文的好几倍。

use claudesktop_builtin::gateway::RequestLog;
use std::collections::HashMap;

/// 日志对回到的本地会话。
#[derive(Clone, Debug, PartialEq)]
pub struct SessionRef {
    pub title: String,
    /// `Session::token`（点了切过去）
    pub token: u64,
}

/// 一轮里的全部请求。
#[derive(Clone, Debug, PartialEq)]
pub struct TurnRow {
    /// (主会话的内核会话 ID, 第几轮)；没打标记的请求（列模型）各自一行，键是 (用途, 时刻)
    pub key: (String, u64),
    pub session: Option<SessionRef>,
    /// 用途：turn / subagent / compact / models / acp / other（取这一轮第一个请求的；有子任务时看 `subagents`）
    pub purpose: String,
    pub at_ms: u64,
    pub model: Option<String>,
    /// 时间顺序
    pub requests: Vec<RequestLog>,
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

/// 按（会话, 轮）归组，最新的在前。`names`：内核会话 ID → 本地会话。
pub fn group(logs: &[RequestLog], names: &HashMap<String, SessionRef>) -> Vec<TurnRow> {
    let mut rows: Vec<TurnRow> = Vec::new();
    let mut index: HashMap<(String, u64), usize> = HashMap::new();
    for log in logs {
        // 子任务的会话是 `主会话/调用 ID`，归到主会话的那一轮
        let key = match (&log.session, log.turn) {
            (Some(session), Some(turn)) => {
                (session.split('/').next().unwrap_or(session).to_string(), turn)
            }
            _ => (format!("{}#{}", log.purpose, log.req_id.unwrap_or(0)), log.at_ms),
        };
        let i = *index.entry(key.clone()).or_insert_with(|| {
            rows.push(TurnRow {
                session: names.get(&key.0).cloned(),
                key,
                purpose: log.purpose.clone(),
                at_ms: log.at_ms,
                model: None,
                requests: Vec::new(),
                input: 0,
                context: 0,
                output: 0,
                cache_read: 0,
                elapsed_ms: 0,
                retries: 0,
                subagents: 0,
                failed: 0,
                cancelled: false,
            });
            rows.len() - 1
        });
        rows[i].requests.push(log.clone());
    }
    for row in &mut rows {
        let r = &row.requests;
        row.input = r.iter().map(|q| q.input).sum();
        row.output = r.iter().map(|q| q.output).sum();
        row.cache_read = r.iter().map(|q| q.cache_read).sum();
        row.retries = r.iter().filter(|q| q.attempt.is_some_and(|a| a > 1)).count();
        row.subagents = r.iter().filter(|q| q.purpose == "subagent").count();
        row.failed = r.iter().filter(|q| failed(q)).count();
        row.cancelled = r.iter().any(|q| q.cancelled);
        row.context =
            r.iter().rev().find(|q| conversation(q) && q.input > 0).map_or(0, |q| q.input);
        row.model =
            r.iter().rev().find(|q| conversation(q)).or(r.last()).and_then(|q| q.model.clone());
        let end = r.iter().map(|q| q.at_ms + q.total_ms).max().unwrap_or(row.at_ms);
        row.elapsed_ms = end.saturating_sub(row.at_ms);
    }
    rows.sort_by_key(|r| std::cmp::Reverse(r.at_ms));
    rows
}

/// 汇总。
pub fn summarize(rows: &[&TurnRow]) -> Summary {
    let mut firsts: Vec<u64> = rows
        .iter()
        .flat_map(|row| &row.requests)
        .filter(|q| conversation(q) && !failed(q))
        .filter_map(|q| q.first_token_ms)
        .collect();
    firsts.sort_unstable();
    Summary {
        turns: rows.len(),
        requests: rows.iter().map(|r| r.requests.len()).sum(),
        input: rows.iter().map(|r| r.input).sum(),
        output: rows.iter().map(|r| r.output).sum(),
        retries: rows.iter().map(|r| r.retries).sum(),
        failed: rows.iter().map(|r| r.failed).sum(),
        first_token_ms: firsts.get(firsts.len() / 2).copied(),
    }
}

/// 主对话的请求（自研内核的 turn、外部 agent 的请求），不含子任务、压缩、列模型。
fn conversation(r: &RequestLog) -> bool {
    matches!(r.purpose.as_str(), "turn" | "acp")
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
        let rows = group(&logs, &names);
        assert_eq!(rows.len(), 3);
        // 最新的在前
        assert_eq!(rows[0].purpose, "models");
        assert_eq!(rows[0].session, None);
        let first = &rows[2];
        assert_eq!(first.key, ("k1".into(), 1));
        assert_eq!(first.session.as_ref().unwrap().title, "修 bug");
        assert_eq!(first.requests.len(), 5);
        // 累计输入是各次相加，上下文是最后一次对话请求的
        assert_eq!((first.input, first.context), (1_650, 700));
        assert_eq!((first.retries, first.failed, first.subagents), (1, 1, 1));
        assert_eq!(first.elapsed_ms, 600);

        let all: Vec<&TurnRow> = rows.iter().collect();
        let s = summarize(&all);
        assert_eq!((s.turns, s.requests, s.failed, s.retries), (3, 7, 1, 1));
        assert_eq!(s.input, 1_650 + 900);
        // 成功的对话请求的首字中位数（这里各请求的首字毫秒取 at_ms 的零头）
        assert_eq!(s.first_token_ms, Some(200));
    }

    #[test]
    fn labels() {
        assert_eq!(ms_label(820), "820 毫秒");
        assert_eq!(ms_label(4_120), "4.1 秒");
        assert_eq!(ms_label(125_000), "2 分 05 秒");
        assert_eq!(purpose_label("subagent"), "子任务");
    }
}

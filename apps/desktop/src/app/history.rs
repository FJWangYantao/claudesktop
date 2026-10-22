//! 输入历史：发出去的消息按条记下来，输入框里用上下键回溯。
//!
//! 行为（用户定的设计，见「设计说明-输入框方向键.md」第七节）：
//!
//! - 光标在文字中间（不在行首 / 行尾）时，↑ 把光标移到**行首**，↓ 移到**行尾**——只在
//!   多行输入框的第一行按 ↑、最后一行按 ↓ 才这么走，其余行由输入框自己上下移光标
//!   （多行的行内移动不能丢）。
//! - 光标已经**在第一行开头**（`cursor == 0`）时再按 ↑，换成上一条历史，光标置于开头；
//!   已经是最早一条就什么都不做（`cx.propagate()` 放行）。
//! - 光标已经**在末尾**（`cursor == len`）时再按 ↓，换成下一条历史，光标置于末尾；
//!   已经是最新一条就什么都不做。
//! - 开始回溯前输入框里的草稿先存进 [`InputHistory::draft`]，按 ↓ 回到最新一条之后拿回来。
//!
//! 记的是「发出去的消息」：`send()` 走到底（含排队、worktree 建设期排队）才入表，
//! 发了没发成的不入（见 `ChatApp::remember_input`）。相邻重复只留一条。

/// 最多记多少条（再多就挤掉最早的）
const MAX_ENTRIES: usize = 200;

/// 输入历史表 + 当前浏览到第几条。
#[derive(Default)]
pub(super) struct InputHistory {
    /// 发出去的消息，按时间先后（最后一条是最近发的）
    entries: Vec<String>,
    /// 正在回溯时停在第几条（`0` 是最早那条、`len - 1` 是最近一条）；`None` = 没在回溯
    index: Option<usize>,
    /// 开始回溯前输入框里的草稿：退回到最新一条之后拿回输入框
    draft: String,
}

impl InputHistory {
    /// 发出去一条消息：入表并清掉回溯状态（发完输入框是空的，光标在开头）。
    /// 空白不入表；和上一条完全一样也不入（重发同一条不占两格）。
    pub(super) fn push(&mut self, text: &str) {
        self.index = None;
        self.draft.clear();
        if text.trim().is_empty() {
            return;
        }
        if self.entries.last().is_some_and(|last| last == text) {
            return;
        }
        self.entries.push(text.to_string());
        if self.entries.len() > MAX_ENTRIES {
            let drop = self.entries.len() - MAX_ENTRIES;
            self.entries.drain(..drop);
        }
    }

    /// 上一条。`draft` 传输入框现在的全文：第一次回溯时存起来。
    /// 没有上一条（还没发过消息，或已经在最早一条）时返回 `None`。
    pub(super) fn prev(&mut self, draft: &str) -> Option<&str> {
        if self.entries.is_empty() {
            return None;
        }
        let next = match self.index {
            Some(0) => return None,
            Some(i) => i - 1,
            None => {
                self.draft = draft.to_string();
                self.entries.len() - 1
            }
        };
        self.index = Some(next);
        self.entries.get(next).map(String::as_str)
    }

    /// 下一条。走到最近一条之后（`index` 回到 `None`）返回存下来的草稿。
    /// 没在回溯、或者有草稿但输入框已经被改过（`draft` 对不上）时返回 `None`。
    pub(super) fn next(&mut self, current: &str) -> Option<String> {
        let i = self.index?;
        let next = i + 1;
        if next >= self.entries.len() {
            // 回到最新一条之后：拿回草稿。草稿是空串（开始回溯时输入框是空的）也要返回，
            // 否则按 ↓ 就卡在最后一条历史上、回不到空输入框
            self.index = None;
            let draft = std::mem::take(&mut self.draft);
            if draft == current {
                return None;
            }
            return Some(draft);
        }
        self.index = Some(next);
        self.entries.get(next).cloned()
    }

    /// 输入内容变了（用户打字 / 粘贴）：退出回溯，别让历史把新写的字冲掉。
    pub(super) fn touch(&mut self) {
        self.index = None;
        self.draft.clear();
    }

    /// 正在回溯（输入框里的内容来自历史，不是用户写的）
    pub(super) fn browsing(&self) -> bool {
        self.index.is_some()
    }

    /// 记了多少条（设置页 / 调试用）
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[core::prelude::v1::test]
    fn prev_walks_back_to_the_earliest_and_stops() {
        let mut h = InputHistory::default();
        h.push("第一条");
        h.push("第二条");
        h.push("第三条");
        assert_eq!(h.prev("草稿"), Some("第三条"));
        assert_eq!(h.prev("草稿"), Some("第二条"));
        assert_eq!(h.prev("草稿"), Some("第一条"));
        // 到最早一条：无反应
        assert_eq!(h.prev("草稿"), None);
        assert_eq!(h.prev("草稿"), None);
    }

    #[core::prelude::v1::test]
    fn next_comes_back_and_restores_the_draft() {
        let mut h = InputHistory::default();
        h.push("甲");
        h.push("乙");
        assert_eq!(h.prev("写了一半"), Some("乙"));
        assert_eq!(h.prev("写了一半"), Some("甲"));
        assert_eq!(h.next("甲"), Some("乙".to_string()));
        // 回到最近一条之后：拿回草稿
        assert_eq!(h.next("乙"), Some("写了一半".to_string()));
        assert!(!h.browsing());
        // 再按 ↓ 已经不在回溯里
        assert_eq!(h.next("写了一半"), None);
    }

    #[core::prelude::v1::test]
    fn next_without_draft_just_stops() {
        let mut h = InputHistory::default();
        h.push("唯一一条");
        assert_eq!(h.prev(""), Some("唯一一条"));
        // 草稿是空的：也要退回到「没有东西」，不能卡在历史上
        assert_eq!(h.next("唯一一条"), Some(String::new()));
        assert!(!h.browsing());
    }

    #[core::prelude::v1::test]
    fn empty_history_has_nothing_to_walk() {
        let mut h = InputHistory::default();
        assert_eq!(h.prev("随便写点"), None);
        assert_eq!(h.next("随便写点"), None);
        assert_eq!(h.len(), 0);
    }

    #[core::prelude::v1::test]
    fn drafts_are_not_shared_between_two_browsing_runs() {
        let mut h = InputHistory::default();
        h.push("旧消息");
        assert_eq!(h.prev("第一份草稿"), Some("旧消息"));
        // 打字：退出回溯，草稿作废
        h.touch();
        assert!(!h.browsing());
        assert_eq!(h.prev("第二份草稿"), Some("旧消息"));
        assert_eq!(h.next("旧消息"), Some("第二份草稿".to_string()));
    }

    #[core::prelude::v1::test]
    fn blank_and_repeated_messages_are_skipped() {
        let mut h = InputHistory::default();
        h.push("   \n ");
        assert_eq!(h.len(), 0);
        h.push("同一句");
        h.push("同一句");
        assert_eq!(h.len(), 1);
        h.push("不同的一句");
        assert_eq!(h.len(), 2);
    }

    #[core::prelude::v1::test]
    fn push_after_browsing_starts_over_from_the_end() {
        let mut h = InputHistory::default();
        h.push("一");
        h.push("二");
        assert_eq!(h.prev("草稿"), Some("二"));
        h.push("三");
        assert!(!h.browsing());
        assert_eq!(h.prev("新的草稿"), Some("三"));
    }

    #[core::prelude::v1::test]
    fn entries_are_capped_from_the_front() {
        let mut h = InputHistory::default();
        for i in 0..(MAX_ENTRIES + 10) {
            h.push(&format!("第 {i} 条"));
        }
        assert_eq!(h.len(), MAX_ENTRIES);
        // 最早那 10 条被挤掉了，回溯到的最早一条是第 10 条
        let mut last = None;
        while let Some(text) = h.prev("") {
            last = Some(text.to_string());
        }
        assert_eq!(last.as_deref(), Some("第 10 条"));
    }
}

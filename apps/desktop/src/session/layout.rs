//! 对话记录的行划分与排版度量：哪条记录画成哪一行、怎么分组、未测行先估多高。

use super::*;
use std::collections::HashMap;

/// 对话记录里一行在当前视图下怎么画。连续的过程步骤（[`ChatItem::is_step`]）合成一组：
/// 组头画在组里第一行的顶上，组内各步缩进在组头下面，折叠时不画。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct RowLayout {
    /// 这一行顶上先画过程组的组头
    pub head: Option<StepGroup>,
    /// 这一行自己的内容画不画
    pub body: bool,
    /// 组内的一步：缩进、左侧细线
    pub nested: bool,
    /// 下面紧跟同组的下一行（行距收紧，细线连上）
    pub tight: bool,
    /// 属于一个多步过程组（不管这会儿露不露出来）：展开 / 收起时按弹簧进度裁切
    pub member: bool,
    /// 这一行属于哪个过程组（组第一行的下标）；不在组里是 None
    pub group_start: Option<usize>,
    /// 计划正钉在输入框上方：这里只留一行进度
    pub compact: bool,
}

/// 一个过程组的状态（组头用）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StepGroup {
    /// 组的末尾（不含）；组从带组头的那一行开始
    pub end: usize,
    /// 进行中：本轮在这之后还没出正文
    pub live: bool,
    /// 展开看全部步骤（用户点开过，或详细视图）
    pub open: bool,
    /// 在这一组里，活跃且只有几步（不超过 `LIVE_TAIL`）：默认展开，点组头收起
    pub live_small: bool,
}

/// 一轮的用时和用量（回复末尾显示）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TurnSummary {
    /// 旧日志没记用时
    pub elapsed_ms: Option<u64>,
    pub usage: TurnUsage,
}

/// 一轮里累计的 token 用量（内核的 `UsageUpdated` 逐次累加）。
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct TurnUsage {
    pub input: u64,
    pub output: u64,
    /// 其中思考消耗的部分（包含在 `output` 里；内核不报告时为 0）
    pub reasoning: u64,
    /// 本轮端点报过用量没有：没报过时状态行上的数字是估的（界面标成 `≈`）
    pub reported: bool,
}

/// 进行中的一轮此刻在干什么（状态条的左侧文字）。只分三档：
/// 思考和调工具在一轮里来回切换很快，细分会让文字一闪一闪；具体在做哪一步看过程组。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Activity {
    /// 有权限请求等用户应答
    WaitingPermission,
    /// 模型问了问题，等用户选
    WaitingQuestion,
    /// 思考、调工具，或刚发出还没回音
    Working,
    /// 正在输出正文
    Replying,
    /// 端点限流 / 出错，等着自动重试（详情在 [`Session::retry`]）
    Retrying,
    /// 正在把较早的对话压缩成摘要
    Compacting,
}

pub(crate) fn new_list_state() -> ListState {
    // overdraw：视口外多渲染的高度，滚动时少一点空白。
    // 初始 1 行 = 末尾的运行状态行（对话记录的行都插在它前面）
    let state = ListState::new(1, ListAlignment::Top, px(360.));
    // 列表自己「离底 1px 就重新跟随」的隐式吸附交给 `Session::sync_follow` 按几何 + 方向判定
    state.set_auto_reengage(false);
    state.set_follow_mode(FollowMode::Tail);
    state
}

/// 给未测行估高用的排版度量。
///
/// 估高只影响**还没量过的行**：滚动条长度、`distance_to_end`、冷会话跳转落点。
/// 真实高度量到之后逐行替换，估得偏一点只会让滚动条先粗后准，不会串位。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ListMetrics {
    /// 对话列的内宽（阅读列限宽之后、去掉左右内边距之前）
    pub column_w: Pixels,
    /// 窗口 rem（字号缩放后）
    pub rem: Pixels,
    /// 字体族版本号（`theme::font_revision`）：换字体不改字号 / 列宽，但字宽变了，估高同样过期
    pub font_rev: u64,
    /// 列宽是不是按真实视口量出来的。窄窗口第一帧还没量到列表宽度，只能先按限宽铺：
    /// 第二帧拿到真实宽度时不算「用户改了列宽」，别因此清掉滚轮余量。
    pub measured: bool,
}

impl ListMetrics {
    pub(crate) fn body_line(self) -> Pixels {
        crate::theme::TEXT_BODY.to_pixels(self.rem) * 1.48
    }

    pub(crate) fn small_line(self) -> Pixels {
        crate::theme::TEXT_SMALL.to_pixels(self.rem) * 1.45
    }

    /// 一行文字实际可用的宽度（阅读列左右各 2rem 内边距）
    pub(crate) fn inner_w(self) -> Pixels {
        (self.column_w - rems(4.).to_pixels(self.rem)).max(rems(8.).to_pixels(self.rem))
    }

    /// 折叠的一行步骤（工具 / 思考 / 信息）的高度
    pub(crate) fn line_row(self) -> Pixels {
        self.small_line().max(rems(1.75).to_pixels(self.rem))
    }

    /// 块间距（和 `ChatApp` 的 `BLOCK_GAP` 一致）
    pub(crate) fn block_gap(self) -> Pixels {
        rems(1.).to_pixels(self.rem)
    }
}

/// 纯文本按可用宽度折行后的高度估计（CJK 按一个字宽、ASCII 按约 0.55 个字宽）。
/// 只看前 16K 字符，超长正文按长度比例外推，避免在界面线程上扫完整条回复。
pub(crate) fn estimate_text_height(
    text: &str,
    font: Pixels,
    leading: f32,
    inner: Pixels,
) -> Pixels {
    const SCAN: usize = 16 * 1024;
    if text.is_empty() || inner <= px(0.) {
        return px(0.);
    }
    let latin = f32::from(font) * 0.55;
    let wide = f32::from(font);
    let inner = f32::from(inner);
    let mut width = 0f32;
    let mut lines = 1f32;
    let mut scanned = 0usize;
    let mut truncated = false;
    for ch in text.chars() {
        if scanned >= SCAN {
            truncated = true;
            break;
        }
        scanned += ch.len_utf8();
        match ch {
            '\n' => {
                lines += 1.;
                width = 0.;
            }
            '\r' => {}
            _ => {
                let w = if ch.is_ascii() { latin } else { wide };
                if width > 0. && width + w > inner {
                    lines += 1.;
                    width = w;
                } else {
                    width += w;
                }
            }
        }
    }
    if truncated {
        let ratio = text.len() as f32 / scanned.max(1) as f32;
        lines *= ratio;
    }
    px(lines * f32::from(font) * leading)
}

/// 虚拟列表的一行画哪条记录。
///
/// 绝大多数记录一行；很大的**已完成**回复按块拆成多个虚拟行（[`split_markdown`]），
/// 这样滚动 / 排版只碰视口附近的那一块，不用每帧把整篇长 Markdown 排完再裁。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Row {
    /// 整条记录一行
    Whole(usize),
    /// 拆开的大回复：记录 `src` 的第 `part` 块
    Part { src: usize, part: usize },
}

impl Row {
    /// 这一行属于哪条记录。
    pub fn src(self) -> usize {
        match self {
            Row::Whole(i) => i,
            Row::Part { src, .. } => src,
        }
    }
}

/// 超过这个长度的已完成回复才拆块。
const CHUNK_MIN: usize = 4096;
/// 每块的目标长度（在行边界上断，实际按段落 / 代码块 / 表格行找断点）
const CHUNK_BUDGET: usize = 2048;

/// 代码围栏行：返回（围栏字符，连续个数，后面的信息串）。三个以上的 ` 或 ~ 才算。
fn fence_run(t: &str) -> Option<(char, usize, &str)> {
    let ch = t.chars().next()?;
    if ch != '`' && ch != '~' {
        return None;
    }
    let len = t.chars().take_while(|&c| c == ch).count();
    (len >= 3).then(|| (ch, len, &t[len..]))
}

/// 把一段 Markdown 拆成多块。尽量在空行（段落之间）断；断在代码块中间就补上围栏收尾、
/// 下一块重新打开（带语言名）；断在表格中间就重复表头两行，每块单独渲染还是同一张表。
/// 拆不出两块时返回单元素，调用方照常整行画。
pub fn split_markdown(text: &str) -> Vec<String> {
    if text.len() < CHUNK_MIN {
        return vec![text.to_string()];
    }
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    // 打开中的代码围栏：(字符, 连续个数, 首行)。只有同字符、长度不短于它、后面没有别的内容才算收尾
    let mut fence: Option<(char, usize, String)> = None;
    let mut in_table = false;
    // 表格的表头 + 分隔行（重开用）
    let mut table_head: Option<String> = None;
    let mut table_rows = 0usize;
    let mut idx = 0;
    while idx < lines.len() {
        let line = lines[idx];
        let t = line.trim_start();
        let run = fence_run(t);
        let pipe = t.starts_with('|');
        // 先判断这一行是不是收尾围栏（按打开时的字符和长度），再更新状态
        let closing = match (&fence, run) {
            (Some((ch, len, _)), Some((c, n, rest))) => {
                c == *ch && n >= *len && rest.trim().is_empty()
            }
            _ => false,
        };
        if fence.is_none()
            && let Some((c, n, _)) = run
        {
            fence = Some((c, n, line.trim_end().to_string()));
        }
        if fence.is_none() {
            if pipe && !in_table {
                in_table = true;
                table_head = Some(String::new());
                table_rows = 0;
            } else if !pipe {
                in_table = false;
                table_head = None;
            }
            if in_table && table_rows < 2 {
                table_rows += 1;
                if let Some(head) = &mut table_head {
                    head.push_str(line);
                }
            }
        }
        cur.push_str(line);
        if closing {
            fence = None;
        }
        idx += 1;
        if cur.len() < CHUNK_BUDGET {
            continue;
        }
        let next_line = lines.get(idx).copied().unwrap_or("");
        let next = next_line.trim_start();
        let next_run = fence_run(next);
        // 下一行是收尾围栏：和这一块一起收进来，别把孤立的围栏丢到下一块开头
        let next_closes = match (&fence, next_run) {
            (Some((ch, len, _)), Some((c, n, rest))) => {
                c == *ch && n >= *len && rest.trim().is_empty()
            }
            _ => false,
        };
        let next_block = next.starts_with('#') || next_run.is_some();
        // 干净的断点：空行 / 下一个块开始，且不在代码块里
        if fence.is_none() && (line.trim().is_empty() || next_block) {
            out.push(std::mem::take(&mut cur));
            continue;
        }
        // 表格：一行一行断；后面还是表格行才重复表头，表格到此为止就干净收尾
        if in_table && fence.is_none() && table_rows >= 2 {
            out.push(std::mem::take(&mut cur));
            if next.starts_with('|')
                && let Some(head) = &table_head
            {
                cur.push_str(head);
            }
            continue;
        }
        // 代码块 / 长段落：翻倍才硬断；代码块补围栏收尾、下一块重开
        if cur.len() < CHUNK_BUDGET * 2 {
            continue;
        }
        if let Some((ch, len, open)) = &fence {
            if next_closes {
                cur.push_str(next_line);
                idx += 1;
                fence = None;
                out.push(std::mem::take(&mut cur));
                continue;
            }
            if !cur.ends_with('\n') {
                cur.push('\n');
            }
            // 收尾用和打开时同样的字符、不短于打开时的长度
            cur.push_str(&ch.to_string().repeat(*len));
            cur.push('\n');
            out.push(std::mem::take(&mut cur));
            cur.push_str(open);
            cur.push('\n');
        } else {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    if out.len() < 2 {
        return vec![text.to_string()];
    }
    out
}

impl Session {
    /// 记录形态的指纹（见 [`Session::rebuild_rows`] 的触发条件）。
    /// 流式写时末尾回复每帧都在变长，那段长度不计入（`busy` 期间恒为 0），
    /// 一轮结束、正文定稿时才因为 `busy` 翻转和长度变化重算一次。
    pub(crate) fn rows_signature(&self) -> (usize, bool, usize) {
        let last = if self.busy {
            0
        } else {
            match self.transcript.last() {
                Some(ChatItem::Agent(text)) => text.len(),
                _ => 0,
            }
        };
        (self.transcript.len(), self.busy, last)
    }

    /// 按当前记录重算每行画什么（[`Row`]），大回复在这里拆块。返回行划分是否变了。
    pub(crate) fn rebuild_rows(&mut self) -> bool {
        let n = self.transcript.len();
        let mut rows = Vec::with_capacity(n);
        let mut parts = HashMap::new();
        let mut starts = Vec::with_capacity(n);
        for i in 0..n {
            starts.push(rows.len());
            // 正在流式写的那条不拆（每来一个 chunk 都要重排，拆了反而白花功夫）
            let split = match &self.transcript[i] {
                ChatItem::Agent(text) if !(self.busy && i + 1 == n) && text.len() >= CHUNK_MIN => {
                    split_markdown(text)
                }
                _ => Vec::new(),
            };
            if split.len() > 1 {
                let chunks: std::rc::Rc<[SharedString]> =
                    split.into_iter().map(SharedString::from).collect();
                for part in 0..chunks.len() {
                    rows.push(Row::Part { src: i, part });
                }
                parts.insert(i, chunks);
            } else {
                rows.push(Row::Whole(i));
            }
        }
        let changed = !rows.starts_with(&self.rows);
        self.rows = rows;
        self.agent_parts = parts;
        self.row_start = starts;
        changed
    }

    /// 行划分变了（大回复开始 / 结束时）：列表整体重建，行由本帧的补行逻辑重新塞进去。
    /// 阅读位置记在 `rows_restore` 里，等这一帧的行都到位再恢复。
    pub(crate) fn reset_rows(&mut self, anchor: Option<Row>, offset: Pixels, following: bool) {
        self.body_cache.clear();
        self.list.reset(1);
        self.measured.clear();
        self.wheel = None;
        self.pin = None;
        self.follow.force_until = None;
        self.follow.armed = false;
        self.rows_restore = Some((anchor, offset, following));
        // 强制下一段重测状态行（重建后它也是未测的）
        self.measured_busy = !self.status_visible();
    }

    /// 旧行划分里的某一行，在重建后的行划分里对应的下标。
    /// 某条记录拆块数变了时夹到它现在的最后一块；记录被回退掉了就夹到最后一行。
    pub(crate) fn row_after_rebuild(&self, row: Row) -> usize {
        let ix = match row {
            Row::Whole(src) => self.row_of(src),
            Row::Part { src, part } => {
                let start = self.row_of(src);
                let count = self.part_count(src).unwrap_or(1);
                start + part.min(count.saturating_sub(1))
            }
        };
        ix.min(self.rows.len().saturating_sub(1))
    }

    /// 记录 `ix` 的第一行在列表里的下标（大回复拆块后一条记录占多行）。
    pub fn row_of(&self, ix: usize) -> usize {
        self.row_start.get(ix).copied().unwrap_or(ix)
    }

    /// 记录 `ix` 的第 `part` 块文本（大回复拆块时用）。
    pub fn part_text(&self, ix: usize, part: usize) -> Option<SharedString> {
        self.agent_parts.get(&ix).and_then(|c| c.get(part)).cloned()
    }

    /// 第 `part` 块在整条回复里的起始字节。拆开后每块的代码块 span 都从 0 起，
    /// 「刚复制了哪个代码块」要用这个偏移拼出唯一标识。
    pub fn part_start(&self, ix: usize, part: usize) -> usize {
        self.agent_parts.get(&ix).map_or(0, |c| c.iter().take(part).map(|s| s.len()).sum())
    }

    /// 记录 `ix` 被拆成了几块（没拆是 None）。
    pub fn part_count(&self, ix: usize) -> Option<usize> {
        self.agent_parts.get(&ix).map(|c| c.len())
    }

    /// 列表末尾那一行运行状态行显不显示。
    pub(crate) fn status_visible(&self) -> bool {
        self.busy || self.resuming || self.worktree_busy.is_some()
    }
}

impl Session {
    /// 一行（显示行，见 [`Row`]）的测量指纹：内容指纹 + 视图模式 + 会不会跟着状态改行高
    /// （回合末尾的回复下面多一行操作、失败卡片的按钮组会多出「继续」）+ 分组结果
    /// （组头、折叠、行距）。
    pub(crate) fn row_key(&self, i: usize, view: ViewMode) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        let Some(row) = self.rows.get(i).copied() else { return 0 };
        if let Row::Part { src, part } = row {
            // 拆开的大回复：块文本 + 这一块是不是最后一块（操作行 / 改动卡片挂在最后一块上）
            let len = self.agent_parts.get(&src).map_or(0, |c| c[part].len());
            let last = self.agent_parts.get(&src).is_some_and(|c| part + 1 == c.len());
            (2u8, src, part, len, view, self.transcript[src].fingerprint()).hash(&mut h);
            // 这一块里有远程图片：加载完成 / 失败会换高度，状态版本号也要进指纹
            if self
                .agent_parts
                .get(&src)
                .is_some_and(|c| c[part].contains("http://") || c[part].contains("https://"))
            {
                crate::app::remote_image::revision().hash(&mut h);
            }
            if last {
                let tail = self.is_turn_end(src);
                (tail, self.changes_open.contains(&src), self.latest_undo()).hash(&mut h);
            }
            return h.finish();
        }
        let i = row.src();
        let layout = self.layout.get(i).copied().unwrap_or_default();
        // 回合末尾（改动卡 / 用时）：和「失败卡片是不是最新一次」分开记——
        // 合成一个 || 的话，下一轮发出后按钮消失了哈希却没变，虚高的一行不会重测
        let tail = self.is_turn_end(i) || self.is_turn_fallback_tail(i, view);
        let live = if matches!(self.transcript[i], ChatItem::Failed { .. }) {
            (self.failure_is_latest(i), self.failure_resume(i), tail)
        } else {
            (tail, false, tail)
        };
        (self.transcript[i].fingerprint(), view, live, layout).hash(&mut h);
        // 展开着的工具行会随流式输出长高（折叠态只看有没有输出，见 `ChatItem::fingerprint`）
        if let ChatItem::Tool(c) = &self.transcript[i]
            && (c.expanded || view == ViewMode::Verbose)
        {
            c.output.len().hash(&mut h);
        }
        // 回复里的远程图片：加载完成 / 失败会换高度，状态版本号进指纹才会重测那一行
        if let ChatItem::Agent(text) = &self.transcript[i]
            && (text.contains("http://") || text.contains("https://"))
        {
            crate::app::remote_image::revision().hash(&mut h);
        }
        // 回复末尾的改动卡片：展开了全部文件、撤回过，高度和按钮都会变
        if tail {
            (self.changes_open.contains(&i), self.latest_undo()).hash(&mut h);
        }
        h.finish()
    }

    /// 算出每行怎么画：连续的过程步骤合成一组。只有一步（不算空思考）时照常一行、不要组头；
    /// 组默认收起，进行中露出最近 [`LIVE_TAIL`] 步，点开或详细视图看全部；摘要视图整组不画。
    pub fn compute_layout(&self, view: ViewMode) -> Vec<RowLayout> {
        let t = &self.transcript;
        let mut out = vec![RowLayout::default(); t.len()];
        // 最后一段正文 / 用户消息之后的组还在长
        let last_text =
            t.iter().rposition(|i| matches!(i, ChatItem::Agent(_) | ChatItem::User { .. }));
        let pinned = self.pinned_plan();
        let mut i = 0;
        while i < t.len() {
            if !t[i].is_step() {
                out[i].body = t[i].visible_in(view);
                out[i].compact = pinned == Some(i);
                i += 1;
                continue;
            }
            let start = i;
            while i < t.len() && t[i].is_step() {
                i += 1;
            }
            if view == ViewMode::Summary {
                continue;
            }
            let steps: Vec<usize> =
                (start..i).filter(|&k| t[k].visible_in(view) && t[k].has_content()).collect();
            match steps.len() {
                0 => {}
                1 => out[steps[0]].body = true,
                n => {
                    let live = self.busy && last_text.is_none_or(|p| p < start);
                    // 活跃的小组默认全展开：刚开始的几步没有「尾巴」可藏，点一下能收起来
                    let live_small = live && view != ViewMode::Verbose && n <= LIVE_TAIL;
                    let open = if live_small {
                        !self.folded_live_groups.contains(&start)
                    } else {
                        view == ViewMode::Verbose || self.open_groups.contains(&start)
                    };
                    let shown = if open {
                        &steps[..]
                    } else if live && !live_small {
                        // 进行中的长组收起时露出最后几步（尾巴），点组头看全部
                        &steps[n.saturating_sub(LIVE_TAIL)..]
                    } else {
                        &[]
                    };
                    out[start].head = Some(StepGroup { end: i, live, open, live_small });
                    out[start].tight = !shown.is_empty();
                    for &s in &steps {
                        out[s].member = true;
                        out[s].group_start = Some(start);
                    }
                    for (k, &s) in shown.iter().enumerate() {
                        out[s].body = true;
                        out[s].nested = true;
                        out[s].tight = k + 1 < shown.len();
                    }
                }
            }
        }
        out
    }
}

impl Session {
    /// 内容区每帧交进排版度量；和上次不同时 `sync_list` 会按新字号 / 列宽重估并重测所有行。
    pub fn set_list_metrics(&mut self, metrics: ListMetrics) {
        self.metrics = Some(metrics);
    }

    pub fn list_metrics(&self) -> Option<ListMetrics> {
        self.metrics
    }

    /// 给第 `i` **行**估高（未测行先按它排；列表量到真实高度后自动替换）。
    /// 折叠 / 当前视图不显示的行按 0 高算——这和实际测量结果一致，不是瞎猜。
    pub(crate) fn estimate_row_height(&self, i: usize, m: ListMetrics, view: ViewMode) -> Pixels {
        let Some(row) = self.rows.get(i).copied() else { return px(0.) };
        if let Row::Part { src, part } = row {
            // 拆开的大回复：只估这一块，最后一块下面才是回合末尾的操作行
            let Some(chunks) = self.agent_parts.get(&src) else { return px(0.) };
            let mut h = estimate_text_height(
                &chunks[part],
                crate::theme::TEXT_BODY.to_pixels(m.rem),
                1.48,
                m.inner_w(),
            );
            if part + 1 == chunks.len() && self.is_turn_end(src) {
                h += rems(2.).to_pixels(m.rem) + m.block_gap();
            }
            return h.max(px(1.));
        }
        let i = row.src();
        let Some(item) = self.transcript.get(i) else { return px(0.) };
        let layout = self.layout.get(i).copied().unwrap_or_default();
        let mut h = px(0.);
        if layout.head.is_some() {
            h += m.line_row();
            if layout.body || layout.tight {
                h += rems(0.375).to_pixels(m.rem);
            }
        }
        // 这一行自己有没有东西画；合并进工具行的权限请求、当前视图看不见的行都是 0
        let visible_body = layout.body
            || (!matches!(item, ChatItem::Agent(_))
                && self.is_turn_fallback_tail(i, view)
                && item.has_content());
        if !visible_body {
            return h;
        }
        let inner = m.inner_w();
        let body = m.body_line();
        match item {
            ChatItem::User { text, images, .. } => {
                h += estimate_text_height(
                    text,
                    crate::theme::TEXT_BODY.to_pixels(m.rem),
                    1.48,
                    inner,
                );
                h += rems(0.75).to_pixels(m.rem) * 2.; // 气泡内边距
                h += px(96.) * images.len() as f32;
                if self.is_turn_end(i) || layout.tight {
                    h += m.block_gap();
                }
            }
            ChatItem::Agent(text) => {
                h += estimate_text_height(
                    text,
                    crate::theme::TEXT_BODY.to_pixels(m.rem),
                    1.48,
                    inner,
                );
                // 回合末尾的复制 / 重新生成那一行
                if self.is_turn_end(i) {
                    h += rems(2.).to_pixels(m.rem);
                }
            }
            ChatItem::Thinking { text, open, .. } => {
                h += m.line_row();
                if (*open || view == ViewMode::Verbose) && !text.trim().is_empty() {
                    h += rems(0.25).to_pixels(m.rem)
                        + estimate_text_height(
                            text,
                            crate::theme::TEXT_SMALL.to_pixels(m.rem),
                            1.48,
                            inner,
                        );
                }
            }
            ChatItem::Info { text, open } => {
                h += m.line_row();
                if *open || view == ViewMode::Verbose {
                    h += rems(0.25).to_pixels(m.rem)
                        + estimate_text_height(text, m.small_line(), 1.48, inner);
                }
            }
            ChatItem::Tool(card) => {
                h += m.line_row();
                if !card.diffs.is_empty() {
                    let rows: usize = card.diffs.iter().map(|d| d.lines.len()).sum();
                    let shown = if card.expanded || view == ViewMode::Verbose {
                        rows
                    } else {
                        rows.min(8 * card.diffs.len())
                    };
                    h += m.small_line() * shown as f32 * 0.5
                        + m.small_line() * card.diffs.len() as f32 * 1.6;
                }
                if card.expanded || view == ViewMode::Verbose {
                    let text =
                        if card.content.trim().is_empty() { &card.output } else { &card.content };
                    let est = estimate_text_height(
                        text,
                        crate::theme::TEXT_SMALL.to_pixels(m.rem),
                        1.48,
                        inner,
                    )
                    .min(rems(18.).to_pixels(m.rem));
                    h += est + rems(1.25).to_pixels(m.rem);
                }
            }
            ChatItem::Permission { options, hunks, .. } => {
                let hunk_lines: usize = hunks
                    .as_ref()
                    .map(|h| h.files.iter().flat_map(|f| &f.hunks).map(|x| x.lines.len()).sum())
                    .unwrap_or(0);
                h += m.line_row()
                    + m.small_line() * options.len() as f32 * 1.6
                    + m.small_line() * hunk_lines.min(12) as f32;
            }
            ChatItem::Question { questions, .. } => {
                let options: usize = questions.iter().map(|q| q.options.len()).sum();
                h += m.line_row()
                    + m.small_line() * 2. * questions.len() as f32
                    + m.small_line() * options as f32 * 1.6;
            }
            ChatItem::Plan(rows) => {
                let shown = if layout.compact { 1 } else { rows.len() };
                h += m.line_row() + body * shown as f32;
            }
            ChatItem::Failed { .. } => h += rems(5.).to_pixels(m.rem),
            ChatItem::Compaction { state, open, .. } => {
                h += m.line_row();
                if *open && let CompactState::Done(text) | CompactState::Failed(text) = state {
                    h += estimate_text_height(text, m.small_line(), 1.48, inner);
                }
            }
            ChatItem::Note(text) => {
                h += estimate_text_height(
                    text,
                    crate::theme::TEXT_SMALL.to_pixels(m.rem),
                    1.48,
                    inner,
                ) * 0.8;
            }
            ChatItem::AutoApproved { .. } => h += m.line_row(),
        }
        if (layout.body || layout.head.is_some()) && !layout.tight {
            h += m.block_gap();
        } else if layout.body {
            h += rems(0.375).to_pixels(m.rem);
        }
        h.max(px(1.))
    }
}

impl Session {
    /// 把对话记录的变化同步给虚拟列表：新增的行追加，高度可能变了的行重新测量。
    /// 列表比记录多一行：第 `n` 行是运行状态行，忙 / 闲切换时重新测量。
    /// 每次渲染前调用；滚动等没有改变内容的帧只比对版本。内容改变时再重算分组和行指纹。
    /// 指纹里带着视图模式和分组结果，切模式、展开 / 收起组时相关的行自动重测。
    pub fn sync_list(&mut self, view: ViewMode) {
        // 跟在底部（或已经在吸附带里待命）就是都看到了
        if self.list.is_following_tail() || self.follow.armed {
            self.seen_seq = self.content_seq;
        }
        let mut sync = SyncSig {
            layout_rev: self.layout_rev,
            transcript_rev: self.transcript.revision,
            content_seq: self.content_seq,
            rows: self.rows.len(),
            view,
            metrics: self.metrics,
            images: crate::app::remote_image::revision(),
            status_visible: self.status_visible(),
            busy: self.busy,
            compact_turn: self.compact_turn,
            undo: self.undo,
        };
        if self.last_sync == Some(sync) && self.rows_restore.is_none() {
            self.restore_list_position();
            return;
        }
        self.layout = self.compute_layout(view);
        // 大回复拆块只跟记录形态有关（条数 / 是否在跑 / 末尾回复多长），和视图模式无关
        let sig = self.rows_signature();
        if self.rows_sig != Some(sig) {
            self.rows_sig = Some(sig);
            let anchor = self.list.logical_scroll_top();
            let following = self.list.is_following_tail();
            let anchor_row = self.rows.get(anchor.item_ix).copied();
            if self.rebuild_rows() {
                self.reset_rows(anchor_row, anchor.offset_in_item, following);
            }
        }
        let n = self.rows.len();
        sync.rows = n;
        self.last_sync = Some(sync);
        // 字号 / 列宽变了：屏外缓存的行高全部过期，按新度量重估 + 重测。
        // 视口顶那一行按比例保留（`remeasure_with_hints` 走 proportional 锚点）
        if let Some(m) = self.metrics
            && self.applied_metrics != Some(m)
        {
            let prev = self.applied_metrics;
            self.applied_metrics = Some(m);
            // `measured` 只影响滚轮余量要不要作废，不改变行高：只有列宽 / 字号 / 字体真变了才重排重测
            let layout_changed = prev.is_none_or(|p| {
                p.column_w != m.column_w || p.rem != m.rem || p.font_rev != m.font_rev
            });
            if layout_changed {
                if n > 0 {
                    let hints: Vec<Pixels> =
                        (0..n).map(|i| self.estimate_row_height(i, m, view)).collect();
                    self.list.remeasure_with_hints(0..n, hints);
                }
                // 末尾的运行状态行跟着字号变
                self.list.remeasure_items(n..n + 1);
                self.measured_busy = self.status_visible();
                // 滚轮余量只在「用户真改了列宽 / 字号」时作废（相对像素在重排后仍然有效）。
                // 窄窗口第 1 帧按限宽铺、第 2 帧才有真实宽度，那一次不算用户改动。
                let real_width_change =
                    prev.is_some_and(|p| p.measured && m.measured && p.column_w != m.column_w);
                let type_change = prev.is_some_and(|p| p.rem != m.rem || p.font_rev != m.font_rev);
                if real_width_change || type_change {
                    self.wheel = None;
                }
            }
        }
        if n < self.measured.len() {
            // 记录只会变长；万一变短就整体重来
            self.list.reset(1);
            self.measured.clear();
            // 列表被重置，旧的滚轮余量对不上号
            self.wheel = None;
        }
        let old = self.measured.len();
        for i in 0..old {
            let f = self.row_key(i, view);
            if f != self.measured[i] {
                self.measured[i] = f;
                self.list.remeasure_items(i..i + 1);
            }
        }
        if n > old {
            for i in old..n {
                let f = self.row_key(i, view);
                self.measured.push(f);
            }
            // 插在状态行前面；新行先给估高，滚动条和底部判定第一帧就有意义
            let hints = match self.applied_metrics {
                Some(m) => (old..n).map(|i| self.estimate_row_height(i, m, view)).collect(),
                None => vec![px(0.); n - old],
            };
            self.list.splice_with_hints(old..old, hints);
        }
        // 状态行在忙 / 续接 / 建 worktree 时出现（见 `render_turn_status`）
        let status = self.status_visible();
        if status != self.measured_busy {
            self.measured_busy = status;
            self.list.remeasure_items(n..n + 1);
        }
        // 行划分刚重建过：行都塞进去了，把阅读位置放回去
        if let Some((anchor_row, offset, following)) = self.rows_restore.take() {
            if following {
                self.list.scroll_to_end();
            } else if let Some(ix) = anchor_row.map(|row| self.row_after_rebuild(row)) {
                self.list.scroll_to(ListOffset { item_ix: ix, offset_in_item: offset });
            }
        }
        self.restore_list_position();
    }

    // Search and session restore can change the scroll position without changing row layout.
    fn restore_list_position(&mut self) {
        if let Some((offset, following)) = self.display_scroll.take() {
            // 会话恢复的位置说了算：滚轮剩下的余量作废
            self.wheel = None;
            self.follow.force_until = None;
            self.follow.armed = false;
            if following {
                self.list.scroll_to_end();
            } else {
                self.list.scroll_to(offset);
                self.quiet_follow(FOLLOW_QUIET);
            }
        }
        // 行数到位之后再滚，否则会被截到末尾；再往上让一点，别让顶上的渐隐盖住这一行。
        // 未测行现在带估高，`scroll_by` 不会再被一串 0 高行推到最后一行
        if let Some(ix) = self.reveal.take().filter(|&ix| ix < self.transcript.len()) {
            self.wheel = None;
            self.follow.force_until = None;
            self.follow.armed = false;
            let row = self.row_of(ix);
            self.list.scroll_to(ListOffset { item_ix: row, offset_in_item: px(0.) });
            self.list.scroll_by(-px(24.));
            self.quiet_follow(FOLLOW_QUIET);
        }
    }
}

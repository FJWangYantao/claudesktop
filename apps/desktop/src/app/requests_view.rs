//! 首页用量卡片的「请求」页签：用量网关记下的每个请求，按「会话的第几轮」归组（归组与汇总见 `requests.rs`）。
//!
//! 日志在切到这个页签、换时间范围、点刷新时在后台读；点一轮展开看里面的每次请求（左边一条时间轴，
//! 看出每次请求落在这一轮的哪一段）。费用按设置里的模型单价估算（`pricing.rs`）。
//! 自研内核的请求都经过网关；ACP agent 的要在设置里打开「经过用量网关」、agent 认 base URL 才有。

use super::ChatApp;
use super::menu::{MenuChip, MenuList, menu_item, menu_popover};
use super::widgets::Surface;
use crate::model::StatCard;
use crate::pricing::{self, Cost, PeakWindow, Price};
use crate::requests::{self, SessionRef, TurnRow};
use crate::theme::rgb;
use crate::theme::*;
use crate::usage;
use claudesktop_builtin::gateway::{self, RequestLog};
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

/// 先列多少轮；「显示更多」每次再多列这么多（首页整页滚动，不另开滚动框）
const PAGE_ROWS: usize = 20;

/// 视口上下各多排这么远（滚得快时边上不露白）
const OVERSCAN: Pixels = px(200.);

/// 「请求」页签的状态。
#[derive(Default)]
pub(super) struct RequestsView {
    /// 读到的日志；None = 还没读
    logs: Option<Arc<Vec<RequestLog>>>,
    /// 读的是哪个时间范围（`overview_range`）
    loaded_range: Option<usize>,
    loading: bool,
    /// 已经约好过一会儿重读（跑着的轮又发完一个请求，见 `refresh_requests_soon`）
    reload_soon: bool,
    /// 点开的轮（`TurnRow::key`）
    open: HashSet<(String, u64)>,
    /// 只看某个模型 / 某个会话（`Session::token`）/ 某一天（`usage::day_of`，用量页的筛选）
    pub(super) model: Option<String>,
    pub(super) session: Option<u64>,
    pub(super) day: Option<i64>,
    /// 多列出来的页数（「显示更多」）
    pub(super) more: usize,
    /// 上次归组、筛选、汇总的结果（见 `derived_requests`）
    derived: RefCell<Option<Rc<Derived>>>,
    /// 列表只排视口附近的几轮（见 [`ListWindow`]）
    window: Rc<RefCell<ListWindow>>,
}

/// 列表的窗口化：首页整页滚动、列表在页面中间，用不了自带滚动框的虚拟列表，
/// 所以按首页的滚动位置算出视口附近是哪几轮，只排这几轮，上下用空白占住其余的高度
/// （「显示更多」点了几次之后，几百轮全排一遍，滚一格要上百毫秒）。
#[derive(Default)]
struct ListWindow {
    /// 列表顶端在滚动内容里的 y（画过一帧才知道）
    top: Option<Pixels>,
    /// 量高度时的列表宽度（变了量过的高度全作废）
    width: Pixels,
    /// 画过的轮的高度：[收起时, 展开时]
    heights: HashMap<(String, u64), [Option<Pixels>; 2]>,
    /// 没画过的收起的轮按这个估（画过的收起的轮的高度）
    estimate: Option<Pixels>,
}

impl ListWindow {
    /// 一轮的高度：量过的，或者估一个（展开的按每次请求一行估）。
    fn height(&self, row: &TurnRow, open: bool, line: Pixels) -> Pixels {
        let measured = self.heights.get(&row.key).and_then(|h| h[open as usize]);
        let collapsed = self.estimate.unwrap_or(line * 3.5);
        measured.unwrap_or(if open {
            collapsed + line * row.requests.len() as f32 * 1.6
        } else {
            collapsed
        })
    }
}

/// 「请求」页签按当前日志、筛选和单价算好的数据（用量页的费用也从这里取）。
pub(super) struct Derived {
    /// 算的是哪一份日志（重读日志才换）
    logs: Arc<Vec<RequestLog>>,
    key: DerivedKey,
    rows: Vec<TurnRow>,
    /// 筛选的候选：模型按出现次数，会话按最近
    models: Vec<String>,
    sessions: Vec<SessionRef>,
    /// 筛选后留下的轮（`rows` 的下标）
    shown: Vec<usize>,
    pub(super) sum: requests::Summary,
    pub(super) cost: Cost,
    /// 筛选后各模型、各会话（`Session::token`）的费用
    pub(super) model_cost: HashMap<String, Cost>,
    pub(super) session_cost: HashMap<u64, Cost>,
    /// 筛选后对话请求的缓存命中率
    pub(super) cache_hit: Option<u32>,
}

/// 除日志外，影响 [`Derived`] 的输入。
#[derive(PartialEq)]
struct DerivedKey {
    names: HashMap<String, SessionRef>,
    model: Option<String>,
    session: Option<u64>,
    day: Option<i64>,
    prices: Vec<Price>,
    peaks: Vec<PeakWindow>,
}

impl ChatApp {
    /// 用量页或首页「请求」页签正开着（请求日志只在它开着时才值得读）。
    pub(super) fn requests_shown(&self) -> bool {
        if self.settings_open {
            self.settings_form.tab == super::usage_view::USAGE_TAB
        } else {
            self.overview_tab == 2
        }
    }

    /// 在首页「请求」页签上或用量页开着、日志还没按当前范围读过：在后台读。
    pub(super) fn sync_requests(&mut self, cx: &mut Context<Self>) {
        // 用量页整页都要（费用、缓存命中也从请求日志算）
        let shown = self.requests_shown();
        let v = &self.requests_view;
        if shown && !v.loading && v.loaded_range != Some(self.overview_range) {
            self.load_requests(cx);
        }
    }

    /// 列表所在的整页滚动区：首页，或用量页。
    fn list_scroll(&self) -> &ScrollHandle {
        if self.settings_open { &self.settings_scroll } else { &self.home_scroll }
    }

    fn load_requests(&mut self, cx: &mut Context<Self>) {
        let Some(dir) = gateway::default_dir() else {
            return;
        };
        let range = self.overview_range;
        let days = match range {
            1 => Some(30),
            2 => Some(7),
            _ => None,
        };
        let since = days.map(|d: u64| super::now_ms().saturating_sub(d * 86_400_000));
        self.requests_view.loading = true;
        cx.spawn(async move |this, cx| {
            let logs = cx
                .background_executor()
                .spawn(async move { gateway::read_logs(&dir, since) })
                .await;
            let _ = this.update(cx, |app, cx| {
                let v = &mut app.requests_view;
                v.logs = Some(Arc::new(logs));
                // 同一范围重读（跑着的轮刷新）不收起已经展开的「显示更多」
                if v.loaded_range != Some(range) {
                    v.more = 0;
                }
                v.loaded_range = Some(range);
                v.loading = false;
                cx.notify();
            });
        })
        .detach();
    }

    /// 网关又记下了请求（跑着的轮报了用量、一轮结束）：过一会儿重读日志。
    /// 页面开着就读，没开着记成过期，下次打开再读；一轮里请求一个接一个，攒 2 秒读一次。
    pub(super) fn refresh_requests_soon(&mut self, cx: &mut Context<Self>) {
        let v = &mut self.requests_view;
        if !self.settings.usage_gateway || v.logs.is_none() || v.reload_soon {
            return;
        }
        v.reload_soon = true;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(std::time::Duration::from_secs(2)).await;
            let _ = this.update(cx, |app, cx| {
                app.requests_view.reload_soon = false;
                // 上一次还没读完：读完会把范围记成已读，这次的就丢了，所以再约一次
                if app.requests_view.loading {
                    return app.refresh_requests_soon(cx);
                }
                app.requests_view.loaded_range = None;
                app.sync_requests(cx);
            });
        })
        .detach();
    }

    /// 压测钩子：灌 `n` 轮假请求（每轮 3 次，几个会话和模型轮着来）；`all` = 全部列出来，否则只列第一页。
    pub(super) fn debug_fill_requests(&mut self, n: usize, all: bool) {
        let now = super::now_ms();
        let models = ["deepseek-chat", "kimi-k2-0905-preview", "claude-sonnet-4-5"];
        let mut logs = Vec::with_capacity(n * 3);
        for turn in 0..n as u64 {
            for k in 0..3u64 {
                logs.push(RequestLog {
                    at_ms: now.saturating_sub((n as u64 - turn) * 60_000) + k * 4_000,
                    req_id: Some(turn * 3 + k),
                    attempt: Some(1),
                    session: Some(format!("stress-{}", turn % 7)),
                    turn: Some(turn),
                    purpose: "turn".into(),
                    protocol: "openai".into(),
                    model: Some(models[turn as usize % models.len()].into()),
                    host: "api.example.com".into(),
                    path: "/v1/chat/completions".into(),
                    method: "POST".into(),
                    status: 200,
                    ttfb_ms: Some(300),
                    first_token_ms: Some(800),
                    total_ms: 3_000,
                    input: 20_000 + turn * 50,
                    output: 600,
                    cache_read: 12_000,
                    ..Default::default()
                });
            }
        }
        self.requests_view = RequestsView {
            logs: Some(Arc::new(logs)),
            loaded_range: Some(self.overview_range),
            more: if all { n / PAGE_ROWS } else { 0 },
            ..Default::default()
        };
    }

    /// 归组、筛选、汇总的结果，输入没变就用上次的（滚一格都要重画，日志多时每帧重算很贵）。
    /// `day`：只看那天（用量页的筛选；首页「请求」页签不按天筛）。
    fn derived_requests(&self, logs: &Arc<Vec<RequestLog>>, day: Option<i64>) -> Rc<Derived> {
        let v = &self.requests_view;
        let key = DerivedKey {
            names: self.request_names(),
            model: v.model.clone(),
            session: v.session,
            day,
            prices: self.settings.prices(),
            peaks: self.settings.price_peaks(),
        };
        if let Some(d) = v.derived.borrow().as_ref()
            && Arc::ptr_eq(&d.logs, logs)
            && d.key == key
        {
            return d.clone();
        }
        let rows = requests::group(logs, &key.names);
        // 筛选的候选：模型按出现次数，会话按最近
        let mut models: Vec<(String, usize)> = Vec::new();
        let mut sessions: Vec<SessionRef> = Vec::new();
        for row in &rows {
            if let Some(m) = &row.model {
                match models.iter_mut().find(|(n, _)| n == m) {
                    Some((_, c)) => *c += 1,
                    None => models.push((m.clone(), 1)),
                }
            }
            if let Some(s) = &row.session
                && !sessions.iter().any(|x| x.token == s.token)
            {
                sessions.push(s.clone());
            }
        }
        models.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let shown: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter(|(_, r)| key.model.as_ref().is_none_or(|m| r.model.as_ref() == Some(m)))
            .filter(|(_, r)| {
                key.session.is_none_or(|t| r.session.as_ref().is_some_and(|s| s.token == t))
            })
            .filter(|(_, r)| {
                key.day.is_none_or(|d| usage::day_of(r.at_ms, usage::local_offset_secs()) == d)
            })
            .map(|(i, _)| i)
            .collect();
        let shown_rows: Vec<&TurnRow> = shown.iter().map(|&i| &rows[i]).collect();
        let sum = requests::summarize(&shown_rows);
        let cost = Cost::of(&key.prices, &key.peaks, shown_rows.iter().flat_map(|r| &r.requests));
        let mut by_model: HashMap<&str, Vec<&RequestLog>> = HashMap::new();
        let mut by_session: HashMap<u64, Vec<&RequestLog>> = HashMap::new();
        for row in &shown_rows {
            for r in &row.requests {
                if let Some(m) = &r.model {
                    by_model.entry(m).or_default().push(r);
                }
                if let Some(s) = &row.session {
                    by_session.entry(s.token).or_default().push(r);
                }
            }
        }
        let model_cost = by_model
            .into_iter()
            .map(|(m, logs)| (m.to_string(), Cost::of(&key.prices, &key.peaks, logs)))
            .collect();
        let session_cost = by_session
            .into_iter()
            .map(|(t, logs)| (t, Cost::of(&key.prices, &key.peaks, logs)))
            .collect();
        let (cache_read, input) =
            shown_rows.iter().fold((0, 0), |(c, i), r| (c + r.cache_read, i + r.input));
        let cache_hit = usage::cache_hit_percent(cache_read, input);
        let d = Rc::new(Derived {
            logs: logs.clone(),
            models: models.into_iter().map(|(m, _)| m).collect(),
            sessions,
            shown,
            sum,
            cost,
            model_cost,
            session_cost,
            cache_hit,
            rows,
            key,
        });
        *v.derived.borrow_mut() = Some(d.clone());
        d
    }

    /// 内核会话 ID → 侧栏里的会话（续接过的会话有好几个 ID）。
    fn request_names(&self) -> HashMap<String, SessionRef> {
        let mut names = HashMap::new();
        for s in &self.sessions {
            let r = SessionRef { title: s.title.to_string(), token: s.token };
            let ids = s.meta.iter().flat_map(|m| m.kernel_sessions.iter()).chain(&s.acp_id);
            for id in ids {
                names.insert(id.clone(), r.clone());
            }
        }
        names
    }

    /// 用量页要的费用、缓存命中：网关开着、日志读到了才有。
    pub(super) fn request_costs(&self) -> Option<Rc<Derived>> {
        if !self.settings.usage_gateway {
            return None;
        }
        let logs = self.requests_view.logs.clone()?;
        Some(self.derived_requests(&logs, self.requests_view.day))
    }

    /// 请求列表。`embedded` = 用量页里的那份：筛选跟着整页走、总数在页顶，这里不再画筛选菜单和统计卡。
    pub(super) fn render_usage_requests(&self, embedded: bool, cx: &mut Context<Self>) -> Div {
        let hint = |text: &'static str| {
            div().py_6().text_size(TEXT_BODY).text_color(rgb(INK_SOFT)).child(text)
        };
        if !self.settings.usage_gateway {
            return div().child(hint(
                "用量网关关着，没有请求记录。在 设置 → 内核与模型 里打开后，自研内核的每个请求都会记下来。",
            ));
        }
        let v = &self.requests_view;
        let Some(logs) = v.logs.clone() else {
            return div().child(hint("正在读取请求记录…"));
        };
        let d = self.derived_requests(&logs, if embedded { v.day } else { None });
        let shown: Vec<&TurnRow> = d.shown.iter().map(|&i| &d.rows[i]).collect();
        let (sum, cost) = (&d.sum, &d.cost);
        let (prices, peaks) = (&d.key.prices, &d.key.peaks);
        let (models, sessions) = (d.models.clone(), d.sessions.clone());

        let model_label = v.model.clone().unwrap_or_else(|| "全部模型".into());
        let session_label = v
            .session
            .and_then(|t| sessions.iter().find(|s| s.token == t))
            .map_or_else(|| "全部会话".to_string(), |s| s.title.clone());
        let model_items: Vec<String> = models.into_iter().take(20).collect();
        let session_items: Vec<SessionRef> = sessions.into_iter().take(20).collect();
        let filters = div()
            .h_flex()
            .items_center()
            .gap_1()
            .when(!embedded, |d| {
                d.child(menu_popover(
                    "req-model",
                    Anchor::TopLeft,
                    rems(18.),
                    &cx.entity(),
                    &self.menu_focus,
                    MenuChip::new(IconAll::Cpu, model_label).filled().shrink(),
                    move |app, handle, popover| {
                        let current = app.requests_view.model.clone();
                        let mut menu = MenuList::new("req-model-list", handle, popover).pick(
                            menu_item(
                                "req-model-all",
                                IconAll::Cpu,
                                "全部模型",
                                "",
                                current.is_none(),
                            ),
                            |this, cx| {
                                this.requests_view.model = None;
                                this.requests_view.more = 0;
                                cx.notify();
                            },
                        );
                        for (i, m) in model_items.iter().enumerate() {
                            let pick = m.clone();
                            menu = menu.pick(
                                menu_item(
                                    ("req-model-item", i),
                                    IconAll::Cpu,
                                    m.clone(),
                                    "",
                                    current.as_ref() == Some(m),
                                ),
                                move |this, cx| {
                                    this.requests_view.model = Some(pick.clone());
                                    this.requests_view.more = 0;
                                    cx.notify();
                                },
                            );
                        }
                        menu
                    },
                ))
            })
            .when(!embedded, |d| {
                d.child(menu_popover(
                    "req-session",
                    Anchor::TopLeft,
                    rems(20.),
                    &cx.entity(),
                    &self.menu_focus,
                    MenuChip::new(IconAll::MessageSquare, session_label).filled().shrink(),
                    move |app, handle, popover| {
                        let current = app.requests_view.session;
                        let mut menu = MenuList::new("req-session-list", handle, popover).pick(
                            menu_item(
                                "req-session-all",
                                IconAll::MessageSquare,
                                "全部会话",
                                "",
                                current.is_none(),
                            ),
                            |this, cx| {
                                this.requests_view.session = None;
                                this.requests_view.more = 0;
                                cx.notify();
                            },
                        );
                        for (i, s) in session_items.iter().enumerate() {
                            let token = s.token;
                            menu = menu.pick(
                                menu_item(
                                    ("req-session-item", i),
                                    IconAll::MessageSquare,
                                    s.title.clone(),
                                    "",
                                    current == Some(token),
                                ),
                                move |this, cx| {
                                    this.requests_view.session = Some(token);
                                    this.requests_view.more = 0;
                                    cx.notify();
                                },
                            );
                        }
                        menu
                    },
                ))
            })
            .child(div().flex_1())
            .child(
                Self::quiet_button("req-refresh", IconAll::RefreshCw, Surface::Sand, cx)
                    .small()
                    .tooltip("重新读取")
                    .loading(v.loading)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.load_requests(cx);
                        cx.notify();
                    })),
            );

        let dash = || SharedString::from("—");
        let stats = [
            StatCard { label: "请求", value: usage::compact(sum.requests as u64).into() },
            StatCard { label: "累计输入", value: usage::compact(sum.input).into() },
            StatCard { label: "输出", value: usage::compact(sum.output).into() },
        ];
        // 两种币种各占一行（并排一行放不下）
        let money = cost.parts();
        let cost_card = Self::stat_card(&StatCard {
            label: "估算费用",
            value: money.first().map_or_else(dash, |m| m.clone().into()),
        })
        .children(money.into_iter().skip(1).map(|m| {
            div()
                .text_size(TEXT_LEAD)
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(INK))
                .child(m)
        }));
        let mut notes = vec![format!("{} 轮", sum.turns)];
        if embedded {
            notes.push(format!("{} 次请求", sum.requests));
        }
        if let Some(ms) = sum.first_token_ms {
            notes.push(format!("首字中位 {}", requests::ms_label(ms)));
        }
        if let Some(saved) = cost.saved_label() {
            notes.push(format!("缓存省下约 {saved}"));
        }
        if cost.peak > 0 {
            notes.push(format!("{} 次请求按峰价算", cost.peak));
        }
        match cost.unpriced.as_slice() {
            [] => {}
            [one] => notes.push(format!("{one} 没有单价")),
            many => notes.push(format!("{} 个模型没有单价", many.len())),
        }
        if sum.retries > 0 {
            notes.push(format!("重试 {} 次", sum.retries));
        }
        if sum.failed > 0 {
            notes.push(format!("失败 {} 次", sum.failed));
        }

        let mut list = div().v_flex().gap_1p5();
        if shown.is_empty() {
            list = list.child(hint(
                "这段时间还没有请求记录（自研内核的请求都会记下；ACP agent 的要在设置里打开「经过用量网关」）",
            ));
        }
        let limit = PAGE_ROWS * (v.more + 1);
        list = self.request_rows(list, &shown[..shown.len().min(limit)], prices, peaks, cx);
        if shown.len() > limit {
            list = list.child(
                div().h_flex().justify_center().pt_1().child(
                    Self::quiet_button("req-more", IconAll::ChevronDown, Surface::Sand, cx)
                        .small()
                        .label(format!("再显示 {} 轮", (shown.len() - limit).min(PAGE_ROWS)))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.requests_view.more += 1;
                            cx.notify();
                        })),
                ),
            );
        }

        // 用量页里：说明一行和刷新按钮并排
        let notes =
            meta_line(notes.into_iter().map(|n| (n, INK_SOFT)).collect()).text_size(TEXT_SMALL);
        if embedded {
            return div()
                .v_flex()
                .gap_3()
                .child(div().h_flex().items_center().gap_2().child(notes.flex_1()).child(filters))
                .child(list);
        }
        div()
            .v_flex()
            .gap_3()
            .child(filters)
            .child(
                div()
                    .h_flex()
                    // 费用卡可能两行，几张卡一样高
                    .items_stretch()
                    .gap_3()
                    .children(stats.iter().map(Self::stat_card))
                    .child(cost_card),
            )
            .child(notes)
            .child(list)
    }

    /// 列出的这些轮里只排视口附近的（见 [`ListWindow`]），上下各一块空白占住其余的高度。
    /// 直接加进 `list`（竖排、行距 `gap_1p5`）：多包一层容器，taffy 会把每轮的文字多量几遍，
    /// 20 轮一帧就多出好几毫秒。
    fn request_rows(
        &self,
        list: Div,
        rows: &[&TurnRow],
        prices: &[Price],
        peaks: &[PeakWindow],
        cx: &mut Context<Self>,
    ) -> Div {
        let state = self.requests_view.window.clone();
        let rem = crate::theme::root_font_size();
        let (gap, line) = (rems(0.375).to_pixels(rem), TEXT_SMALL.to_pixels(rem));
        let scroll = self.list_scroll().clone();
        let viewport = scroll.bounds().size.height;
        // 滚过头（顶上再往上、底下再往下）时滚动区要到排版之后才把位置收回来，这里先自己收住，
        // 否则这一帧按越界的位置算，视口里一片空白
        let max = scroll.max_offset().y.max(px(0.));
        let scrolled = (-scroll.offset().y).clamp(px(0.), max);
        // 视口在列表里的范围；还没画过（不知道列表在哪）就先排一屏
        let (lo, hi) = match state.borrow().top {
            Some(top) if viewport > px(0.) => {
                (scrolled - top - OVERSCAN, scrolled - top + viewport + OVERSCAN)
            }
            _ => (px(0.), viewport.max(px(900.)) + OVERSCAN),
        };
        // 每轮占「高 + 行距」：before 是第一个排出来的那轮之前的，after 是最后一个之后的
        let (mut first, mut last) = (rows.len(), rows.len());
        let (mut before, mut after, mut y) = (px(0.), px(0.), px(0.));
        {
            let w = state.borrow();
            for (i, row) in rows.iter().enumerate() {
                let h = w.height(row, self.requests_view.open.contains(&row.key), line);
                if first == rows.len() && y + h >= lo {
                    first = i;
                    before = y;
                }
                if first < rows.len() && last == rows.len() && y > hi {
                    last = i;
                }
                if last < rows.len() {
                    after += h + gap;
                }
                y += h + gap;
            }
        }
        let keys: Vec<((String, u64), bool)> = rows[first..last]
            .iter()
            .map(|r| (r.key.clone(), self.requests_view.open.contains(&r.key)))
            .collect();
        // list 里排在这几轮前面的子元素：量位置的 canvas、上面的空白
        let skip = 1 + usize::from(before > gap);
        let top_state = state.clone();
        // 量列表顶端在滚动内容里的位置（上面的统计卡片变高时会变）；绝对定位，不占行距
        let probe = canvas(
            move |bounds, window, _| {
                let top = bounds.origin.y - scroll.bounds().origin.y - scroll.offset().y;
                let mut w = top_state.borrow_mut();
                if w.top.is_none_or(|t| (t - top).abs() > px(0.5)) {
                    w.top = Some(top);
                    window.request_animation_frame();
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .w_full()
        .h(px(0.));
        list.relative()
            .child(probe)
            // 空白和相邻的那轮之间还有一个行距，空白本身少算一个
            .when(before > gap, |d| d.child(div().flex_none().h(before - gap)))
            .children(
                rows[first..last]
                    .iter()
                    .enumerate()
                    .map(|(k, row)| self.request_turn(first + k, row, prices, peaks, cx)),
            )
            .when(after > gap, |d| d.child(div().flex_none().h(after - gap)))
            // 量排出来的几轮的高度；和估的不一样就再排一帧
            .on_children_prepainted(move |bounds, window, _| {
                let rows = bounds.get(skip..).unwrap_or_default();
                let mut w = state.borrow_mut();
                if let Some(width) = rows.first().map(|b| b.size.width)
                    && (w.width - width).abs() > px(0.5)
                {
                    w.width = width;
                    w.heights.clear();
                }
                let mut changed = false;
                for (b, (key, open)) in rows.iter().zip(&keys) {
                    let slot = &mut w.heights.entry(key.clone()).or_default()[*open as usize];
                    if slot.is_none_or(|h| (h - b.size.height).abs() > px(0.5)) {
                        *slot = Some(b.size.height);
                        changed = true;
                    }
                    if !*open && w.estimate.is_none() {
                        w.estimate = Some(b.size.height);
                    }
                }
                if changed {
                    window.request_animation_frame();
                }
            })
    }

    /// 一轮：标题、时间，一行用量；点开列出每次请求。
    fn request_turn(
        &self,
        i: usize,
        row: &TurnRow,
        prices: &[Price],
        peaks: &[PeakWindow],
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let open = self.requests_view.open.contains(&row.key);
        let title = match &row.session {
            Some(s) => s.title.clone(),
            None if row.purpose == "models" => "列出模型".into(),
            None if row.key.1 > 0 && !row.key.0.contains('#') => "已删除的会话".into(),
            None => requests::purpose_label(&row.purpose).into(),
        };
        let mut parts = vec![format!("{} 次请求", row.requests.len())];
        if row.input > 0 {
            parts.push(format!("累计输入 {}", usage::compact(row.input)));
        }
        // 只有一次请求时上下文就等于累计输入，不重复说
        if row.context > 0 && row.requests.len() > 1 {
            parts.push(format!("上下文 {}", usage::compact(row.context)));
        }
        if row.output > 0 {
            parts.push(format!("输出 {}", usage::compact(row.output)));
        }
        if let Some(c) = Cost::of(prices, peaks, &row.requests).label() {
            parts.push(format!("约 {c}"));
        }
        parts.push(format!("用时 {}", requests::ms_label(row.elapsed_ms)));
        if row.subagents > 0 {
            parts.push(format!("子任务 {} 次请求", row.subagents));
        }
        if row.retries > 0 {
            parts.push(format!("重试 {} 次", row.retries));
        }
        if row.cancelled {
            parts.push("已停止".into());
        }
        let key = row.key.clone();
        let mut card = div()
            .id(("req-turn", i))
            .v_flex()
            .gap_1()
            .px_3()
            .py_2()
            .rounded_lg()
            .bg(rgb(CARD))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                let open = &mut this.requests_view.open;
                if !open.remove(&key) {
                    open.insert(key.clone());
                }
                cx.notify();
            }))
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Icon::new(if open { IconAll::ChevronDown } else { IconAll::ChevronRight })
                            .size_3()
                            .text_color(rgb(INK_SOFT)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(TEXT_BODY)
                            .text_color(rgb(INK))
                            .child(title),
                    )
                    .when_some(row.model.clone(), |d, m| {
                        d.child(
                            div()
                                .flex_shrink_0()
                                .max_w(rems(10.))
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .font_family(MONO)
                                .text_size(TEXT_META)
                                .text_color(rgb(INK_SOFT))
                                .child(m),
                        )
                    })
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(TEXT_SMALL)
                            .text_color(rgb(INK_SOFT))
                            .font_features(tabular_nums())
                            .child(match &row.session {
                                // 同一个会话的几轮靠「第几轮」分开
                                Some(_) => {
                                    format!("第 {} 轮 · {}", row.key.1, time_label(row.at_ms))
                                }
                                None => time_label(row.at_ms),
                            }),
                    ),
            )
            .child({
                let mut parts: Vec<(String, Color)> =
                    parts.into_iter().map(|p| (p, INK_SOFT)).collect();
                if row.failed > 0 {
                    parts.push((format!("失败 {} 次", row.failed), RED));
                }
                meta_line(parts).pl(rems(1.25)).text_size(TEXT_SMALL)
            });
        if open {
            let mut detail = div().v_flex().gap_0p5().pt_1().pl(rems(1.25));
            for (k, r) in row.requests.iter().enumerate() {
                // 存了原文的：行尾一个「原文」，点开看发出去的请求体和返回（别让卡片跟着收起）
                let raw = r.body.is_some().then(|| {
                    let log = r.clone();
                    Self::quiet_button(
                        ("req-raw", i * 1000 + k),
                        IconAll::Braces,
                        Surface::Paper,
                        cx,
                    )
                    .xsmall()
                    .label("原文")
                    .px_1p5()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.open_raw_request(&log, window, cx);
                    }))
                });
                detail = detail.child(
                    div()
                        .h_flex()
                        .items_start()
                        .gap_2()
                        .child(timeline_bar(row, r))
                        .child(div().flex_1().min_w_0().child(request_line(
                            k,
                            r,
                            &row.requests[..k],
                            prices,
                            peaks,
                        )))
                        .children(raw.map(|b| div().flex_none().mt(rems(-0.125)).child(b))),
                );
            }
            card = card.child(detail);
        }
        card
    }
}

/// 时间轴上的一段：整条是这一轮从第一个请求发出到最后一个结束，亮的那段是这次请求。
fn timeline_bar(row: &TurnRow, r: &RequestLog) -> Div {
    let span = row.elapsed_ms.max(1) as f32;
    let start = (r.at_ms.saturating_sub(row.at_ms) as f32 / span).clamp(0., 1.);
    let width = (r.total_ms as f32 / span).clamp(0., 1. - start);
    let color = if r.cancelled {
        INK_FAINT
    } else if requests::failed(r) {
        RED
    } else if r.purpose == "subagent" {
        ChatApp::heat_color(2)
    } else {
        ChatApp::heat_color(4)
    };
    // 和右边第一行字的中线对齐
    div()
        .flex_shrink_0()
        .w(rems(6.))
        .mt(rems(0.4))
        .h(px(6.))
        .rounded_full()
        .bg(rgb(ChatApp::heat_color(0)))
        .h_flex()
        .child(div().w(relative(start)))
        .child(div().w(relative(width)).min_w(px(3.)).h_full().rounded_full().bg(rgb(color)))
}

/// 一次请求：用途、状态（失败标红）、第几次尝试、首字 / 全程、用量、费用（峰价标出来）。
fn request_line(
    k: usize,
    r: &RequestLog,
    before: &[RequestLog],
    prices: &[Price],
    peaks: &[PeakWindow],
) -> Div {
    let mut parts = vec![format!("{}. {}", k + 1, requests::purpose_label(&r.purpose))];
    if r.attempt.is_some_and(|a| a > 1) {
        parts.push(format!("第 {} 次尝试", r.attempt.unwrap_or(1)));
    }
    if let Some(ms) = r.first_token_ms {
        parts.push(format!("首字 {}", requests::ms_label(ms)));
    }
    parts.push(format!("共 {}", requests::ms_label(r.total_ms)));
    if r.input > 0 || r.output > 0 {
        let cached = if r.cache_read > 0 {
            format!("（缓存 {}）", usage::compact(r.cache_read))
        } else {
            String::new()
        };
        parts.push(format!("↑ {}{cached} ↓ {}", usage::compact(r.input), usage::compact(r.output)));
    }
    if let Some(m) = r.model.as_deref()
        && let Some(p) = pricing::find(prices, m)
        && (r.input > 0 || r.output > 0)
    {
        let cost = pricing::request_cost(p, r, peaks);
        let peak = match pricing::peak_of(p, peaks, r) {
            Some(true) => "（峰价）",
            _ => "",
        };
        parts.push(format!("约 {}{peak}", pricing::money(cost, p.currency)));
    }
    // 模型中途换过才标出来
    if let Some(m) = &r.model
        && before.last().is_some_and(|p| p.model.as_ref() != Some(m))
    {
        parts.push(m.clone());
    }
    let mut parts: Vec<(String, Color)> = parts.into_iter().map(|p| (p, INK_SOFT)).collect();
    if r.cancelled {
        parts.push(("已停止".into(), INK_SOFT));
    } else if requests::failed(r) {
        let status =
            if r.status == 0 { "连不上".to_string() } else { format!("HTTP {}", r.status) };
        parts.push((
            match &r.error {
                Some(e) => format!("{status}：{e}"),
                None => status,
            },
            RED,
        ));
    }
    meta_line(parts).text_size(TEXT_META)
}

/// 用「·」隔开的一行小字，放不下时折行；不是 `INK_SOFT` 的段（失败次数）单独着色。
fn meta_line(parts: Vec<(String, Color)>) -> Div {
    // 拼成一段文字、按段着色：每段一个元素再 flex_wrap 的话，taffy 每帧要把每段量好几遍，
    // 「请求」页签列 60 轮时每帧多花 10ms 以上
    let mut text = String::new();
    let mut highlights = Vec::new();
    for (i, (part, color)) in parts.into_iter().enumerate() {
        if i > 0 {
            text.push_str(" · ");
        }
        let start = text.len();
        text.push_str(&part);
        if color != INK_SOFT {
            let color: Hsla = rgb(color).into();
            highlights.push((
                start..text.len(),
                HighlightStyle { color: Some(color), ..Default::default() },
            ));
        }
    }
    div()
        .font_features(tabular_nums())
        .text_color(rgb(INK_SOFT))
        .child(StyledText::new(text).with_highlights(highlights))
}

/// 本地时间 `10-18 21:03`。
pub(super) fn time_label(ms: u64) -> String {
    chrono::DateTime::from_timestamp_millis(ms as i64)
        .map(|t| t.with_timezone(&chrono::Local).format("%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

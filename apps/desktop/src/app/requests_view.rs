//! 用量页的「请求明细」：用量网关记下的请求按「会话的第几轮」归组（归组与汇总见 `requests.rs`）。
//!
//! 日志在打开用量、换时间范围、点刷新时在后台读；点一轮展开看里面的每次请求（左边一条时间轴，
//! 看出每次请求落在这一轮的哪一段）。费用按设置里的模型单价估算（`pricing.rs`）。
//! 自研内核的请求都经过网关；ACP agent 的要在设置里打开「经过用量网关」、agent 认 base URL 才有。

use super::ChatApp;
use super::menu::{MenuChip, MenuList, menu_item, menu_popover};
use super::widgets::Surface;
use crate::model::StatCard;
use crate::pricing::{self, Cost, PeakWindow, Price};
use crate::requests::{self, Groups, SessionRef, TurnKey, TurnRow};
use crate::theme::rgb;
use crate::theme::*;
use crate::usage;
use claudesktop_builtin::gateway::{self, LogChange, LogCursor, RequestLog};
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

const REQUEST_CACHE_BUDGET: usize = 16 * 1024 * 1024;
const REQUEST_CACHE_IDLE: Duration = Duration::from_secs(30);

/// 先列多少轮；「显示更多」每次再多列这么多（首页整页滚动，不另开滚动框）
const PAGE_ROWS: usize = 20;

/// 视口上下各多排这么远（滚得快时边上不露白）
const OVERSCAN: Pixels = px(200.);

/// 「请求」页签的状态。
#[derive(Default)]
pub(super) struct RequestsView {
    /// 读到的日志；None = 还没读
    logs: Option<Vec<RequestLog>>,
    groups: Groups,
    cursor: LogCursor,
    /// 读的是哪个时间范围（`overview_range`）
    loaded_range: Option<usize>,
    loading: Option<Task<()>>,
    needs_reload: bool,
    reload_timer: Option<Task<()>>,
    hidden_since: Option<Instant>,
    /// 点开的轮（`TurnRow::key`）
    open: HashSet<TurnKey>,
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

impl RequestsView {
    fn reclaim_hidden(&mut self, now: Instant) -> Option<(usize, usize)> {
        let logs = self.logs.as_ref()?;
        let bytes = logs.capacity() * size_of::<RequestLog>()
            + self.groups.rows.capacity() * size_of::<TurnRow>();
        if bytes < REQUEST_CACHE_BUDGET {
            return None;
        }
        let hidden_since = *self.hidden_since.get_or_insert(now);
        if now.saturating_duration_since(hidden_since) < REQUEST_CACHE_IDLE
            || self.loading.is_some()
        {
            return None;
        }
        let rows = logs.len();
        self.logs = None;
        self.groups = Groups::default();
        self.cursor = LogCursor::default();
        self.derived.get_mut().take();
        *self.window.borrow_mut() = ListWindow::default();
        self.reload_timer = None;
        self.needs_reload = true;
        Some((rows, bytes))
    }
}

/// 列表的窗口化：首页整页滚动、列表在页面中间，用不了自带滚动框的虚拟列表，
/// 所以按首页的滚动位置算出视口附近是哪几轮，只排这几轮，上下用空白占住其余的高度
/// （「显示更多」点了几次之后，几百轮全排一遍，滚一格要上百毫秒）。
#[derive(Default)]
struct ListWindow {
    /// 列表顶端在滚动内容里的 y（画过一帧才知道）
    top: Option<Pixels>,
    /// 加载占位的 y。只用它判断列表是否还在视口及预排区下方。
    loading_top: Option<Pixels>,
    embedded: bool,
    /// 量高度时的列表宽度（变了量过的高度全作废）
    width: Pixels,
    /// 画过的轮的高度：[收起时, 展开时]
    heights: HashMap<TurnKey, [Option<Pixels>; 2]>,
    /// 没画过的收起的轮按这个估（画过的收起的轮的高度）
    estimate: Option<Pixels>,
}

impl ListWindow {
    fn select_page(&mut self, embedded: bool) {
        if self.embedded != embedded {
            self.embedded = embedded;
            self.top = None;
            self.loading_top = None;
        }
    }

    fn measure_top(
        state: Rc<RefCell<Self>>,
        scroll: ScrollHandle,
        loading: bool,
    ) -> impl IntoElement {
        canvas(
            move |bounds, window, _| {
                let top = bounds.origin.y - scroll.bounds().origin.y - scroll.offset().y;
                let mut w = state.borrow_mut();
                if loading {
                    w.loading_top = Some(top);
                } else {
                    w.loading_top = None;
                    if w.top.is_none_or(|t| (t - top).abs() > px(0.5)) {
                        w.top = Some(top);
                        window.request_animation_frame();
                    }
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .w_full()
        .h(px(0.))
    }

    /// 一轮的高度：量过的，或者估一个（展开的按每次请求一行估）。
    fn height(&self, row: &TurnRow, open: bool, line: Pixels) -> Pixels {
        let measured = self.heights.get(&row.key).and_then(|h| h[open as usize]);
        let collapsed = self.estimate.unwrap_or(line * 3.5);
        measured.unwrap_or(if open {
            collapsed + line * row.request_count() as f32 * 1.6
        } else {
            collapsed
        })
    }
}

/// 「请求」页签按当前日志、筛选和单价算好的数据（用量页的费用也从这里取）。
pub(super) struct Derived {
    key: DerivedKey,
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
        self.settings.usage_gateway
            && if self.settings_open {
                self.settings_form.tab == super::usage_view::USAGE_TAB
            } else {
                let session = self.session();
                self.overview_tab == 2
                    && self.space() == crate::settings::Space::Code
                    && session.history_loaded
                    && session.blocks_error.is_none()
                    && !session.has_user_message()
                    && !self.settings.needs_setup()
            }
    }

    pub(super) fn maintain_requests(&mut self, cx: &mut Context<Self>) {
        if self.requests_view.logs.is_none() && self.requests_view.hidden_since.is_none() {
            return;
        }
        if self.requests_shown() {
            if self.requests_view.hidden_since.is_some() {
                self.sync_requests(cx);
            }
        } else if let Some((rows, bytes)) = self.requests_view.reclaim_hidden(Instant::now())
            && std::env::var_os("CLAUDESKTOP_MEM_REPORT").is_some()
        {
            eprintln!("[mem] request_cache_evict rows={rows} estimated_bytes={bytes}");
        }
    }

    /// 在首页「请求」页签上或用量页开着、日志还没按当前范围读过：在后台读。
    pub(super) fn sync_requests(&mut self, cx: &mut Context<Self>) {
        // 用量页整页都要（费用、缓存命中也从请求日志算）
        if !self.requests_shown() {
            return;
        }
        let v = &mut self.requests_view;
        v.hidden_since = None;
        if v.loading.is_none() && (v.needs_reload || v.loaded_range != Some(self.overview_range)) {
            self.load_requests(false, cx);
        }
    }

    /// 列表所在的整页滚动区：首页，或用量页。
    fn list_scroll(&self) -> &ScrollHandle {
        if self.settings_open { &self.settings_scroll } else { &self.home_scroll }
    }

    fn load_requests(&mut self, from_start: bool, cx: &mut Context<Self>) {
        let v = &mut self.requests_view;
        if v.loading.is_some() {
            return;
        }
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
        let cursor = if !from_start && v.loaded_range == Some(range) {
            v.cursor.clone()
        } else {
            LogCursor::default()
        };
        v.needs_reload = false;
        v.loading = Some(cx.spawn(async move |this, cx| {
            let update =
                cx.background_executor().spawn(async move { cursor.read(&dir, since) }).await;
            let _ = this.update(cx, |app, cx| {
                let v = &mut app.requests_view;
                v.loading = None;
                if app.overview_range != range {
                    v.needs_reload = true;
                    app.sync_requests(cx);
                    cx.notify();
                    return;
                }
                let queued = v.needs_reload;
                let logs = v.logs.get_or_insert_with(Vec::new);
                match update {
                    Ok(update) => {
                        let (cursor, change) = update.apply(logs);
                        v.cursor = cursor;
                        match change {
                            LogChange::Unchanged => {}
                            LogChange::Appended { start } => v.groups.extend(logs, start),
                            LogChange::Rebuilt => {
                                v.groups = Groups::default();
                                v.groups.extend(logs, 0);
                            }
                        }
                        if change != LogChange::Unchanged {
                            v.derived.get_mut().take();
                            v.window.borrow_mut().heights.clear();
                        }
                        if v.loaded_range != Some(range) {
                            v.more = 0;
                        }
                        v.loaded_range = Some(range);
                    }
                    Err(err) => {
                        v.needs_reload = true;
                        app.toast(Notification::error(format!("读取请求记录失败：{err}")), cx);
                    }
                }
                if queued {
                    app.sync_requests(cx);
                }
                cx.notify();
            });
        }));
    }

    /// 网关又记下了请求（跑着的轮报了用量、一轮结束）：合批补读，页面没开就等下次打开。
    pub(super) fn refresh_requests_soon(&mut self, cx: &mut Context<Self>) {
        let v = &mut self.requests_view;
        if !self.settings.usage_gateway
            || (v.logs.is_none() && v.loading.is_none())
            || v.reload_timer.is_some()
        {
            return;
        }
        v.reload_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(std::time::Duration::from_secs(2)).await;
            let _ = this.update(cx, |app, cx| {
                app.requests_view.reload_timer = None;
                app.requests_view.needs_reload = true;
                app.sync_requests(cx);
            });
        }));
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
                    session: Some(format!("stress-{}", turn % 7).into()),
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
        let mut groups = Groups::default();
        groups.extend(&logs, 0);
        self.requests_view = RequestsView {
            logs: Some(logs),
            groups,
            loaded_range: Some(self.overview_range),
            more: if all { n / PAGE_ROWS } else { 0 },
            ..Default::default()
        };
    }

    /// 归组、筛选、汇总的结果，输入没变就用上次的（滚一格都要重画，日志多时每帧重算很贵）。
    /// `day`：只看那天（用量页的筛选；首页「请求」页签不按天筛）。
    fn derived_requests(&self, logs: &[RequestLog], day: Option<i64>) -> Rc<Derived> {
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
            && d.key == key
        {
            return d.clone();
        }
        v.derived.borrow_mut().take();
        let mut model_counts = HashMap::<&str, usize>::new();
        let mut session_tokens = HashSet::new();
        let mut sessions = Vec::new();
        let mut shown = Vec::new();
        let offset = key.day.map(|_| usage::local_offset_secs());
        for (i, row) in v.groups.newest_first() {
            let model = row.model(logs);
            let session = row.session(&key.names);
            if let Some(model) = model {
                *model_counts.entry(model).or_default() += 1;
            }
            if let Some(session) = session
                && session_tokens.insert(session.token)
            {
                sessions.push(session.clone());
            }
            if key.model.as_deref().is_none_or(|m| Some(m) == model)
                && key.session.is_none_or(|t| session.is_some_and(|s| s.token == t))
                && key
                    .day
                    .zip(offset)
                    .is_none_or(|(day, offset)| usage::day_of(row.at_ms, offset) == day)
            {
                shown.push(i);
            }
        }
        let mut models: Vec<_> = model_counts.into_iter().collect();
        models.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        let shown_rows: Vec<&TurnRow> = shown.iter().map(|&i| &v.groups.rows[i]).collect();
        let sum = requests::summarize(&shown_rows, logs);
        let requests::Costs { total: cost, models: model_cost, sessions: session_cost } =
            requests::costs(&shown_rows, logs, &key.names, &key.prices, &key.peaks);
        let (cache_read, input) =
            shown_rows.iter().fold((0, 0), |(c, i), r| (c + r.cache_read, i + r.input));
        let cache_hit = usage::cache_hit_percent(cache_read, input);
        let d = Rc::new(Derived {
            models: models.into_iter().map(|(m, _)| m.to_owned()).collect(),
            sessions,
            shown,
            sum,
            cost,
            model_cost,
            session_cost,
            cache_hit,
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
        let logs = self.requests_view.logs.as_deref()?;
        Some(self.derived_requests(logs, self.requests_view.day))
    }

    /// 请求列表。`embedded` = 用量页里的那份：筛选跟着整页走、总数在页顶，这里不再画筛选菜单和统计卡。
    pub(super) fn render_usage_requests(&self, embedded: bool, cx: &mut Context<Self>) -> Div {
        let hint = |text: &'static str| {
            div().py_6().text_size(TEXT_BODY).text_color(rgb(INK_SOFT)).child(text)
        };
        if !self.settings.usage_gateway {
            return div().child(hint(
                "用量网关关着，没有请求记录。在 设置 → 内核 里打开后，自研内核的每个请求都会记下来。",
            ));
        }
        let v = &self.requests_view;
        v.window.borrow_mut().select_page(embedded);
        let Some(logs) = v.logs.as_deref() else {
            return div()
                .relative()
                .child(ListWindow::measure_top(v.window.clone(), self.list_scroll().clone(), true))
                .child(hint("正在读取请求记录…"));
        };
        let d = self.derived_requests(logs, if embedded { v.day } else { None });
        let shown = &d.shown;
        let (sum, cost) = (&d.sum, &d.cost);
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
                    .loading(v.loading.is_some())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.load_requests(true, cx);
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
        list = self.request_rows(list, &shown[..shown.len().min(limit)], &d, logs, cx);
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
        shown: &[usize],
        data: &Derived,
        logs: &[RequestLog],
        cx: &mut Context<Self>,
    ) -> Div {
        let rows = &self.requests_view.groups.rows;
        let state = self.requests_view.window.clone();
        let rem = crate::theme::root_font_size();
        let (gap, line) = (rems(0.375).to_pixels(rem), TEXT_SMALL.to_pixels(rem));
        let scroll = self.list_scroll().clone();
        let viewport = scroll.bounds().size.height;
        // 滚过头（顶上再往上、底下再往下）时滚动区要到排版之后才把位置收回来，这里先自己收住，
        // 否则这一帧按越界的位置算，视口里一片空白
        let max = scroll.max_offset().y.max(px(0.));
        let scrolled = (-scroll.offset().y).clamp(px(0.), max);
        let bottom = scrolled + viewport + OVERSCAN;
        let (lo, hi) = {
            let w = state.borrow();
            match w.top.filter(|_| viewport > px(0.)) {
                Some(top) => (scrolled - top - OVERSCAN, bottom - top),
                None if viewport > px(0.) && w.loading_top.is_some_and(|top| top > bottom) => {
                    (px(0.), px(-1.))
                }
                None => (px(0.), viewport.max(px(900.)) + OVERSCAN),
            }
        };
        // 每轮占「高 + 行距」：before 是第一个排出来的那轮之前的，after 是最后一个之后的
        let (mut first, mut last) = (shown.len(), shown.len());
        let (mut before, mut after, mut y) = (px(0.), px(0.), px(0.));
        {
            let w = state.borrow();
            for (i, &index) in shown.iter().enumerate() {
                let row = &rows[index];
                let h = w.height(row, self.requests_view.open.contains(&row.key), line);
                if first == shown.len() && y + h >= lo {
                    first = i;
                    before = y;
                }
                if first < shown.len() && last == shown.len() && y > hi {
                    last = i;
                }
                if last < shown.len() {
                    after += h + gap;
                }
                y += h + gap;
            }
        }
        let keys: Vec<(TurnKey, bool)> = shown[first..last]
            .iter()
            .map(|&i| &rows[i])
            .map(|r| (r.key.clone(), self.requests_view.open.contains(&r.key)))
            .collect();
        // list 里排在这几轮前面的子元素：量位置的 canvas、上面的空白
        let skip = 1 + usize::from(before > gap);
        list.relative()
            .child(ListWindow::measure_top(state.clone(), scroll, false))
            // 空白和相邻的那轮之间还有一个行距，空白本身少算一个
            .when(before > gap, |d| d.child(div().flex_none().h(before - gap)))
            .children(
                shown[first..last]
                    .iter()
                    .map(|&i| self.request_turn(i, &rows[i], &data.key, logs, cx)),
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
        key: &DerivedKey,
        logs: &[RequestLog],
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let (prices, peaks) = (&key.prices, &key.peaks);
        let open = self.requests_view.open.contains(&row.key);
        let session = row.session(&key.names);
        let purpose = row.purpose(logs);
        let title = match session {
            Some(s) => s.title.clone(),
            None if purpose == "models" => "列出模型".into(),
            None if row.key.1 > 0 && !row.key.0.contains('#') => "已删除的会话".into(),
            None => requests::purpose_label(purpose).into(),
        };
        let mut parts = vec![format!("{} 次请求", row.request_count())];
        if row.input > 0 {
            parts.push(format!("累计输入 {}", usage::compact(row.input)));
        }
        // 只有一次请求时上下文就等于累计输入，不重复说
        if row.context > 0 && row.request_count() > 1 {
            parts.push(format!("上下文 {}", usage::compact(row.context)));
        }
        if row.output > 0 {
            parts.push(format!("输出 {}", usage::compact(row.output)));
        }
        if let Some(c) = Cost::of(prices, peaks, row.requests(logs)).label() {
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
                    .when_some(row.model(logs), |d, m| {
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
                                .child(SharedString::from(m.to_owned())),
                        )
                    })
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(TEXT_SMALL)
                            .text_color(rgb(INK_SOFT))
                            .font_features(tabular_nums())
                            .child(match session {
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
            let mut previous = None;
            for (k, r) in row.requests(logs).enumerate() {
                // 存了原文的：行尾一个「原文」，点开看发出去的请求体和返回（别让卡片跟着收起）
                let raw = r.body.as_ref().map(|id| {
                    let (id, model, at_ms) = (id.clone(), r.model.clone(), r.at_ms);
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
                        this.open_raw_request(&id, model.as_deref(), at_ms, window, cx);
                    }))
                });
                detail = detail.child(
                    div()
                        .h_flex()
                        .items_start()
                        .gap_2()
                        .child(timeline_bar(row, r))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(request_line(k, r, previous, prices, peaks)),
                        )
                        .children(raw.map(|b| div().flex_none().mt(rems(-0.125)).child(b))),
                );
                previous = Some(r);
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
    } else if r.purpose.as_ref() == "subagent" {
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
    previous: Option<&RequestLog>,
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
    if (r.input > 0 || r.output > 0)
        && let Some(m) = r.model.as_deref()
        && let Some(p) = pricing::find(prices, m)
    {
        let charge = pricing::charge(p, r, peaks);
        let peak = if charge.peak { "（峰价）" } else { "" };
        parts.push(format!("约 {}{peak}", pricing::money(charge.amount, charge.currency)));
    }
    // 模型中途换过才标出来
    if let Some(m) = &r.model
        && previous.is_some_and(|p| p.model.as_ref() != Some(m))
    {
        parts.push(m.to_string());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[core::prelude::v1::test]
    fn hidden_large_cache_is_dropped_without_discarding_view_state() {
        let mut logs = Vec::with_capacity(REQUEST_CACHE_BUDGET.div_ceil(size_of::<RequestLog>()));
        logs.push(RequestLog::default());
        let key = ("session".into(), 1);
        let mut view = RequestsView {
            logs: Some(logs),
            loaded_range: Some(2),
            day: Some(1),
            model: Some("example".into()),
            session: Some(7),
            more: 2,
            open: HashSet::from([key.clone()]),
            ..Default::default()
        };
        view.groups.extend(view.logs.as_deref().unwrap(), 0);
        view.window.borrow_mut().heights.insert(key.clone(), [Some(px(10.)), None]);
        let now = Instant::now();
        assert!(view.reclaim_hidden(now).is_none());
        assert!(view.reclaim_hidden(now + REQUEST_CACHE_IDLE - Duration::from_secs(1)).is_none());
        assert_eq!(view.reclaim_hidden(now + REQUEST_CACHE_IDLE).unwrap().0, 1);
        assert!(view.logs.is_none() && view.groups.rows.is_empty());
        assert!(view.window.borrow().heights.is_empty() && view.needs_reload);
        assert_eq!(
            (view.loaded_range, view.day, view.session, view.more),
            (Some(2), Some(1), Some(7), 2)
        );
        assert_eq!(view.model.as_deref(), Some("example"));
        assert!(view.open.contains(&key));
    }

    #[core::prelude::v1::test]
    fn hidden_small_cache_stays_available() {
        let mut view =
            RequestsView { logs: Some(vec![RequestLog::default()]), ..Default::default() };
        let now = Instant::now();
        assert!(view.reclaim_hidden(now).is_none());
        assert!(view.reclaim_hidden(now + REQUEST_CACHE_IDLE).is_none());
        assert_eq!(view.logs.unwrap().len(), 1);
    }
}

//! 用量页：一页从上往下看完——四个总数（费用、Token、轮数、缓存命中）、趋势图、模型和会话排行、
//! 星期 × 小时的热力、请求明细。时间范围和首页共用（`overview_range`）。
//!
//! 没有页签：点趋势图的一天、模型或会话的一行，是给整页加筛选（页顶列出来，点 × 去掉），
//! 筛选存在 `RequestsView` 的 `day` / `model` / `session` 上，请求明细和费用跟着同一份筛选走。
//! 轮数和 token 来自会话日志（`usage::board`，跑着的轮也算进去，见 `ChatApp::usage_stats`），
//! 费用和缓存命中来自用量网关的日志（`requests_view.rs` 的 `request_costs`，网关关着时没有）。
//!
//! 画在设置页的位置上（`settings_open` + `USAGE_TAB`，Esc、侧栏高亮都沿用），但不带设置的分类导航。

use super::ChatApp;
use super::settings_kit::seg_control;
use super::widgets::Surface;
use crate::pricing::Cost;
use crate::theme::rgb;
use crate::theme::*;
use crate::usage::{self, Board, Filter, WEEKDAYS};
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

/// 设置页里「用量」占的分类下标（不在导航里列，见 `render_settings`）
pub(super) const USAGE_TAB: usize = 6;

/// 会话排行先列多少行；「再显示」每次多列这么多
const PAGE_ROWS: usize = 6;

/// 模型排行最多列几个
const MODEL_ROWS: usize = 8;

/// 趋势图高度
const CHART_H: Rems = rems(8.);

/// 页顶的一个筛选标签：(元素 ID, 图标, 文字, 去掉它)
type FilterChip = (&'static str, IconAll, String, fn(&mut ChatApp));

/// 趋势图按什么画。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum Metric {
    #[default]
    Tokens,
    Turns,
}

/// 用量页的状态（筛选在 `RequestsView` 上）。
#[derive(Default)]
pub(super) struct UsagePage {
    /// 进用量页之前停在设置的哪个分类（从用量页去「设置」时回到那里）
    pub(super) settings_tab: usize,
    metric: Metric,
    /// 会话排行多列出来的页数
    more: usize,
}

impl ChatApp {
    /// 打开用量页，不带筛选（侧栏、命令面板、首页卡片）。
    pub(super) fn open_usage(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_usage(None, None, window, cx);
    }

    /// 打开用量页、只看某一天（首页热力图的格子）。
    pub(super) fn open_usage_day(&mut self, day: i64, window: &mut Window, cx: &mut Context<Self>) {
        self.show_usage(Some(day), None, window, cx);
    }

    /// 打开用量页、只看某个模型（首页「模型」页签的一行）。
    pub(super) fn open_usage_model(
        &mut self,
        model: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_usage(None, Some(model.to_string()), window, cx);
    }

    fn show_usage(
        &mut self,
        day: Option<i64>,
        model: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_usage_filter(day, model, None);
        if self.settings_form.tab != USAGE_TAB {
            self.usage_page.settings_tab = self.settings_form.tab;
        }
        // 设置页本来就开着：只换过去，别把表单里没保存的改动重置掉
        if self.settings_open {
            self.settings_form.tab = USAGE_TAB;
        } else {
            self.open_settings_at(USAGE_TAB, window, cx);
        }
        self.settings_scroll.set_offset(point(px(0.), px(0.)));
        self.sync_requests(cx);
        cx.notify();
    }

    /// 换整页的筛选（会话是 `Session::token`）。
    fn set_usage_filter(&mut self, day: Option<i64>, model: Option<String>, session: Option<u64>) {
        let v = &mut self.requests_view;
        (v.day, v.model, v.session) = (day, model, session);
        self.usage_filter_changed();
    }

    /// 筛选变了：列表都从第一页列起。
    fn usage_filter_changed(&mut self) {
        self.requests_view.more = 0;
        self.usage_page.more = 0;
    }

    /// 用量统计里的会话 ID（`TurnStat::session`）对应侧栏的第几个会话；删掉了的找不到。
    fn usage_session(&self, id: &str) -> Option<usize> {
        self.sessions.iter().position(|s| match &s.meta {
            Some(m) => m.id == id,
            None => id.strip_prefix("live-").and_then(|t| t.parse().ok()) == Some(s.token),
        })
    }

    /// 侧栏会话（`Session::token`）在用量统计里的 ID（同 `usage_stats`）。
    fn usage_session_id(&self, token: u64) -> Option<String> {
        let s = self.sessions.iter().find(|s| s.token == token)?;
        Some(s.meta.as_ref().map_or_else(|| format!("live-{token}"), |m| m.id.clone()))
    }

    fn range_days(&self) -> Option<u64> {
        match self.overview_range {
            1 => Some(30),
            2 => Some(7),
            _ => None,
        }
    }

    /// 整页：标题行（范围、关闭）固定，下面滚动。
    pub(super) fn render_usage_page(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let ranges = seg_control(
            "usage-range",
            [(0, "全部"), (1, "30 天"), (2, "7 天")],
            self.overview_range,
            |this, range, _, cx| {
                this.overview_range = range;
                this.usage_page.more = 0;
                this.sync_requests(cx);
                cx.notify();
            },
            cx,
        );
        let f = &self.settings_form;
        let content_w = f.content_w.get();
        div()
            .id("usage")
            // 焦点锚：点空白处也留在 ChatApp 键上下文里（Esc 关掉）
            .track_focus(&self.settings_focus)
            .flex_1()
            .min_h_0()
            .v_flex()
            .child(
                div()
                    .v_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .max_w(SETTINGS_NAV_W + FORM_W)
                    .mx_auto()
                    .child(
                        div()
                            .h_flex()
                            .items_center()
                            .gap_3()
                            .flex_none()
                            .px_8()
                            .pt_6()
                            .pb_4()
                            .child(
                                div()
                                    .flex_1()
                                    .text_size(TEXT_HEADING)
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(rgb(INK))
                                    .child("用量"),
                            )
                            .child(ranges)
                            .child(
                                Self::quiet_button("usage-close", IconAll::X, Surface::Paper, cx)
                                    .tooltip("关闭")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.settings_open = false;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .v_flex()
                            .flex_1()
                            .min_h_0()
                            .relative()
                            .child(
                                div()
                                    .id("usage-scroll")
                                    .track_scroll(&self.settings_scroll)
                                    .flex_1()
                                    .min_h_0()
                                    .overflow_y_scroll()
                                    .child(
                                        div()
                                            .v_flex()
                                            .gap_6()
                                            .px_8()
                                            .pb_10()
                                            // 内容列定宽，滚动时不用每帧把整页来回排几遍（见 `MeasuredWidth`）
                                            .when_some(content_w, |d, w| d.w(w))
                                            .child(self.render_usage_body(cx)),
                                    ),
                            )
                            .child(f.content_w.probe()),
                    ),
            )
    }

    fn render_usage_body(&self, cx: &mut Context<Self>) -> Div {
        let now = super::now_ms();
        let offset = usage::local_offset_secs();
        let today = usage::day_of(now, offset);
        let v = &self.requests_view;
        let session_id = v.session.and_then(|t| self.usage_session_id(t));
        let filter =
            Filter { day: v.day, model: v.model.as_deref(), session: session_id.as_deref() };
        let b = usage::board(&self.usage_stats(), now, offset, self.range_days(), filter);
        let costs = self.request_costs();

        div()
            .v_flex()
            .gap_6()
            .children(self.render_usage_filters(today, cx))
            .child(self.render_usage_totals(&b, costs.as_deref()))
            .child(self.render_usage_trend(&b, today, cx))
            .child(
                div()
                    .h_flex()
                    .items_start()
                    .gap_4()
                    .child(self.render_model_rank(&b, costs.as_deref(), cx))
                    .child(self.render_session_rank(&b, costs.as_deref(), cx)),
            )
            .child(render_usage_heat(&b))
            .child(panel("请求明细", None, self.render_usage_requests(true, cx)))
    }

    /// 页顶的筛选标签：只看某天 / 某个模型 / 某个会话，点 × 去掉；没有筛选时不画。
    fn render_usage_filters(&self, today: i64, cx: &mut Context<Self>) -> Option<Div> {
        let v = &self.requests_view;
        let mut chips: Vec<FilterChip> = Vec::new();
        if let Some(day) = v.day {
            chips.push((
                "usage-filter-day",
                IconAll::Calendar,
                usage::date_label(day, today),
                |a| a.requests_view.day = None,
            ));
        }
        if let Some(m) = &v.model {
            chips.push(("usage-filter-model", IconAll::Cpu, m.clone(), |a| {
                a.requests_view.model = None
            }));
        }
        if let Some(token) = v.session {
            let title = self
                .sessions
                .iter()
                .find(|s| s.token == token)
                .map_or_else(|| "（已删除的会话）".to_string(), |s| s.title.to_string());
            chips.push(("usage-filter-session", IconAll::MessageSquare, title, |a| {
                a.requests_view.session = None
            }));
        }
        if chips.is_empty() {
            return None;
        }
        let many = chips.len() > 1;
        let mut row = div()
            .h_flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .child(div().text_size(TEXT_SMALL).text_color(rgb(INK_SOFT)).child("只看"));
        for (id, icon, label, clear) in chips {
            row = row.child(
                div()
                    .id(id)
                    .h_flex()
                    .items_center()
                    .gap_1p5()
                    .max_w(rems(20.))
                    .pl_2p5()
                    .pr_2()
                    .py_1()
                    .rounded_full()
                    .bg(rgb(SAND))
                    .cursor_pointer()
                    .hover(|d| d.bg(rgb(SELECTED)))
                    .tooltip(|window, cx| Tooltip::new("去掉这个筛选").build(window, cx))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        clear(this);
                        this.usage_filter_changed();
                        cx.notify();
                    }))
                    .child(Icon::new(icon).size_3p5().text_color(rgb(INK_SOFT)))
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(TEXT_SMALL)
                            .text_color(rgb(INK))
                            .child(label),
                    )
                    .child(Icon::new(IconAll::X).size_3().text_color(rgb(INK_SOFT))),
            );
        }
        Some(row.when(many, |d| {
            d.child(
                Self::quiet_button("usage-filter-clear", IconAll::X, Surface::Paper, cx)
                    .xsmall()
                    .label("全部去掉")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.set_usage_filter(None, None, None);
                        cx.notify();
                    })),
            )
        }))
    }

    /// 四个总数：费用（网关）、Token、轮数、缓存命中（网关），下面一句补充（涨跌、省了多少）。
    fn render_usage_totals(&self, b: &Board, costs: Option<&super::requests_view::Derived>) -> Div {
        let dash = || "—".to_string();
        let prev_label = match self.range_days() {
            Some(7) => "上 7 天",
            _ => "上 30 天",
        };
        let delta = |now: u64, before: u64| -> String {
            match (now, before) {
                (_, 0) if now == 0 => format!("和{prev_label}一样"),
                (_, 0) => format!("{prev_label}没有对话"),
                _ => {
                    let pct = (now as f64 - before as f64) / before as f64 * 100.;
                    match pct.round() as i64 {
                        0 => format!("和{prev_label}差不多"),
                        p if p > 0 => format!("比{prev_label} ↑{p}%"),
                        p => format!("比{prev_label} ↓{}%", -p),
                    }
                }
            }
        };
        let gateway = self.settings.usage_gateway;
        let no_requests = costs.is_some_and(|d| d.sum.requests == 0);
        let (cost_value, cost_note) = match costs.map(|d| &d.cost) {
            _ if !gateway => (dash(), "打开用量网关后估算".to_string()),
            None => (dash(), "正在读取请求记录…".to_string()),
            Some(_) if no_requests => (dash(), "没有请求记录".to_string()),
            Some(c) => {
                let note = match (c.saved_label(), c.unpriced.as_slice()) {
                    (_, [one]) => format!("{one} 没有单价"),
                    (_, many) if many.len() > 1 => format!("{} 个模型没有单价", many.len()),
                    (Some(saved), _) => format!("缓存省下约 {saved}"),
                    _ => "按设置里的单价估算".to_string(),
                };
                (c.label().unwrap_or_else(dash), note)
            }
        };
        let tokens_note = b.prev.map_or_else(
            || format!("{} 天有对话", b.active_days),
            |(_, tokens)| delta(b.tokens, tokens),
        );
        let turns_note = match b.prev {
            Some((turns, _)) => {
                format!("{} 个会话 · {}", b.sessions, delta(b.turns as u64, turns as u64))
            }
            None => format!("{} 个会话", b.sessions),
        };
        let cache = match costs.and_then(|d| d.cache_hit) {
            Some(p) => (format!("{p}%"), "对话请求的输入里读缓存的比例".to_string()),
            None if !gateway => (dash(), "打开用量网关后统计".to_string()),
            None if costs.is_none() => (dash(), "正在读取请求记录…".to_string()),
            None if no_requests => (dash(), "没有请求记录".to_string()),
            None => (dash(), "端点没报缓存用量".to_string()),
        };
        let cards = [
            ("估算费用", cost_value, cost_note),
            ("Token", usage::compact(b.tokens), tokens_note),
            ("轮数", usage::compact(b.turns as u64), turns_note),
            ("缓存命中", cache.0, cache.1),
        ];
        div().h_flex().items_stretch().gap_3().children(cards.into_iter().map(
            |(label, value, note)| {
                div()
                    .v_flex()
                    .gap_1()
                    .flex_1()
                    .min_w_0()
                    .p_4()
                    .rounded_xl()
                    .bg(rgb(SAND))
                    .child(div().text_size(TEXT_SMALL).text_color(rgb(INK_SOFT)).child(label))
                    .child(
                        div()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(TEXT_HEADING)
                            .font_weight(FontWeight::SEMIBOLD)
                            .font_features(tabular_nums())
                            .text_color(rgb(INK))
                            .child(value),
                    )
                    .child(
                        div()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(TEXT_META)
                            .text_color(rgb(INK_SOFT))
                            .child(note),
                    )
            },
        ))
    }

    /// 趋势：每天（跨度长时每周）一根柱子，按 token 或轮数；点一天只看那天，再点一次去掉。
    fn render_usage_trend(&self, b: &Board, today: i64, cx: &mut Context<Self>) -> Div {
        let metric = self.usage_page.metric;
        let switch = seg_control(
            "usage-metric",
            [(Metric::Tokens, "Token"), (Metric::Turns, "轮数")],
            metric,
            |this, m, _, cx| {
                this.usage_page.metric = m;
                cx.notify();
            },
            cx,
        );
        let value = |d: &usage::DayRow| match metric {
            Metric::Tokens => d.tokens,
            Metric::Turns => d.turns as u64,
        };
        let title = if b.weekly { "每周" } else { "每天" };
        if b.series.iter().all(|d| d.turns == 0) {
            return panel(title, Some(switch.into_any_element()), empty("这段时间还没有对话"));
        }
        let max = b.series.iter().map(value).max().unwrap_or(0).max(1);
        let picked = self.requests_view.day;
        // 柱子多（30 天、按周的全部）时缝窄一点
        let gap = if b.series.len() > 40 { px(1.) } else { px(3.) };
        let mut bars = div().h_flex().items_end().gap(gap).h(CHART_H).w_full();
        for (i, d) in b.series.iter().enumerate() {
            let n = value(d);
            let day = d.day;
            let when = if b.weekly {
                format!("{} 起的一周", usage::date_label(day, today))
            } else {
                usage::date_label(day, today)
            };
            let tip: SharedString = if d.turns == 0 {
                format!("{when} · 没有对话")
            } else {
                format!("{when} · {} 轮 · {} token", d.turns, usage::compact(d.tokens))
            }
            .into();
            let color = match picked {
                Some(p) if p == day => CORAL,
                Some(_) => Self::heat_color(1),
                None => Self::heat_color(3),
            };
            let bar = div()
                .w_full()
                .rounded_sm()
                .bg(rgb(if n == 0 { Self::heat_color(0) } else { color }))
                // 没有对话的日子也留一道底线，看得出是 0 而不是没画
                .h(if n == 0 { px(2.).into() } else { relative(n as f32 / max as f32) });
            let column = div()
                .id(("usage-trend", i))
                .flex_1()
                .h_full()
                .v_flex()
                .justify_end()
                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                .child(bar);
            // 按周画的不能点（筛选按天）
            bars = bars.child(if b.weekly || day > today {
                column
            } else {
                column.cursor_pointer().on_click(cx.listener(move |this, _, _, cx| {
                    let v = &mut this.requests_view;
                    v.day = (v.day != Some(day)).then_some(day);
                    this.usage_filter_changed();
                    cx.notify();
                }))
            });
        }
        // 横轴只标头、中、尾三个日子
        let label = |d: Option<&usage::DayRow>| {
            div()
                .text_size(TEXT_META)
                .text_color(rgb(INK_SOFT))
                .font_features(tabular_nums())
                .children(d.map(|d| short_date(d.day, today)))
        };
        let axis = div()
            .h_flex()
            .justify_between()
            .child(label(b.series.first()))
            .child(label(b.series.get(b.series.len() / 2)))
            .child(label(b.series.last()));
        let total = match metric {
            Metric::Tokens => format!("峰值 {} token", usage::compact(max)),
            Metric::Turns => format!("峰值 {max} 轮"),
        };
        panel(
            title,
            Some(
                div()
                    .h_flex()
                    .items_center()
                    .gap_3()
                    .child(div().text_size(TEXT_SMALL).text_color(rgb(INK_SOFT)).child(total))
                    .child(switch)
                    .into_any_element(),
            ),
            div().v_flex().gap_1().child(bars).child(axis),
        )
    }

    /// 模型排行（按轮数）：轮数、token、占比、费用；点一行只看这个模型，再点一次去掉。
    fn render_model_rank(
        &self,
        b: &Board,
        costs: Option<&super::requests_view::Derived>,
        cx: &mut Context<Self>,
    ) -> Div {
        if b.models.is_empty() {
            return panel("模型", None, empty("没有记录到模型（ACP agent 不上报时不计）"))
                .flex_1()
                .min_w_0();
        }
        let max = b.models.first().map_or(1, |m| m.turns.max(1));
        let total = b.tokens.max(1);
        let picked = self.requests_view.model.clone();
        let mut list = div().v_flex().gap_2();
        for (i, m) in b.models.iter().take(MODEL_ROWS).enumerate() {
            let mut meta = vec![
                format!("{} 轮", usage::compact(m.turns as u64)),
                format!("{}%", (m.tokens * 100).div_ceil(total)),
            ];
            if let Some(c) = costs.and_then(|d| d.model_cost.get(&m.name)).and_then(Cost::label) {
                meta.push(c);
            }
            let name = m.name.clone();
            let on = picked.as_ref() == Some(&m.name);
            list = list.child(
                bar_row(m.name.clone(), meta.join(" · "), m.turns as f32 / max as f32, on, None)
                    .id(("usage-model", i))
                    .cursor_pointer()
                    .tooltip(move |window, cx| {
                        Tooltip::new(if on { "去掉这个筛选" } else { "只看这个模型" })
                            .build(window, cx)
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let v = &mut this.requests_view;
                        v.model = (v.model.as_ref() != Some(&name)).then(|| name.clone());
                        this.usage_filter_changed();
                        cx.notify();
                    })),
            );
        }
        panel("模型", None, list).flex_1().min_w_0()
    }

    /// 会话排行（按 token）：点一行只看这个会话，右边的箭头打开它。
    fn render_session_rank(
        &self,
        b: &Board,
        costs: Option<&super::requests_view::Derived>,
        cx: &mut Context<Self>,
    ) -> Div {
        let rows = &b.session_rows;
        if rows.is_empty() {
            return panel("会话", None, empty("这段时间还没有对话")).flex_1().min_w_0();
        }
        let limit = PAGE_ROWS * (self.usage_page.more + 1);
        let max = rows.first().map_or(1, |r| r.tokens.max(1));
        let picked = self.requests_view.session;
        let mut list = div().v_flex().gap_2();
        for (i, r) in rows.iter().take(limit).enumerate() {
            let found = self.usage_session(&r.session);
            let token = found.map(|ix| self.sessions[ix].token);
            let title = found.map_or_else(
                || "（已删除的会话）".to_string(),
                |ix| self.sessions[ix].title.to_string(),
            );
            let mut meta = vec![
                format!("{} 轮", usage::compact(r.turns as u64)),
                format!("{} token", usage::compact(r.tokens)),
            ];
            if let Some(c) =
                token.and_then(|t| costs.and_then(|d| d.session_cost.get(&t))).and_then(Cost::label)
            {
                meta.push(c);
            }
            let on = token.is_some() && picked == token;
            let open = found.map(|ix| {
                Self::quiet_button(
                    ("usage-session-open", i),
                    IconAll::ArrowUpRight,
                    Surface::Paper,
                    cx,
                )
                .xsmall()
                .tooltip("打开这个会话")
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.activate_session(ix, cx)
                }))
                .into_any_element()
            });
            let mut row = bar_row(title, meta.join(" · "), r.tokens as f32 / max as f32, on, open)
                .id(("usage-session", i));
            if let Some(token) = token {
                row = row
                    .cursor_pointer()
                    .tooltip(move |window, cx| {
                        Tooltip::new(if on { "去掉这个筛选" } else { "只看这个会话" })
                            .build(window, cx)
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let v = &mut this.requests_view;
                        v.session = (v.session != Some(token)).then_some(token);
                        this.usage_filter_changed();
                        cx.notify();
                    }));
            }
            list = list.child(row);
        }
        if rows.len() > limit {
            list = list.child(
                div().h_flex().justify_center().pt_1().child(
                    Self::quiet_button("usage-more", IconAll::ChevronDown, Surface::Sand, cx)
                        .small()
                        .label(format!("再显示 {} 个", (rows.len() - limit).min(PAGE_ROWS)))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.usage_page.more += 1;
                            cx.notify();
                        })),
                ),
            );
        }
        panel("会话", None, list).flex_1().min_w_0()
    }
}

/// 时段：一行一个星期几、一格一个小时，颜色按轮数分 4 档（同首页热力图的色阶）。
fn render_usage_heat(b: &Board) -> Div {
    let max = b.heat.iter().flatten().copied().max().unwrap_or(0);
    if max == 0 {
        return panel("时段", None, empty("这段时间还没有对话"));
    }
    let (mut peak_day, mut peak_hour) = (0, 0);
    for (d, hours) in b.heat.iter().enumerate() {
        for (h, n) in hours.iter().enumerate() {
            if *n > b.heat[peak_day][peak_hour] {
                (peak_day, peak_hour) = (d, h);
            }
        }
    }
    let label_w = rems(2.5);
    let mut grid = div().v_flex().gap(px(3.)).w_full();
    for (d, hours) in b.heat.iter().enumerate() {
        let mut line = div().h_flex().items_center().gap(px(3.)).w_full().child(
            div()
                .w(label_w)
                .flex_none()
                .text_size(TEXT_META)
                .text_color(rgb(INK_SOFT))
                .child(WEEKDAYS[d]),
        );
        for (h, n) in hours.iter().enumerate() {
            let level = if *n == 0 { 0 } else { (n * 4).div_ceil(max).clamp(1, 4) };
            let tip: SharedString =
                format!("{}{} · {n} 轮", WEEKDAYS[d], usage::hour_label(h as u32)).into();
            line = line.child(
                div()
                    .id(("usage-heat", d * 24 + h))
                    .flex_1()
                    .h(px(14.))
                    .rounded_sm()
                    .bg(rgb(ChatApp::heat_color(level as u8)))
                    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx)),
            );
        }
        grid = grid.child(line);
    }
    let mut axis = div().h_flex().gap(px(3.)).w_full().child(div().w(label_w).flex_none());
    for h in 0..24 {
        axis = axis.child(
            div()
                .flex_1()
                .whitespace_nowrap()
                .text_size(TEXT_META)
                .text_color(rgb(INK_SOFT))
                .font_features(tabular_nums())
                .when(h % 6 == 0, |d| d.child(h.to_string())),
        );
    }
    let note = format!("最常在{}{}", WEEKDAYS[peak_day], usage::hour_label(peak_hour as u32));
    panel(
        "时段",
        Some(div().text_size(TEXT_SMALL).text_color(rgb(INK_SOFT)).child(note).into_any_element()),
        grid.child(axis),
    )
}

/// 一块沙色面板：标题（右边可放操作）+ 内容。
fn panel(title: &'static str, actions: Option<AnyElement>, body: impl IntoElement) -> Div {
    div()
        .v_flex()
        .gap_3()
        .p_5()
        .rounded_xl()
        .bg(rgb(SAND))
        .child(
            div()
                .h_flex()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .flex_1()
                        .text_size(TEXT_BODY)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(rgb(INK))
                        .child(title),
                )
                .children(actions),
        )
        .child(body)
}

/// 一行：标题 + 右边一句数字，下面一根条形（白卡片；选中的是筛选着的那个，换底色）。
fn bar_row(
    title: impl Into<SharedString>,
    meta: String,
    share: f32,
    on: bool,
    trailing: Option<AnyElement>,
) -> Div {
    div()
        .v_flex()
        .gap_1()
        .p_3()
        .rounded_lg()
        .bg(rgb(if on { SELECTED } else { CARD }))
        .when(!on, |d| d.hover(|d| d.bg(rgb(HOVER))))
        .child(
            div()
                .h_flex()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_size(TEXT_BODY)
                        .text_color(rgb(INK))
                        .child(title.into()),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text_size(TEXT_SMALL)
                        .text_color(rgb(INK_SOFT))
                        .font_features(tabular_nums())
                        .child(meta),
                )
                .children(trailing),
        )
        .child(bar(share))
}

/// 细条形：底色是热力图的空格色，填充是三档色。
fn bar(share: f32) -> Div {
    div().h(px(6.)).w_full().rounded_full().bg(rgb(ChatApp::heat_color(0))).child(
        div()
            .h_full()
            .w(relative(share.clamp(0., 1.)))
            .rounded_full()
            .bg(rgb(ChatApp::heat_color(3))),
    )
}

fn empty(text: &'static str) -> Div {
    div().py_6().text_size(TEXT_BODY).text_color(rgb(INK_SOFT)).child(text)
}

/// 横轴上的短日期：「10月14日」，不是今年的带年份（`date_label` 去掉星期）。
fn short_date(day: i64, today: i64) -> String {
    let label = usage::date_label(day, today);
    label.rsplit_once(' ').map_or(label.clone(), |(date, _)| date.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[core::prelude::v1::test]
    fn short_date_drops_the_weekday() {
        let today = usage::day_of(1_791_979_200_000, 0);
        assert_eq!(short_date(today, today), "10月14日");
        assert_eq!(short_date(today - 300, today), "2025年12月18日");
    }
}

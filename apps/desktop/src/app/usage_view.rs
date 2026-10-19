//! 设置页「用量」：首页用量卡片的详情页。概览、每天、时段、模型、会话、请求六栏，
//! 时间范围和首页共用（`overview_range`）。
//!
//! 首页上的统计卡、热力图格子、模型行和右上角的箭头都跳到这里对应的一栏（[`ChatApp::open_usage`]）；
//! 这里的条目再往下钻：某一天 → 那天的会话，会话 → 打开它 / 看它的请求，模型 → 看它的请求。
//! 汇总在 `usage.rs`（`breakdown`），「请求」一栏就是首页那个页签（`requests_view.rs`）。

use super::ChatApp;
use super::settings_kit::{section, seg_control};
use super::widgets::Surface;
use crate::model::StatCard;
use crate::theme::rgb;
use crate::theme::*;
use crate::usage::{self, Breakdown, SessionRow, WEEKDAYS};
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

/// 设置页「用量」分类的下标
pub(super) const USAGE_TAB: usize = 5;

/// 列表先列多少行；「再显示」每次多列这么多
const PAGE_ROWS: usize = 30;

/// 柱状图高度
const CHART_H: Rems = rems(7.);

/// 看哪一栏。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum UsageView {
    #[default]
    Overview,
    Days,
    Hours,
    Models,
    Sessions,
    Requests,
}

/// 「用量」页的状态。
#[derive(Default)]
pub(super) struct UsagePage {
    pub(super) view: UsageView,
    /// 「每天」里点开的那天（`usage::day_of`）
    pub(super) day: Option<i64>,
    /// 「模型」里展开的模型
    pub(super) model: Option<String>,
    /// 列表多列出来的页数
    more: usize,
}

impl ChatApp {
    /// 打开设置页「用量」的某一栏（首页卡片、热力图、侧栏入口共用）。
    pub(super) fn open_usage(
        &mut self,
        view: UsageView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let page = &mut self.usage_page;
        page.view = view;
        page.more = 0;
        if !(self.settings_open && self.settings_form.tab == USAGE_TAB) {
            self.open_settings_at(USAGE_TAB, window, cx);
        }
        self.settings_scroll.set_offset(point(px(0.), px(0.)));
        self.sync_requests(cx);
        cx.notify();
    }

    /// 看某一天（热力图格子、「每天」里的一行）。
    pub(super) fn open_usage_day(&mut self, day: i64, window: &mut Window, cx: &mut Context<Self>) {
        self.usage_page.day = Some(day);
        self.open_usage(UsageView::Days, window, cx);
    }

    /// 看某个模型（首页「模型」页签的一行）；在设置页里再点一次收起。
    pub(super) fn open_usage_model(
        &mut self,
        model: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let here = self.settings_open && self.usage_page.view == UsageView::Models;
        let same = self.usage_page.model.as_deref() == Some(model);
        self.usage_page.model = (!(here && same)).then(|| model.to_string());
        if here {
            cx.notify();
        } else {
            self.open_usage(UsageView::Models, window, cx);
        }
    }

    /// 用量统计里的会话 ID（`TurnStat::session`）对应侧栏的第几个会话；删掉了的找不到。
    fn usage_session(&self, id: &str) -> Option<usize> {
        self.sessions.iter().position(|s| match &s.meta {
            Some(m) => m.id == id,
            None => id.strip_prefix("live-").and_then(|t| t.parse().ok()) == Some(s.token),
        })
    }

    fn range_days(&self) -> Option<u64> {
        match self.overview_range {
            1 => Some(30),
            2 => Some(7),
            _ => None,
        }
    }

    pub(super) fn render_usage_page(&self, cx: &mut Context<Self>) -> Div {
        let page = &self.usage_page;
        let now = super::now_ms();
        let offset = usage::local_offset_secs();
        let range = self.range_days();
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
        let views = seg_control(
            "usage-view",
            [
                (UsageView::Overview, "概览"),
                (UsageView::Days, "每天"),
                (UsageView::Hours, "时段"),
                (UsageView::Models, "模型"),
                (UsageView::Sessions, "会话"),
                (UsageView::Requests, "请求"),
            ],
            page.view,
            |this, view, window, cx| {
                // 再点「每天」回到日子列表
                if view == UsageView::Days {
                    this.usage_page.day = None;
                }
                this.open_usage(view, window, cx)
            },
            cx,
        );

        let o = usage::overview(&self.usage, now, offset, range);
        let body = match page.view {
            UsageView::Overview => Self::render_usage_cards(&o, cx).child(
                div()
                    .text_size(TEXT_SMALL)
                    .text_color(rgb(INK_SOFT))
                    .child(usage::book_line(o.tokens)),
            ),
            UsageView::Days => {
                let today = usage::day_of(now, offset);
                match page.day {
                    Some(day) => {
                        let b = usage::breakdown(&self.usage, now, offset, None, Some(day));
                        self.render_usage_day(day, today, &b, cx)
                    }
                    None => {
                        let b = usage::breakdown(&self.usage, now, offset, range, None);
                        self.render_usage_days(today, &b, cx)
                    }
                }
            }
            UsageView::Hours => {
                let b = usage::breakdown(&self.usage, now, offset, range, None);
                render_usage_hours(&b)
            }
            UsageView::Models => Self::render_usage_models(&o, page.model.as_deref(), true, cx),
            UsageView::Sessions => {
                let b = usage::breakdown(&self.usage, now, offset, range, None);
                self.render_usage_sessions(&b.sessions, PAGE_ROWS * (page.more + 1), cx)
            }
            UsageView::Requests => self.render_usage_requests(cx),
        };

        section(
            "用量",
            "所有会话的对话轮数和 token，按天、时段、模型、会话细看",
            Some(ranges.into_any_element()),
            div()
                .v_flex()
                .gap_4()
                .child(div().h_flex().child(views))
                .child(div().v_flex().gap_4().p_5().rounded_xl().bg(rgb(SAND)).child(body)),
        )
    }

    /// 「每天」：有对话的日子，新的在前；条形按轮数。点一行看那天。
    fn render_usage_days(&self, today: i64, b: &Breakdown, cx: &mut Context<Self>) -> Div {
        if b.days.is_empty() {
            return empty("这段时间还没有对话");
        }
        let max = b.days.iter().map(|d| d.turns).max().unwrap_or(1).max(1);
        let limit = PAGE_ROWS * (self.usage_page.more + 1);
        let mut list = div().v_flex().gap_2();
        for d in b.days.iter().take(limit) {
            let day = d.day;
            let label = match today - day {
                0 => format!("今天 · {}", usage::date_label(day, today)),
                1 => format!("昨天 · {}", usage::date_label(day, today)),
                _ => usage::date_label(day, today),
            };
            list = list.child(
                bar_row(
                    label,
                    format!(
                        "{} 轮 · {} token · {} 个会话",
                        usage::compact(d.turns as u64),
                        usage::compact(d.tokens),
                        d.sessions
                    ),
                    d.turns as f32 / max as f32,
                    None,
                )
                .id(("usage-day", day as u64))
                .cursor_pointer()
                .hover(|s| s.bg(rgb(HOVER)))
                .on_click(
                    cx.listener(move |this, _, window, cx| this.open_usage_day(day, window, cx)),
                ),
            );
        }
        list.when(b.days.len() > limit, |d| d.child(self.more_button(b.days.len() - limit, cx)))
    }

    /// 某一天：三张统计卡、那天各小时的轮数、那天的会话。
    fn render_usage_day(&self, day: i64, today: i64, b: &Breakdown, cx: &mut Context<Self>) -> Div {
        let (turns, tokens) = b.days.first().map_or((0, 0), |d| (d.turns, d.tokens));
        let header = div()
            .h_flex()
            .items_center()
            .gap_2()
            .child(
                Self::quiet_button("usage-day-back", IconAll::ArrowLeft, Surface::Sand, cx)
                    .small()
                    .tooltip("回到全部日子")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.usage_page.day = None;
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .flex_1()
                    .text_size(TEXT_LEAD)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(INK))
                    .child(usage::date_label(day, today)),
            )
            .child(
                Self::quiet_button("usage-day-prev", IconAll::ChevronLeft, Surface::Sand, cx)
                    .small()
                    .tooltip("前一天")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.usage_page.day = Some(day - 1);
                        cx.notify();
                    })),
            )
            .child(
                Self::quiet_button("usage-day-next", IconAll::ChevronRight, Surface::Sand, cx)
                    .small()
                    .tooltip("后一天")
                    .disabled(day >= today)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.usage_page.day = Some(day + 1);
                        cx.notify();
                    })),
            );
        if turns == 0 {
            return div().v_flex().gap_4().child(header).child(empty("这天没有对话"));
        }
        let stats = [
            StatCard { label: "轮数", value: usage::compact(turns as u64).into() },
            StatCard { label: "Token", value: usage::compact(tokens).into() },
            StatCard { label: "会话", value: b.sessions.len().to_string().into() },
        ];
        div()
            .v_flex()
            .gap_4()
            .child(header)
            .child(div().h_flex().gap_3().children(stats.iter().map(Self::stat_card)))
            .child(caption("各时段"))
            .child(hour_chart(&b.hours))
            .child(caption("会话"))
            .child(self.render_usage_sessions(&b.sessions, b.sessions.len(), cx))
    }

    /// 会话列表（按 token 从多到少，列前 `limit` 个）：点一行打开会话，右上角的按钮看它的请求。
    fn render_usage_sessions(
        &self,
        rows: &[SessionRow],
        limit: usize,
        cx: &mut Context<Self>,
    ) -> Div {
        if rows.is_empty() {
            return empty("这段时间还没有对话");
        }
        let max = rows.first().map_or(1, |r| r.tokens.max(1));
        let mut list = div().v_flex().gap_2();
        for (i, r) in rows.iter().take(limit).enumerate() {
            let found = self.usage_session(&r.session);
            let title = found.map_or_else(
                || "（已删除的会话）".to_string(),
                |ix| self.sessions[ix].title.to_string(),
            );
            let mut meta = vec![
                format!("{} 轮", usage::compact(r.turns as u64)),
                format!("{} token", usage::compact(r.tokens)),
            ];
            meta.push(super::requests_view::time_label(r.last_ms));
            let token = found.map(|ix| self.sessions[ix].token);
            let filter = token.map(|token| {
                Self::quiet_button(
                    ("usage-session-req", i),
                    IconAll::ListFilter,
                    Surface::Paper,
                    cx,
                )
                .xsmall()
                .tooltip("看这个会话的请求")
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.filter_requests(None, Some(token));
                    this.open_usage(UsageView::Requests, window, cx)
                }))
                .into_any_element()
            });
            let mut row = bar_row(title, meta.join(" · "), r.tokens as f32 / max as f32, filter)
                .id(("usage-session", i));
            if let Some(ix) = found {
                row = row
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(HOVER)))
                    .on_click(cx.listener(move |this, _, _, cx| this.activate_session(ix, cx)));
            }
            list = list.child(row);
        }
        list.when(rows.len() > limit, |d| d.child(self.more_button(rows.len() - limit, cx)))
    }

    fn more_button(&self, left: usize, cx: &mut Context<Self>) -> Div {
        div().h_flex().justify_center().pt_1().child(
            Self::quiet_button("usage-more", IconAll::ChevronDown, Surface::Sand, cx)
                .small()
                .label(format!("再显示 {} 条", left.min(PAGE_ROWS)))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.usage_page.more += 1;
                    cx.notify();
                })),
        )
    }
}

/// 「时段」：一天 24 小时和一周 7 天各自的轮数。
fn render_usage_hours(b: &Breakdown) -> Div {
    if b.hours.iter().all(|n| *n == 0) {
        return empty("这段时间还没有对话");
    }
    let peak =
        (0..24u32).max_by_key(|h| (b.hours[*h as usize], std::cmp::Reverse(*h))).unwrap_or(0);
    let max = b.weekdays.iter().copied().max().unwrap_or(1).max(1);
    let mut week = div().v_flex().gap_2();
    for (i, n) in b.weekdays.iter().enumerate() {
        week = week.child(
            div()
                .h_flex()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .w(rems(2.5))
                        .flex_none()
                        .text_size(TEXT_SMALL)
                        .text_color(rgb(INK_SOFT))
                        .child(WEEKDAYS[i]),
                )
                .child(div().flex_1().child(bar(*n as f32 / max as f32)))
                .child(
                    div()
                        .w(rems(4.))
                        .flex_none()
                        .text_right()
                        .text_size(TEXT_SMALL)
                        .text_color(rgb(INK_SOFT))
                        .font_features(tabular_nums())
                        .child(format!("{} 轮", usage::compact(*n as u64))),
                ),
        );
    }
    div()
        .v_flex()
        .gap_4()
        .child(caption(format!("一天里最常在{}对话", usage::hour_label(peak))))
        .child(hour_chart(&b.hours))
        .child(caption("一周里"))
        .child(week)
}

/// 24 根竖条（本地时间每小时的轮数），最高的那根用强调色；悬停看具体数。
fn hour_chart(hours: &[usize; 24]) -> Div {
    let max = hours.iter().copied().max().unwrap_or(0).max(1);
    let peak = hours.iter().position(|n| *n == max);
    let mut bars = div().h_flex().items_end().gap(px(3.)).h(CHART_H).w_full();
    let mut axis = div().h_flex().gap(px(3.)).w_full();
    for (h, n) in hours.iter().enumerate() {
        let tip: SharedString = format!("{} · {n} 轮", usage::hour_label(h as u32)).into();
        let color = if peak == Some(h) && *n > 0 { CORAL } else { ChatApp::heat_color(2) };
        bars = bars.child(
            div()
                .id(("hour-bar", h))
                .flex_1()
                .h_full()
                .v_flex()
                .justify_end()
                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                .child(
                    div()
                        .w_full()
                        .rounded_sm()
                        .bg(rgb(if *n == 0 { ChatApp::heat_color(0) } else { color }))
                        // 没有对话的小时也留一道底线，看得出是 0 而不是没画
                        .h(if *n == 0 { px(2.).into() } else { relative(*n as f32 / max as f32) }),
                ),
        );
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
    div().v_flex().gap_1().child(bars).child(axis)
}

/// 一行：标题 + 右边一句数字，下面一根条形（白卡片；可点的由调用方加 id 和点击）。
fn bar_row(
    title: impl Into<SharedString>,
    meta: String,
    share: f32,
    trailing: Option<AnyElement>,
) -> Div {
    div()
        .v_flex()
        .gap_1()
        .p_3()
        .rounded_lg()
        .bg(rgb(CARD))
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

/// 小节标题（沙色面板里）。
fn caption(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(TEXT_SMALL)
        .font_weight(FontWeight::MEDIUM)
        .text_color(rgb(INK_SOFT))
        .child(text.into())
}

fn empty(text: &'static str) -> Div {
    div().py_6().text_size(TEXT_BODY).text_color(rgb(INK_SOFT)).child(text)
}

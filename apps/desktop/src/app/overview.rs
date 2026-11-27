//! 空会话首页：问候语 + 用量概览（数据来自会话日志，见 usage.rs；卡片、格子、模型行点了进用量页，见 usage_view.rs）。

use super::ChatApp;
use super::settings_view::{kernel_icon, provider_icon};
use super::widgets::Surface;
use crate::model::*;
use crate::settings::Space;
use crate::theme::rgb;
use crate::theme::*;
use crate::usage::{self, Overview};
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::input::Position;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::time::{SystemTime, UNIX_EPOCH};

/// 输入框平时的占位文字（代码空间）
const PLACEHOLDER: &str = "描述要做的事，回车发送，Shift+回车换行";

/// 输入框平时的占位文字（聊天空间）
const CHAT_PLACEHOLDER: &str = "有什么想聊的？回车发送，Shift+回车换行";

impl ChatApp {
    /// 首页问候语：只保留紧凑的时段问候，不摆放品牌 Logo。
    fn render_greeting(&self) -> Div {
        div()
            .text_size(TEXT_TITLE)
            .font_weight(FontWeight::MEDIUM)
            .text_color(rgb(INK))
            .child(greeting())
    }

    /// 聊天接不上模型（没配自研内核的端点，只配了 ACP agent）时，按品牌选一个模型服务。
    fn render_chat_setup(&self, cx: &mut Context<Self>) -> Div {
        div().v_flex().gap_2p5().w_full().child(self.render_provider_logos(cx))
    }

    pub(super) fn heat_color(v: u8) -> Color {
        HEAT[(v as usize).min(HEAT.len() - 1)]
    }

    pub(super) fn stat_card(c: &StatCard) -> Div {
        div()
            .v_flex()
            .gap_1()
            // 等分整行（不按内容宽），两行卡片上下对齐成列
            .flex_1()
            .min_w_0()
            .p_3()
            .rounded_lg()
            .bg(rgb(CARD))
            .border_1()
            .border_color(rgb(LINE))
            .shadow(raised_shadow())
            .child(div().text_size(TEXT_SMALL).text_color(rgb(INK_SOFT)).child(c.label))
            .child(
                div()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(TEXT_LEAD)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(INK))
                    .child(c.value.clone()),
            )
    }

    /// 首页轻量用量摘要条：只留一行关键数字，完整热力图与排行点进侧栏「用量」页查看。
    fn render_usage_strip(&self, o: &Overview, cx: &mut Context<Self>) -> Stateful<Div> {
        let detail = if o.sessions == 0 && o.tokens == 0 {
            "还没有用量记录".to_string()
        } else {
            let mut text = format!(
                "{} 个会话 · {} token · 活跃 {} 天",
                usage::compact(o.sessions as u64),
                usage::compact(o.tokens),
                o.active_days
            );
            if let Some(m) = o.models.first() {
                text.push_str(&format!(" · 常用 {}", m.name));
            }
            text
        };
        div()
            .id("overview-usage-strip")
            .h_flex()
            .items_center()
            .gap_2p5()
            .w_full()
            .px_3p5()
            .py_2p5()
            .rounded_xl()
            .bg(rgb(SAND))
            .cursor_pointer()
            .hover(|d| d.bg(rgb(SELECTED)))
            .on_click(cx.listener(|this, _, window, cx| this.open_usage(window, cx)))
            .child(Icon::new(IconAll::ChartColumn).size_4().flex_shrink_0().text_color(rgb(CORAL)))
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(TEXT_SMALL)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(rgb(INK))
                    .child("最近用量"),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(TEXT_SMALL)
                    .text_color(rgb(INK_SOFT))
                    .font_features(tabular_nums())
                    .child(detail),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .h_flex()
                    .items_center()
                    .gap_1()
                    .text_size(TEXT_SMALL)
                    .text_color(rgb(INK_SOFT))
                    .child("详情")
                    .child(Icon::new(IconAll::ArrowUpRight).size_3p5()),
            )
    }

    fn tab(
        &self,
        id: &'static str,
        label: &'static str,
        active: bool,
        cx: &mut Context<Self>,
        set: fn(&mut Self),
    ) -> Stateful<Div> {
        div()
            .id(id)
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                set(this);
                // 切到「请求」页签或换了范围：按需读网关日志
                this.sync_requests(cx);
                cx.notify();
            }))
            .child(Self::seg(label, active))
    }

    /// 新装还没配内核时，首页直接按品牌选择；点了进入该服务商或 agent 的配置。
    fn render_setup(&self, cx: &mut Context<Self>) -> Div {
        div().v_flex().gap_4().w_full().child(self.render_provider_logos(cx)).child(
            div()
                .v_flex()
                .gap_2()
                .w_full()
                .child(
                    div()
                        .px_1()
                        .text_size(TEXT_SMALL)
                        .text_color(rgb(INK_SOFT))
                        .child("本机 agent"),
                )
                .child(div().h_flex().gap_3().w_full().children(
                    claudesktop_core::agents::PRESETS.iter().map(|preset| {
                        let id = preset.id;
                        Self::setup_tile(
                            ElementId::Name(format!("setup-agent-{id}").into()),
                            kernel_icon(id),
                            preset.name,
                        )
                        .on_click(cx.listener(
                            move |this, _, window, cx| this.start_setup_agent(id, window, cx),
                        ))
                    }),
                )),
        )
    }

    fn render_provider_logos(&self, cx: &mut Context<Self>) -> Div {
        let tiles = crate::settings::preset_profiles()
            .into_iter()
            .enumerate()
            .map(|(i, p)| {
                let id = crate::assets::provider_logo_for_url(&p.base_url).unwrap_or("custom");
                let name = provider_name(&p.name);
                Self::setup_tile(
                    ElementId::Name(format!("setup-provider-{id}").into()),
                    provider_icon(&p.base_url),
                    name,
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.start_setup_provider(Some(i), window, cx)
                }))
            })
            .chain(std::iter::once(
                Self::setup_tile("setup-provider-custom", Icon::new(IconAll::Plus), "自定义")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.start_setup_provider(None, window, cx)
                    })),
            ))
            .collect::<Vec<_>>();
        let mut grid = div().v_flex().gap_3().w_full();
        let mut tiles = tiles.into_iter();
        loop {
            let row: Vec<_> = tiles.by_ref().take(4).collect();
            if row.is_empty() {
                break;
            }
            grid = grid.child(div().h_flex().gap_3().w_full().children(row));
        }
        grid
    }

    fn setup_tile(id: impl Into<ElementId>, icon: Icon, name: &'static str) -> Stateful<Div> {
        div()
            .id(id)
            .v_flex()
            .items_center()
            .justify_center()
            .gap_2()
            .flex_1()
            .min_w_0()
            .py_4()
            .rounded_lg()
            .bg(rgb(CARD))
            .border_1()
            .border_color(rgb(LINE))
            .shadow(raised_shadow())
            .cursor_pointer()
            .hover(|d| d.border_color(rgb(LINE_STRONG)))
            .child(icon.size_6().flex_shrink_0().text_color(rgb(INK)))
            .child(
                div()
                    .w_full()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_center()
                    .text_size(TEXT_SMALL)
                    .text_color(rgb(INK))
                    .child(name),
            )
    }

    pub(super) fn render_overview(&self, cx: &mut Context<Self>) -> Div {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
        let range = match self.overview_range {
            1 => Some(30),
            2 => Some(7),
            _ => None,
        };
        let o = usage::overview(&self.usage_stats(), now, usage::local_offset_secs(), range);
        let needs_setup = self.settings.needs_setup();
        let setup = needs_setup.then(|| self.render_setup(cx));
        // 聊天空间：问候语 + 最近会话，**不放用量概览**（用量在侧栏「用量」里）。
        // 聊天接不上模型（只配了 ACP agent）时换成一张引导卡
        let chat = self.space() == Space::Chat;
        if chat {
            let can_chat = self.settings.chat_available();
            return div()
                .v_flex()
                .flex_1()
                .min_h_0()
                .items_start()
                .justify_center()
                .gap_6()
                .child(self.render_greeting())
                .children((!can_chat).then(|| self.render_chat_setup(cx)))
                .children(self.render_recent(cx));
        }

        div()
            .v_flex()
            .flex_1()
            .min_h_0()
            .items_start()
            .justify_center()
            .gap_6()
            .child(self.render_greeting())
            .children(setup)
            .children((!needs_setup).then(|| self.render_recent(cx)).flatten())
            .when(!needs_setup, |d| {
                // `CLAUDESKTOP_STRESS_VIEW=requests` 压测钩子显式切到 tab 2 时仍画请求明细面板
                if self.overview_tab == 2 {
                    d.child(
                        div()
                            .v_flex()
                            .gap_4()
                            .w_full()
                            .p_5()
                            .rounded_xl()
                            .bg(rgb(SAND))
                            .child(
                                div()
                                    .h_flex()
                                    .items_center()
                                    .child(
                                        div()
                                            .text_size(TEXT_BODY)
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(rgb(INK))
                                            .child("请求明细"),
                                    )
                                    .child(div().flex_1())
                                    .child(
                                        div()
                                            .h_flex()
                                            .gap_1()
                                            .child(self.tab(
                                                "range-all",
                                                "全部",
                                                self.overview_range == 0,
                                                cx,
                                                |t| t.overview_range = 0,
                                            ))
                                            .child(self.tab(
                                                "range-30",
                                                "30 天",
                                                self.overview_range == 1,
                                                cx,
                                                |t| t.overview_range = 1,
                                            ))
                                            .child(self.tab(
                                                "range-7",
                                                "7 天",
                                                self.overview_range == 2,
                                                cx,
                                                |t| t.overview_range = 2,
                                            )),
                                    )
                                    .child(
                                        div().pl_2().child(
                                            Self::quiet_button(
                                                "usage-detail",
                                                IconAll::ArrowUpRight,
                                                Surface::Sand,
                                                cx,
                                            )
                                            .small()
                                            .tooltip("看用量详情")
                                            .on_click(
                                                cx.listener(|this, _, window, cx| {
                                                    this.open_usage(window, cx)
                                                }),
                                            ),
                                        ),
                                    ),
                            )
                            .child(self.render_usage_requests(false, cx)),
                    )
                } else {
                    d.child(self.render_usage_strip(&o, cx))
                }
            })
    }
}

/// 首页「最近会话」最多列几张
const RECENT_MAX: usize = 3;

impl ChatApp {
    /// 首页问候语下面一排「最近会话」：最近活动的几个（不含已归档和当前这个），点了切过去接着聊。
    /// 一个都没有（新装）时不画。
    fn render_recent(&self, cx: &mut Context<Self>) -> Option<Div> {
        let mut recent: Vec<(usize, u64)> = self
            .sessions
            .iter()
            .enumerate()
            .filter(|(i, s)| *i != self.active && !s.title.is_empty())
            .filter_map(|(i, s)| {
                let meta = s.meta.as_ref().filter(|m| !m.archived)?;
                Some((i, meta.updated_ms))
            })
            .collect();
        if recent.is_empty() {
            return None;
        }
        recent.sort_by_key(|r| std::cmp::Reverse(r.1));
        recent.truncate(RECENT_MAX);
        // 不满一排时补空位，卡片宽度不随个数变（只有一个时不撑满整行）
        let fillers = RECENT_MAX - recent.len();
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
        let cards = recent.into_iter().map(|(i, updated)| {
            let s = &self.sessions[i];
            let (icon, place): (IconAll, SharedString) = if s.task {
                (IconAll::ListTodo, "任务".into())
            } else {
                (IconAll::Folder, super::project_label(&s.cwd))
            };
            div()
                .id(("recent-session", i))
                .v_flex()
                .gap_1()
                // 等分整行，和下面的统计卡片对齐成列
                .flex_1()
                .min_w_0()
                .px_3()
                .py_2p5()
                .rounded_lg()
                .bg(rgb(CARD))
                .border_1()
                .border_color(rgb(LINE))
                .shadow(raised_shadow())
                .cursor_pointer()
                .hover(|d| d.border_color(rgb(LINE_STRONG)))
                .child(
                    div()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_size(TEXT_BODY)
                        .text_color(rgb(INK))
                        .child(s.title.clone()),
                )
                .child(
                    div()
                        .h_flex()
                        .gap_1()
                        .items_center()
                        .text_size(TEXT_META)
                        .text_color(rgb(INK_SOFT))
                        .child(Icon::new(icon).size_3().flex_shrink_0())
                        .child(div().min_w_0().overflow_hidden().text_ellipsis().child(place))
                        .child(div().flex_1())
                        .child(div().flex_shrink_0().child(ago(now, updated))),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.activate_session(i, cx)))
        });
        Some(
            div()
                .v_flex()
                .gap_2()
                .w_full()
                .child(
                    div().px_1().text_size(TEXT_SMALL).text_color(rgb(INK_SOFT)).child("最近会话"),
                )
                .child(
                    div()
                        .h_flex()
                        .gap_3()
                        .children(cards)
                        .children((0..fillers).map(|_| div().flex_1().min_w_0())),
                ),
        )
    }

    /// 输入框的占位文字：跑着时说明回车会排队，刚拒绝了权限时请用户说要怎么改，
    /// 其余是操作提示。变了才写进输入框（每帧调一次）。
    ///
    /// 「猜的下一句」画在输入框里时占位文字让开（留空）：两句淡色字叠在同一行没法读。
    pub(super) fn sync_placeholder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let s = self.session();
        let text: SharedString = if self.next_suggestion_text(cx).is_some() {
            SharedString::default()
        } else {
            match self.space() {
                _ if s.steering.is_some() => s.steering.unwrap_or_default().into(),
                _ if s.busy || s.stopping => "回车排队，这一轮结束后自动发出".into(),
                // 后台任务还在跑（这一轮已经结束）：输入框照常能用，顺手说明一句
                _ if self.session_tasks().iter().any(|task| task.running) => {
                    "后台任务运行中，可继续输入…".into()
                }
                Space::Chat => CHAT_PLACEHOLDER.into(),
                Space::Code => PLACEHOLDER.into(),
            }
        };
        if text != self.placeholder {
            self.placeholder = text.clone();
            self.input.update(cx, |state, cx| state.set_placeholder(text, window, cx));
        }
    }

    /// 把建议的提示词填进输入框（光标放到末尾），不直接发送：用户可以先改。
    pub(super) fn fill_prompt(
        &mut self,
        prompt: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 光标放到最后一行行尾（退回输入框的排队消息可能有好几行）
        let line = prompt.matches('\n').count() as u32;
        let column = prompt.rsplit('\n').next().unwrap_or_default().chars().count() as u32;
        self.input.update(cx, |state, cx| {
            state.set_value(prompt.to_string(), window, cx);
            state.set_cursor_position(Position::new(line, column), window, cx);
            state.focus(window, cx);
        });
        self.refresh_composer(cx);
    }
}

fn provider_name(name: &str) -> &'static str {
    match name {
        "GLM 智谱" => "GLM",
        "Kimi 月之暗面" => "Kimi",
        "Ollama 本地" => "Ollama",
        "DeepSeek" => "DeepSeek",
        "Anthropic" => "Anthropic",
        "OpenAI" => "OpenAI",
        "OpenRouter" => "OpenRouter",
        _ => "模型服务",
    }
}

/// 「下午好，WYT」：时段 + 系统用户名（取不到就不带名字）
/// 「3 分钟前」「2 天前」这样的相对时间。
fn ago(now_ms: u64, at_ms: u64) -> String {
    let mins = now_ms.saturating_sub(at_ms) / 60_000;
    match mins {
        0 => "刚刚".into(),
        m if m < 60 => format!("{m} 分钟前"),
        m if m < 60 * 24 => format!("{} 小时前", m / 60),
        m => format!("{} 天前", m / (60 * 24)),
    }
}

fn greeting() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
        + usage::local_offset_secs();
    let part = match secs.rem_euclid(86_400) / 3600 {
        5..12 => "上午好",
        12..18 => "下午好",
        _ => "晚上好",
    };
    match std::env::var("USERNAME").or_else(|_| std::env::var("USER")) {
        Ok(name) if !name.trim().is_empty() => format!("{part}，{name}"),
        _ => part.to_string(),
    }
}

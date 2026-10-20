//! 空会话首页：问候语 + 用量概览（数据来自会话日志，见 usage.rs；卡片、格子、模型行点了进用量页，见 usage_view.rs），
//! 输入框下方一排建议入口、空输入框里的建议提示词（Tab 填入，见 `complete_view.rs`）。

use super::ChatApp;
use super::widgets::Surface;
use crate::model::*;
use crate::settings::KernelChoice;
use crate::theme::rgb;
use crate::theme::*;
use crate::usage::{self, HEAT_COLS, HEAT_ROWS, Overview};
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::input::Position;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::time::{SystemTime, UNIX_EPOCH};

/// 首页的建议：(图标, 入口上的短名, 填进输入框的提示词)
const SUGGESTIONS: [(IconAll, &str, &str); 4] = [
    (IconAll::FolderTree, "梳理项目结构", "梳理这个项目的结构：主要模块、入口和它们之间的关系"),
    (IconAll::Bug, "排查潜在问题", "通读最近改动的代码，找出可能的 bug 和没处理的边界情况"),
    (IconAll::FlaskConical, "补单元测试", "给核心逻辑补上单元测试，先列出要覆盖的情况再写"),
    (
        IconAll::GitCommitHorizontal,
        "总结最近改动",
        "看看 git log 和还没提交的改动，总结最近做了什么",
    ),
];

/// 输入框平时的占位文字
const PLACEHOLDER: &str = "描述要做的事，回车发送，Shift+回车换行";

impl ChatApp {
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
            .shadow_xs()
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

    /// 可点的统计卡：点了打开用量页（右上角一个小箭头示意）。
    pub(super) fn stat_link(c: &StatCard, cx: &mut Context<Self>) -> Stateful<Div> {
        Self::stat_card(c)
            .id(c.label)
            .relative()
            .cursor_pointer()
            .hover(|d| d.bg(rgb(HOVER)))
            .child(
                Icon::new(IconAll::ArrowUpRight)
                    .size_3p5()
                    .absolute()
                    .top_3()
                    .right_3()
                    .text_color(rgb(INK_FAINT)),
            )
            .on_click(cx.listener(|this, _, window, cx| this.open_usage(window, cx)))
    }

    /// 概览页签：6 张统计卡 + 最近 26 周热力图。卡片和有过去日子的格子都能点，进用量页。
    pub(super) fn render_usage_cards(o: &Overview, cx: &mut Context<Self>) -> Div {
        let dash = || SharedString::from("—");
        let stats = [
            StatCard { label: "会话数", value: usage::compact(o.sessions as u64).into() },
            StatCard { label: "消息数", value: usage::compact(o.messages as u64).into() },
            StatCard { label: "Token 总量", value: usage::compact(o.tokens).into() },
            StatCard { label: "活跃天数", value: o.active_days.to_string().into() },
            StatCard {
                label: "高峰时段",
                value: o.peak_hour.map_or_else(dash, |h| usage::hour_label(h).into()),
            },
            StatCard {
                label: "最常用模型",
                value: o.models.first().map_or_else(dash, |m| m.name.clone().into()),
            },
        ];
        let mut cards: Vec<Stateful<Div>> = stats.iter().map(|c| Self::stat_link(c, cx)).collect();
        let second = cards.split_off(3);

        // 格子按列数均分整行宽度，与上面的统计卡两端对齐
        let today = usage::day_of(super::now_ms(), usage::local_offset_secs());
        let mut grid = div().v_flex().w_full().gap(px(3.));
        for row in 0..HEAT_ROWS {
            let mut line = div().h_flex().w_full().gap(px(3.));
            for col in 0..HEAT_COLS {
                let i = row * HEAT_COLS + col;
                let cell = div().flex_1().h(px(14.)).rounded_sm();
                let Some(v) = o.heat[i] else {
                    line = line.child(cell);
                    continue;
                };
                let day = o.heat_first + (col * 7 + row) as i64;
                let tip: SharedString = match o.heat_turns[i] {
                    0 => format!("{} · 没有对话", usage::date_label(day, today)),
                    n => format!("{} · {n} 轮", usage::date_label(day, today)),
                }
                .into();
                line = line.child(
                    cell.id(("heat", i))
                        .bg(rgb(Self::heat_color(v)))
                        .cursor_pointer()
                        .hover(|d| d.border_1().border_color(rgb(INK_SOFT)))
                        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_usage_day(day, window, cx)
                        })),
                );
            }
            grid = grid.child(line);
        }

        div()
            .v_flex()
            .gap_4()
            .child(
                div()
                    .v_flex()
                    .gap_3()
                    .child(div().h_flex().gap_3().children(cards))
                    .child(div().h_flex().gap_3().children(second)),
            )
            .child(grid)
    }

    /// 模型页签：每个模型的轮数和 token（列前 8 个），条形按轮数。点一行进用量页、只看这个模型。
    pub(super) fn render_usage_models(o: &Overview, cx: &mut Context<Self>) -> Div {
        let mut list = div().v_flex().gap_2().w_full();
        if o.models.is_empty() {
            return list.child(
                div()
                    .py_6()
                    .text_size(TEXT_BODY)
                    .text_color(rgb(INK_SOFT))
                    .child("这段时间还没有记录到模型（ACP agent 不上报模型时不计入）"),
            );
        }
        let max = o.models.first().map_or(1, |m| m.turns.max(1));
        for (i, m) in o.models.iter().take(8).enumerate() {
            let share = m.turns as f32 / max as f32;
            let name = m.name.clone();
            list = list.child(
                div()
                    .id(("model-row", i))
                    .v_flex()
                    .gap_1()
                    .p_3()
                    .rounded_lg()
                    .bg(rgb(CARD))
                    .cursor_pointer()
                    .hover(|d| d.bg(rgb(HOVER)))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_usage_model(&name, window, cx)
                    }))
                    .child(
                        div()
                            .h_flex()
                            .gap_3()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .font_family(MONO)
                                    .text_size(TEXT_BODY)
                                    .text_color(rgb(INK))
                                    .child(m.name.clone()),
                            )
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_size(TEXT_SMALL)
                                    .text_color(rgb(INK_SOFT))
                                    .child(format!(
                                        "{} 轮 · {} token",
                                        usage::compact(m.turns as u64),
                                        usage::compact(m.tokens)
                                    )),
                            ),
                    )
                    .child(
                        div().h(px(6.)).w_full().rounded_full().bg(rgb(Self::heat_color(0))).child(
                            div()
                                .h_full()
                                .w(relative(share))
                                .rounded_full()
                                .bg(rgb(Self::heat_color(3))),
                        ),
                    ),
            );
        }
        list
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

    /// 新装还没配内核时，首页的用量面板换成两种接法（见 `Settings::needs_setup`）。
    fn render_setup(&self, cx: &mut Context<Self>) -> Div {
        let option = |id: &'static str,
                      icon: IconAll,
                      title: &'static str,
                      detail: &'static str,
                      kind: KernelChoice,
                      cx: &mut Context<Self>| {
            div()
                .id(id)
                .h_flex()
                .gap_3()
                .p_4()
                .rounded_lg()
                .bg(rgb(CARD))
                .cursor_pointer()
                .hover(|d| d.bg(rgb(HOVER)))
                .child(Icon::new(icon).size_5().flex_shrink_0().text_color(rgb(CORAL)))
                .child(
                    div()
                        .v_flex()
                        .gap_0p5()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .text_size(TEXT_LARGE)
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(rgb(INK))
                                .child(title),
                        )
                        .child(div().text_size(TEXT_SMALL).text_color(rgb(INK_SOFT)).child(detail)),
                )
                .child(Icon::new(IconAll::ChevronRight).size_4().text_color(rgb(INK_SOFT)))
                .on_click(
                    cx.listener(move |this, _, window, cx| this.start_setup(kind, window, cx)),
                )
        };
        div()
            .v_flex()
            .gap_3()
            .w(OVERVIEW_W)
            .p_5()
            .rounded_xl()
            .bg(rgb(SAND))
            .child(
                div()
                    .v_flex()
                    .gap_1()
                    .pb_1()
                    .child(
                        div()
                            .text_size(TEXT_LEAD)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(INK))
                            .child("先接上一个模型"),
                    )
                    .child(
                        div()
                            .text_size(TEXT_SMALL)
                            .text_color(rgb(INK_SOFT))
                            .child("claudesktop 自己不带模型。选一种接法，之后随时能在设置里改。"),
                    ),
            )
            .child(option(
                "setup-builtin",
                IconAll::Cpu,
                "用 API Key",
                "DeepSeek、Kimi、GLM、Anthropic、OpenAI 等，或本机的 Ollama：选个预设，填上密钥",
                KernelChoice::Builtin,
                cx,
            ))
            .child(option(
                "setup-acp",
                IconAll::SquareTerminal,
                "接 Claude Code 等 ACP agent",
                "用 agent 自己的登录和模型；已填好 Claude Code 的适配器（需要 Node.js）",
                KernelChoice::Acp,
                cx,
            ))
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
        let body = match self.overview_tab {
            1 => Self::render_usage_models(&o, cx),
            2 => self.render_usage_requests(false, cx),
            _ => Self::render_usage_cards(&o, cx),
        };

        div()
            .v_flex()
            .flex_1()
            .min_h_0()
            .items_center()
            .justify_center()
            .gap_6()
            .child(
                // 问候语
                div()
                    .h_flex()
                    .gap_2p5()
                    .items_center()
                    .child(Icon::new(IconAll::Sparkles).size_6().text_color(rgb(CORAL)))
                    .child(
                        div()
                            .text_size(TEXT_TITLE)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(rgb(INK))
                            .child(greeting()),
                    ),
            )
            .children(setup)
            .children((!needs_setup).then(|| self.render_recent(cx)).flatten())
            .when(!needs_setup, |d| {
                d.child(
                    div()
                        .v_flex()
                        .gap_4()
                        .w(OVERVIEW_W)
                        .p_5()
                        .rounded_xl()
                        .bg(rgb(SAND))
                        .child(
                            // 顶行：左侧 tabs，右侧范围
                            div()
                                .h_flex()
                                .items_center()
                                .child(
                                    div()
                                        .h_flex()
                                        .gap_1()
                                        .child(self.tab(
                                            "tab-overview",
                                            "概览",
                                            self.overview_tab == 0,
                                            cx,
                                            |t| t.overview_tab = 0,
                                        ))
                                        .child(self.tab(
                                            "tab-model",
                                            "模型",
                                            self.overview_tab == 1,
                                            cx,
                                            |t| t.overview_tab = 1,
                                        ))
                                        .child(self.tab(
                                            "tab-requests",
                                            "请求",
                                            self.overview_tab == 2,
                                            cx,
                                            |t| t.overview_tab = 2,
                                        )),
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
                        .child(body)
                        // 「请求」页签自己有汇总行，不再挂书本换算
                        .when(self.overview_tab != 2, |d| {
                            d.child(
                                div()
                                    .pt_1()
                                    .text_size(TEXT_SMALL)
                                    .text_color(rgb(INK_SOFT))
                                    .child(usage::book_line(o.tokens)),
                            )
                        }),
                )
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
                .shadow_xs()
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
                .w(OVERVIEW_W)
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

    /// 当前会话是首页（还没发过消息）。
    pub(super) fn on_home(&self) -> bool {
        !self.settings_open && !self.session().has_user_message()
    }

    /// 首页空输入框里建议的提示词：每个会话固定一条（按会话令牌轮换）。
    pub(super) fn ghost_prompt(&self) -> Option<&'static str> {
        // 还没配内核时不引人发消息（发了也只会报没配置）
        (self.on_home() && !self.settings.needs_setup())
            .then(|| SUGGESTIONS[self.session().token as usize % SUGGESTIONS.len()].2)
    }

    /// 输入框的占位文字：首页是建议的提示词，跑着时说明回车会排队，刚拒绝了权限时请用户说要怎么改，
    /// 其余是操作提示。变了才写进输入框（每帧调一次）。
    ///
    /// 「猜的下一句」画在输入框里时占位文字让开（留空）：两句淡色字叠在同一行没法读。
    pub(super) fn sync_placeholder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let s = self.session();
        let text: SharedString = if self.next_suggestion_text(cx).is_some() {
            SharedString::default()
        } else {
            match self.ghost_prompt() {
                _ if s.steering => "说说要它怎么改，回车发送".into(),
                _ if s.busy || s.stopping => "回车排队，这一轮结束后自动发出".into(),
                Some(prompt) => format!("试试：{prompt}（Tab 填入）").into(),
                None => PLACEHOLDER.into(),
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

    /// 首页输入框下方的一排建议入口，点了把提示词填进输入框。
    pub(super) fn render_suggestions(&self, cx: &mut Context<Self>) -> Div {
        div().h_flex().flex_wrap().justify_center().gap_2().pt_1().children(
            SUGGESTIONS.iter().enumerate().map(|(i, &(icon, label, prompt))| {
                div()
                    .id(("suggestion", i))
                    .h_flex()
                    .items_center()
                    .gap_1p5()
                    .px_2p5()
                    .py_1()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(LINE))
                    .cursor_pointer()
                    .hover(|d| d.bg(rgb(HOVER)).border_color(rgb(LINE_STRONG)))
                    .child(Icon::new(icon).size_3p5().text_color(rgb(INK_SOFT)))
                    .child(div().text_size(TEXT_SMALL).text_color(rgb(INK_SOFT)).child(label))
                    .on_click(
                        cx.listener(move |this, _, window, cx| {
                            this.fill_prompt(prompt, window, cx)
                        }),
                    )
            }),
        )
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

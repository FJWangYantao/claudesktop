//! 空会话首页：问候语 + 用量概览（数据来自会话日志，见 usage.rs），
//! 输入框下方一排建议入口、空输入框里的建议提示词（Tab 填入，见 `complete_view.rs`）。

use super::ChatApp;
use crate::model::*;
use crate::theme::rgb;
use crate::theme::*;
use crate::usage::{self, HEAT_COLS, HEAT_ROWS, Overview};
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::input::Position;
use gpui_kit::component::*;
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

    fn stat_card(c: &StatCard) -> Div {
        div()
            .v_flex()
            .gap_1()
            // 等分整行（不按内容宽），两行卡片上下对齐成列
            .flex_1()
            .min_w_0()
            .p_3()
            .rounded_lg()
            .bg(rgb(CARD))
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

    /// 概览页签：6 张统计卡 + 最近 26 周热力图
    fn render_usage_cards(o: &Overview) -> Div {
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

        // 格子按列数均分整行宽度，与上面的统计卡两端对齐
        let mut grid = div().v_flex().w_full().gap(px(3.));
        for row in 0..HEAT_ROWS {
            let mut line = div().h_flex().w_full().gap(px(3.));
            for col in 0..HEAT_COLS {
                let cell = div().flex_1().h(px(14.)).rounded_sm();
                line = line.child(match o.heat[row * HEAT_COLS + col] {
                    Some(v) => cell.bg(rgb(Self::heat_color(v))),
                    None => cell,
                });
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
                    .child(div().h_flex().gap_3().children(stats[..3].iter().map(Self::stat_card)))
                    .child(div().h_flex().gap_3().children(stats[3..].iter().map(Self::stat_card))),
            )
            .child(grid)
    }

    /// 模型页签：每个模型的轮数和 token，条形按轮数
    fn render_usage_models(o: &Overview) -> Div {
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
        for m in o.models.iter().take(8) {
            let share = m.turns as f32 / max as f32;
            list = list.child(
                div()
                    .v_flex()
                    .gap_1()
                    .p_3()
                    .rounded_lg()
                    .bg(rgb(CARD))
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
                cx.notify();
            }))
            .child(Self::seg(label, active))
    }

    pub(super) fn render_overview(&self, cx: &mut Context<Self>) -> Div {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
        let range = match self.overview_range {
            1 => Some(30),
            2 => Some(7),
            _ => None,
        };
        let o = usage::overview(&self.usage, now, usage::local_offset_secs(), range);
        let body = if self.overview_tab == 1 {
            Self::render_usage_models(&o)
        } else {
            Self::render_usage_cards(&o)
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
            .child(
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
                            ),
                    )
                    .child(body)
                    .child(
                        div()
                            .pt_1()
                            .text_size(TEXT_SMALL)
                            .text_color(rgb(INK_SOFT))
                            .child(usage::book_line(o.tokens)),
                    ),
            )
    }
}

impl ChatApp {
    /// 当前会话是首页（还没发过消息）。
    pub(super) fn on_home(&self) -> bool {
        !self.settings_open && !self.session().has_user_message()
    }

    /// 首页空输入框里建议的提示词：每个会话固定一条（按会话令牌轮换）。
    pub(super) fn ghost_prompt(&self) -> Option<&'static str> {
        self.on_home().then(|| SUGGESTIONS[self.session().token as usize % SUGGESTIONS.len()].2)
    }

    /// 输入框的占位文字：首页是建议的提示词，跑着时说明回车会排队，刚拒绝了权限时请用户说要怎么改，
    /// 其余是操作提示。变了才写进输入框（每帧调一次）。
    pub(super) fn sync_placeholder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let s = self.session();
        let text: SharedString = match self.ghost_prompt() {
            _ if s.steering => "说说要它怎么改，回车发送".into(),
            _ if s.busy || s.stopping => "回车排队，这一轮结束后自动发出".into(),
            Some(prompt) => format!("试试：{prompt}（Tab 填入）").into(),
            None => PLACEHOLDER.into(),
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

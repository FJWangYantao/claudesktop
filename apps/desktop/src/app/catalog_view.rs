//! 设置页「MCP」「插件」共用的上半截：「已安装 / 推荐」两栏切换 + 搜索框，和推荐列表里的一行。
//!
//! 推荐的内容在 `claudesktop_core::catalog`；装的动作各页自己做（MCP 写 mcp.json，插件稀疏克隆）。

use super::ChatApp;
use super::settings_kit::seg_control;
use crate::theme::rgb;
use crate::theme::*;
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

/// 看哪一栏。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Shelf {
    Installed,
    Picks,
}

/// 一页的栏位和搜索框。
pub(super) struct ShelfState {
    /// 用户点过的栏；None = 自动（装了东西看「已安装」，否则看「推荐」）
    pub(super) shelf: Option<Shelf>,
    pub(super) search: Entity<InputState>,
}

impl ShelfState {
    pub(super) fn new(
        placeholder: &'static str,
        window: &mut Window,
        cx: &mut Context<ChatApp>,
    ) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        // 打字即筛
        cx.subscribe(&search, |_: &mut ChatApp, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        })
        .detach();
        Self { shelf: None, search }
    }

    pub(super) fn current(&self, installed: usize) -> Shelf {
        self.shelf.unwrap_or(if installed > 0 { Shelf::Installed } else { Shelf::Picks })
    }

    /// 搜索词（去掉首尾空白）；空 = 不筛。
    pub(super) fn query(&self, cx: &App) -> String {
        self.search.read(cx).value().trim().to_string()
    }
}

/// 任一字段含搜索词（不分大小写）就算匹配。
pub(super) fn matches(query: &str, fields: &[&str]) -> bool {
    let query = query.to_lowercase();
    query.is_empty() || fields.iter().any(|f| f.to_lowercase().contains(&query))
}

/// 两栏切换 + 搜索框。`pick` 取出这一页的 `ShelfState`。
pub(super) fn shelf_bar(
    id: &'static str,
    state: &ShelfState,
    installed: usize,
    picks: usize,
    pick: fn(&mut ChatApp) -> &mut ShelfState,
    cx: &mut Context<ChatApp>,
) -> Div {
    let current = state.current(installed);
    div()
        .h_flex()
        .items_center()
        .gap_3()
        .child(seg_control(
            id,
            [
                (Shelf::Installed, format!("已安装 {installed}")),
                (Shelf::Picks, format!("推荐 {picks}")),
            ],
            current,
            move |this, shelf, _, cx| {
                pick(this).shelf = Some(shelf);
                cx.notify();
            },
            cx,
        ))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .rounded_lg()
                .border_1()
                .border_color(rgb(LINE))
                .bg(rgb(CARD))
                .px_1()
                .child(
                    Input::new(&state.search)
                        .appearance(false)
                        .bordered(false)
                        .cleanable(true)
                        .prefix(Icon::new(IconAll::Search).size_3p5().text_color(rgb(INK_SOFT))),
                ),
        )
}

/// 推荐列表里的一行：标题、名字（等宽）、要准备什么，下面一句说明；右边是按钮或状态。
pub(super) fn pick_row(
    first: bool,
    title: &'static str,
    name: &'static str,
    needs: &'static str,
    description: &'static str,
    action: impl IntoElement,
) -> Div {
    div()
        .h_flex()
        .items_center()
        .gap_3()
        .px_3()
        .py_2()
        .when(!first, |d| d.border_t_1().border_color(rgb(LINE)))
        .child(
            div()
                .v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(
                    div()
                        .h_flex()
                        .items_baseline()
                        .gap_2()
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_size(TEXT_BODY)
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(rgb(INK))
                                .child(title),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .font_family(MONO)
                                .text_size(TEXT_META)
                                .text_color(rgb(INK_SOFT))
                                .child(name),
                        )
                        .when(!needs.is_empty(), |d| {
                            d.child(
                                div()
                                    .flex_shrink_0()
                                    .px_1p5()
                                    .rounded_sm()
                                    .bg(rgb(SAND))
                                    .text_size(TEXT_META)
                                    .text_color(rgb(INK_SOFT))
                                    .child(needs),
                            )
                        }),
                )
                .child(div().text_size(TEXT_SMALL).text_color(rgb(INK_SOFT)).child(description)),
        )
        .child(div().flex_none().child(action))
}

/// 推荐行右边的「已添加」之类的状态字（带对勾）。
pub(super) fn pick_done(text: &'static str) -> Div {
    div()
        .h_flex()
        .items_center()
        .gap_1()
        .text_size(TEXT_SMALL)
        .text_color(rgb(INK_SOFT))
        .child(Icon::new(IconAll::Check).size_3p5())
        .child(text)
}

/// 筛完一个都没有时的一行字。
pub(super) fn no_match(query: &str) -> Div {
    div()
        .py_4()
        .h_flex()
        .justify_center()
        .text_size(TEXT_SMALL)
        .text_color(rgb(INK_SOFT))
        .child(format!("没有和「{query}」匹配的"))
}

/// 推荐列表 / 已安装列表外面那张卡片。
pub(super) fn list_card() -> Div {
    div().v_flex().rounded_xl().border_1().border_color(rgb(LINE)).bg(rgb(CARD))
}

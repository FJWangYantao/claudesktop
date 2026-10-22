//! 快捷键速查（Ctrl+/）：对话框里按分组列出全部快捷键。
//!
//! 按键串取 `keys.rs` 里和绑定共用的常量，改键不用改这里；输入框里的按键（发送、换行、
//! `/` `@` 补全、Tab 填入建议）由输入框和 `complete_view.rs` 处理，没有常量，写在表里。

use super::ChatApp;
use super::keys::*;
use crate::theme::rgb;
use crate::theme::*;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::*;
use gpui_kit::*;

/// 面板宽度（rem：跟着界面缩放）；分两栏，每栏一列分组
const SHORTCUTS_W: Rems = rems(44.);

/// 速查表的一行：(说明, 按键…)。一行有几个键时并排显示，任按其一。
type Row = (&'static str, &'static [&'static str]);

/// 速查表：(分组, 行)。双数下标的组排左栏、单数排右栏，两栏行数尽量持平。
pub(super) const SHORTCUTS: &[(&str, &[Row])] = &[
    (
        "通用",
        &[
            ("命令面板", &[PALETTE_KEY]),
            ("快捷键速查", &[SHORTCUTS_KEY]),
            ("新会话", &[NEW_SESSION_KEY]),
            ("显示 / 隐藏侧栏", &[TOGGLE_SIDEBAR_KEY]),
            ("下一个会话", &[NEXT_SESSION_KEY]),
            ("上一个会话", &[PREV_SESSION_KEY]),
        ],
    ),
    (
        "对话",
        &[
            ("停止这一轮（排队的消息退回输入框）", &[STOP_KEY]),
            ("从子任务详情回到对话", &[STOP_KEY]),
            ("按权限卡上的按钮", &PERMISSION_KEYS),
            ("切换权限模式", &[CYCLE_MODE_KEY]),
            ("切换视图（摘要 / 标准 / 详细）", &[CYCLE_VIEW_KEY]),
        ],
    ),
    ("界面", &[("放大 / 缩小", &[ZOOM_IN_KEY, ZOOM_OUT_KEY]), ("恢复标准大小", &[ZOOM_RESET_KEY])]),
    (
        "输入框",
        &[
            ("发送（这一轮还在跑时排队）", &["enter"]),
            ("换行", &["shift-enter"]),
            ("指令 / 引用文件", &["/", "@"]),
            ("填入建议的提示词 / 接上补全的灰字", &["tab"]),
            ("选补全列表的上一项 / 下一项", &INPUT_HISTORY_KEYS),
            ("光标在开头时回上一条输入 / 在末尾时取下一条", &INPUT_HISTORY_KEYS),
            ("光标在第一行 / 最后一行：移到开头 / 移到末尾", &INPUT_HISTORY_KEYS),
        ],
    ),
];

/// 一组快捷键：灰色组名 + 一行一条（说明 | 按键），行间细分隔线。
fn shortcut_group(group: &'static str, rows: &'static [Row]) -> Div {
    let mut list = div().v_flex();
    for (label, keys) in rows {
        list = list.child(
            div()
                .h_flex()
                .items_center()
                .gap_3()
                .py_1()
                .border_b_1()
                .border_color(rgb(LINE))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(TEXT_BODY)
                        .text_color(rgb(INK))
                        .child(*label),
                )
                .child(
                    div()
                        .h_flex()
                        .flex_shrink_0()
                        .gap_1()
                        .children(keys.iter().map(|key| ChatApp::kbd(key))),
                ),
        );
    }
    div()
        .v_flex()
        .gap_1()
        .child(
            div()
                .text_size(TEXT_SMALL)
                .font_weight(FontWeight::MEDIUM)
                .text_color(rgb(INK_SOFT))
                .child(group),
        )
        .child(list)
}

impl ChatApp {
    /// 打开快捷键速查（已有对话框开着时不叠第二个）。Esc 或点外面关闭。
    pub(crate) fn open_shortcuts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if window.has_active_dialog(cx) {
            return;
        }
        window.open_dialog(cx, |dialog, _, _| {
            let column = |side: usize| {
                div().v_flex().flex_1().min_w_0().gap_4().children(
                    SHORTCUTS
                        .iter()
                        .enumerate()
                        .filter(move |(i, _)| i % 2 == side)
                        .map(|(_, (group, rows))| shortcut_group(group, rows)),
                )
            };
            let body =
                div().h_flex().items_start().gap_6().pb_2().child(column(0)).child(column(1));
            dialog
                .w(SHORTCUTS_W.to_pixels(root_font_size()))
                .title(
                    div()
                        .text_size(TEXT_LEAD)
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(INK))
                        .child("键盘快捷键"),
                )
                .child(body)
        });
    }
}

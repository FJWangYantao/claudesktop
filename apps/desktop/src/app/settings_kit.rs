//! 设置页各分类共用的排版件：分区标题、设置行、选择列表、步骤式说明、可展开的示例。
//!
//! 和首页一个路子：用「面」分层（沙色面板里放白色行），少描边；说明只留一句「这是干什么的」，
//! 收进标题旁的 ⓘ（悬停看），怎么用写成编号步骤放进空状态，格式细节收进「示例」里按需展开。

use super::ChatApp;
use super::motion::{self, Reveal};
use crate::theme::rgb;
use crate::theme::*;
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

/// 标题旁的 ⓘ：悬停显示说明。说明一律收在这里，页面上只留标题和控件；空说明不画。
pub(super) fn info_tip(key: &str, tip: SharedString) -> Option<AnyElement> {
    (!tip.is_empty()).then(|| {
        div()
            .id(ElementId::Name(format!("info-tip-{key}").into()))
            .flex_none()
            .text_color(rgb(INK_FAINT))
            .hover(|d| d.text_color(rgb(INK_SOFT)))
            .child(Icon::new(IconAll::Info).size_3p5())
            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
            .into_any_element()
    })
}

/// 一个分区：标题（说明收进旁边的 ⓘ），右边可挂操作按钮，下面是内容。
pub(super) fn section(
    title: &'static str,
    desc: impl Into<SharedString>,
    actions: Option<AnyElement>,
    body: impl IntoElement,
) -> Div {
    div()
        .v_flex()
        .gap_3()
        .child(
            div()
                .h_flex()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .h_flex()
                        .items_center()
                        .flex_1()
                        .min_w_0()
                        .gap_1p5()
                        .child(
                            div()
                                .text_size(TEXT_LEAD)
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(INK))
                                .child(title),
                        )
                        .children(info_tip(title, desc.into())),
                )
                .when_some(actions, |d, a| d.child(div().flex_none().h_flex().gap_1p5().child(a))),
        )
        .child(body)
}

/// 设置项的一行：左边标题 + ⓘ 说明（空串不画），右边控件。
pub(super) fn setting_row(
    title: impl Into<SharedString>,
    tip: impl Into<SharedString>,
    control: impl IntoElement,
) -> Div {
    row(title.into(), tip.into(), None, control)
}

/// 同 [`setting_row`]，标题下面再挂一行不算说明的东西（文件路径、日志操作按钮）。
pub(super) fn setting_row_with(
    title: impl Into<SharedString>,
    tip: impl Into<SharedString>,
    extra: impl IntoElement,
    control: impl IntoElement,
) -> Div {
    row(title.into(), tip.into(), Some(extra.into_any_element()), control)
}

fn row(
    title: SharedString,
    tip: SharedString,
    extra: Option<AnyElement>,
    control: impl IntoElement,
) -> Div {
    // 放不下时控件换到下一行：文字列按 14rem 起算参与折行（不然宽控件会把它挤成一个字宽、竖着排），
    // 同一行放得下时再长满剩下的宽度
    div()
        .h_flex()
        .flex_wrap()
        .items_center()
        .gap_x_4()
        .gap_y_2()
        .py_3p5()
        .child(
            div()
                .v_flex()
                .flex_grow(1.)
                .flex_shrink(1.)
                .flex_basis(rems(14.))
                .min_w_0()
                .gap_0p5()
                .child(
                    div()
                        .h_flex()
                        .items_center()
                        .gap_1p5()
                        .child(
                            div()
                                .text_size(TEXT_BODY)
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(rgb(INK))
                                .child(title.clone()),
                        )
                        .children(info_tip(&title, tip)),
                )
                .when_some(extra, |d, extra| {
                    d.child(
                        div()
                            .text_size(TEXT_SMALL)
                            .text_color(rgb(INK_SOFT))
                            .line_height(LEADING)
                            .child(extra),
                    )
                }),
        )
        .child(div().flex_none().max_w_full().child(control))
}

/// 一组设置行，行与行之间细分隔线（第一行顶上不留白，紧跟分区标题）。
pub(super) fn setting_group(rows: impl IntoIterator<Item = Div>) -> Div {
    let mut group = div().v_flex();
    for (i, row) in rows.into_iter().enumerate() {
        group = group.child(
            row.when(i == 0, |d| d.pt_0()).when(i > 0, |d| d.border_t_1().border_color(rgb(LINE))),
        );
    }
    group
}

/// 表单里的一项：左边固定宽的标签（提示收进旁边的 ⓘ），右边输入框。
pub(super) fn form_field(
    label: &'static str,
    hint: Option<SharedString>,
    input: impl IntoElement,
) -> Div {
    div()
        .h_flex()
        .items_start()
        .gap_4()
        .child(
            // 标签和输入框文字基线大致对齐：输入框上下各有内边距
            div()
                .w(rems(6.5))
                .flex_none()
                .h_flex()
                .items_center()
                .gap_1()
                .pt_1p5()
                .text_size(TEXT_BODY)
                .text_color(rgb(INK))
                .child(label)
                .children(hint.and_then(|h| info_tip(label, h))),
        )
        .child(div().v_flex().flex_1().min_w_0().child(input))
}

/// 选择列表（默认内核、服务商档案）的外壳：沙色面板，和首页「先接上一个模型」同款。
pub(super) fn choice_panel() -> Div {
    div().v_flex().gap_0p5().p_1().rounded_xl().bg(rgb(SAND))
}

/// 选择列表里的一行：图标 + 名称 + 说明，选中的那行浮起成白色卡片并打勾。
/// 返回的行还没挂点击，`trailing` 放在对勾前（如删除按钮）。
pub(super) fn choice_row(
    id: impl Into<ElementId>,
    icon: IconAll,
    title: impl Into<SharedString>,
    desc: impl Into<SharedString>,
    active: bool,
    trailing: Option<AnyElement>,
) -> Stateful<Div> {
    let desc: SharedString = desc.into();
    div()
        .id(id)
        .h_flex()
        .items_center()
        .gap_3()
        .px_3()
        .py_2p5()
        .rounded_lg()
        .cursor_pointer()
        .when(active, |d| d.bg(rgb(CARD)))
        .when(!active, |d| d.hover(|d| d.bg(rgb(SELECTED))))
        .child(Icon::new(icon).size_4().flex_shrink_0().text_color(rgb(if active {
            CORAL
        } else {
            INK_SOFT
        })))
        .child(
            div()
                .v_flex()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_size(TEXT_BODY)
                        .text_color(rgb(INK))
                        .child(title.into()),
                )
                .when(!desc.is_empty(), |d| {
                    d.child(
                        div()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(TEXT_SMALL)
                            .text_color(rgb(INK_SOFT))
                            .child(desc),
                    )
                }),
        )
        .when_some(trailing, |d, t| d.child(t))
        .child(
            // 对勾占位：没选中时也留出宽度，切换时行里的文字不左右跳
            div().size_4().flex_none().when(active, |d| {
                d.child(Icon::new(IconAll::Check).size_4().text_color(rgb(CORAL)))
            }),
        )
}

/// 分段切换（外观、动画、协议）：沙色底上一排 `seg`，点了调 `on_pick(第几个)`。
pub(super) fn seg_control<T: Copy + PartialEq + 'static, L: Into<SharedString>>(
    id: &'static str,
    options: impl IntoIterator<Item = (T, L)>,
    current: T,
    on_pick: impl Fn(&mut ChatApp, T, &mut Window, &mut Context<ChatApp>) + Clone + 'static,
    cx: &mut Context<ChatApp>,
) -> Div {
    seg_control_in(id, options, current, on_pick, cx)
}

/// 同 [`seg_control`]，用在别的 view 里（如请求原文对话框）。
pub(super) fn seg_control_in<V: 'static, T: Copy + PartialEq + 'static, L: Into<SharedString>>(
    id: &'static str,
    options: impl IntoIterator<Item = (T, L)>,
    current: T,
    on_pick: impl Fn(&mut V, T, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> Div {
    let mut segs = div().h_flex().gap_0p5().p_0p5().rounded_lg().bg(rgb(SAND));
    for (i, (value, label)) in options.into_iter().enumerate() {
        let on_pick = on_pick.clone();
        segs = segs.child(
            div()
                .id((id, i))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, window, cx| on_pick(this, value, window, cx)))
                .child(ChatApp::seg(label, value == current)),
        );
    }
    segs
}

/// 「怎么用」：沙色底的编号步骤，放在空状态里代替一大段说明。
pub(super) fn how_to(title: &'static str, steps: &[&str]) -> Div {
    let mut list = div().v_flex().gap_1p5();
    for (i, step) in steps.iter().enumerate() {
        list = list.child(
            div()
                .h_flex()
                .items_start()
                .gap_2p5()
                .child(
                    div()
                        .size_5()
                        .flex_none()
                        .h_flex()
                        .justify_center()
                        .rounded_full()
                        .bg(rgb(CARD))
                        .text_size(TEXT_META)
                        .font_features(tabular_nums())
                        .text_color(rgb(INK_SOFT))
                        .child((i + 1).to_string()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(TEXT_SMALL)
                        .text_color(rgb(INK))
                        .line_height(LEADING)
                        .child(step.to_string()),
                ),
        );
    }
    div()
        .v_flex()
        .gap_3()
        .p_4()
        .rounded_xl()
        .bg(rgb(SAND))
        .child(
            div()
                .h_flex()
                .items_center()
                .gap_2()
                .child(Icon::new(IconAll::Lightbulb).size_4().text_color(rgb(CORAL)))
                .child(
                    div()
                        .text_size(TEXT_BODY)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(rgb(INK))
                        .child(title),
                ),
        )
        .child(list)
}

/// 一句附注：小号灰字，前面一个图标（安全提醒、适用范围）。
pub(super) fn note(icon: IconAll, text: impl Into<SharedString>) -> Div {
    div()
        .h_flex()
        .items_start()
        .gap_2()
        .text_size(TEXT_SMALL)
        .text_color(rgb(INK_SOFT))
        .line_height(LEADING)
        .child(Icon::new(icon).size_3p5().flex_shrink_0().mt_1())
        // 文字单独包一层可收缩的块，长了才会折行（直接挂在 h_flex 上会撑出页面）
        .child(div().flex_1().min_w_0().child(text.into()))
}

/// 等宽的示例块（配置文件、单价表的写法）。
pub(super) fn example_block(text: &'static str) -> Div {
    div()
        .p_3()
        .rounded_lg()
        .bg(rgb(SAND))
        .font_family(MONO)
        .text_size(TEXT_META)
        .text_color(rgb(INK))
        .line_height(LEADING)
        .children(text.lines().map(|l| div().whitespace_nowrap().child(l.to_string())))
}

impl ChatApp {
    /// 可展开的说明：一个「标题 ›」的小链接，点了在下面展开 `content`（弹簧动画）。
    /// 开着哪些记在 `SettingsForm::open_help`，按 `key` 区分。
    pub(super) fn disclosure(
        &self,
        key: &'static str,
        label: &'static str,
        content: impl IntoElement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let open = self.settings_form.open_help.contains(key);
        let toggle = div()
            .id(ElementId::Name(format!("disclosure-{key}").into()))
            .h_flex()
            .items_center()
            .gap_1()
            .px_1()
            .ml(rems(-0.25))
            .rounded_md()
            .cursor_pointer()
            .text_size(TEXT_SMALL)
            .text_color(rgb(INK_SOFT))
            .hover(|d| d.text_color(rgb(INK)))
            .on_click(cx.listener(move |this, _, _, cx| {
                let help = &mut this.settings_form.open_help;
                if !help.remove(key) {
                    help.insert(key);
                }
                cx.notify();
            }))
            .child(label)
            .child(
                Icon::new(if open { IconAll::ChevronDown } else { IconAll::ChevronRight })
                    .size_3p5(),
            );
        let progress = motion::reveal_progress(
            ElementId::Name(format!("disclosure-{key}").into()),
            open,
            window,
            cx,
        );
        // 外面再包一层 h_flex：链接只有文字那么宽，不占满整行
        div().v_flex().child(div().h_flex().child(toggle)).when(progress > 0., |d| {
            d.child(Reveal::new(
                ElementId::Name(format!("disclosure-body-{key}").into()),
                progress,
                div().w_full().pt_2().child(content),
            ))
        })
    }
}

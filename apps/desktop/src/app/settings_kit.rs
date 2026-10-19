//! 设置页各分类共用的排版件：分区标题、设置行、选择列表、步骤式说明、可展开的示例。
//!
//! 和首页一个路子：用「面」分层（沙色面板里放白色行），少描边；说明只留一句「这是干什么的」，
//! 怎么用写成编号步骤放进空状态，格式细节收进「示例」里按需展开。

use super::ChatApp;
use super::motion::{self, Reveal};
use crate::theme::rgb;
use crate::theme::*;
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

/// 一个分区：标题 + 一句说明（可空），右边可挂操作按钮，下面是内容。
pub(super) fn section(
    title: &'static str,
    desc: impl Into<SharedString>,
    actions: Option<AnyElement>,
    body: impl IntoElement,
) -> Div {
    let desc: SharedString = desc.into();
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
                        .v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_0p5()
                        .child(
                            div()
                                .text_size(TEXT_LEAD)
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(INK))
                                .child(title),
                        )
                        .when(!desc.is_empty(), |d| {
                            d.child(
                                div()
                                    .text_size(TEXT_SMALL)
                                    .text_color(rgb(INK_SOFT))
                                    .line_height(LEADING)
                                    .child(desc),
                            )
                        }),
                )
                .when_some(actions, |d, a| d.child(div().flex_none().h_flex().gap_1p5().child(a))),
        )
        .child(body)
}

/// 设置项的一行：左边标题 + 灰色说明，右边控件。
pub(super) fn setting_row(
    title: impl Into<SharedString>,
    desc: impl IntoElement,
    control: impl IntoElement,
) -> Div {
    div()
        .h_flex()
        .items_center()
        .gap_4()
        .py_3p5()
        .child(
            div()
                .v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(
                    div()
                        .text_size(TEXT_BODY)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(rgb(INK))
                        .child(title.into()),
                )
                .child(
                    div()
                        .text_size(TEXT_SMALL)
                        .text_color(rgb(INK_SOFT))
                        .line_height(LEADING)
                        .child(desc),
                ),
        )
        .child(div().flex_none().child(control))
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

/// 表单里的一项：左边固定宽的标签，右边输入框，下面可带一句提示。
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
                .w(rems(5.5))
                .flex_none()
                .pt_1p5()
                .text_size(TEXT_BODY)
                .text_color(rgb(INK))
                .child(label),
        )
        .child(div().v_flex().flex_1().min_w_0().gap_1p5().child(input).when_some(hint, |d, h| {
            d.child(
                div().text_size(TEXT_SMALL).text_color(rgb(INK_SOFT)).line_height(LEADING).child(h),
            )
        }))
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

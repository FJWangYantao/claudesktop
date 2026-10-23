//! 对话记录渲染：过程组（思考 / 工具 / 自动批准）、diff、权限卡、计划、正文、备注。
//!
//! 层级：回复正文最显眼；过程步骤一律一行灰色小字（左侧固定宽的图标格对齐），
//! 连续的几步收进可折叠的组（分组规则在 `Session::compute_layout`）。

use super::ChatApp;
use super::menu::diff_stat;
use super::motion::{self, Reveal};
use super::rewind::file_name;
use super::widgets::Surface;
use crate::model::*;
use crate::session::{RowLayout, Session, StepGroup, WheelScroll};
use crate::settings::KernelChoice;
use crate::theme::rgb;
use crate::theme::*;
use claudesktop_core::{DiffView, PermissionHunks, PermissionOptionView};
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::base::motion::{Presence, Transition};
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::input::Position;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::text::{TextView, TextViewStyle};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// 同组相邻两步之间的行距
const STEP_GAP: Rems = rems(0.375);
/// 块与块之间的行距
pub(super) const BLOCK_GAP: Rems = rems(1.);
/// 步骤行左侧的图标格宽度；组内细线对着它的中线。
/// 和图标（`size_3p5` 这类也是 rem）、`STEP_INDENT` 一起跟字号缩放，放大后正文才和标题文字对齐
const ICON_COL: Rems = rems(1.);
/// 图标格 + 间距（`step_line` 的 `gap_2`）：步骤的展开内容（思考正文等）缩进到文字起点
pub(super) const STEP_INDENT: Rems = rems(1.5);
/// 输入框上方的计划最多露出几条
const PINNED_PLAN_ROWS: usize = 5;

/// 一张能点开看大图的图片：用户发的那张（本地路径），或者回复里加载过的远程图。
///
/// 远程图不存路径：字节已经按内容哈希登记在 `crate::assets` 里，这里按地址回查。
#[derive(Clone)]
pub(super) enum ImageView {
    /// 本地文件
    Path(PathBuf),
    /// 远程图片的地址
    Remote(String),
}

/// 图片大图：gpui-kit 的对话框，宽约窗口八成，图按原比例缩进去（小图不放大），
/// 底部「另存为…」（本地图写文件，远程图写已经取回的那份字节）。
///
/// 自由函数而不是 `ChatApp` 的方法：`remote_image.rs` 在 Markdown 插件里画图，
/// 那里只有 `&mut App`，拿不到 `ChatApp`。
pub(super) fn open_image(image: ImageView, window: &mut Window, cx: &mut App) {
    let viewport = window.viewport_size();
    let width = (viewport.width * 0.8).min(px(1200.));
    let height = viewport.height * 0.72;
    let dialog_image = image.clone();
    window.open_dialog(cx, move |dialog, _window, _cx| {
        let light: AnyElement = match &dialog_image {
            ImageView::Path(path) => {
                img(path.clone()).size_full().object_fit(ObjectFit::ScaleDown).into_any_element()
            }
            ImageView::Remote(url) => super::remote_image::registered_name(url).map_or_else(
                || div().text_color(rgb(INK_SOFT)).child("这张图还没加载").into_any_element(),
                |name| img(name).size_full().object_fit(ObjectFit::ScaleDown).into_any_element(),
            ),
        };
        let save = dialog_image.clone();
        let save_button = Button::new("save-image")
            .label("另存为…")
            .small()
            .cursor_pointer()
            .on_click(move |_, window, cx| save_image(save.clone(), window, cx));
        dialog.w(width).margin_top(viewport.height * 0.06).child(
            div()
                .v_flex()
                .gap_3()
                .w_full()
                .child(div().w_full().h(height).flex().justify_center().items_center().child(light))
                .child(div().h_flex().justify_end().child(save_button)),
        )
    });
}

/// 「另存为…」：弹系统保存对话框再把字节写下去（远程图写的是已经取回的那份）。
pub(super) fn save_image(image: ImageView, window: &mut Window, cx: &mut App) {
    let (bytes, suggested) = match &image {
        ImageView::Path(path) => match std::fs::read(path) {
            Ok(bytes) => {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "image.png".into());
                (Some(bytes), name)
            }
            Err(err) => (None, format!("读不了这张图：{err}")),
        },
        ImageView::Remote(url) => {
            // 扩展名按取回的实际格式（登记名 `remote/<哈希>.jpg` 的尾巴），别一律写 .png
            let ext = super::remote_image::registered_name(url)
                .and_then(|n| n.rsplit_once('.').map(|(_, ext)| ext.to_string()))
                .unwrap_or_else(|| "png".into());
            let name = format!("{}.{ext}", super::remote_image::RemoteImage::host_of(url));
            (crate::assets::cached(url), name)
        }
    };
    let Some(bytes) = bytes else {
        // 失败原因本身就是一句话（本地读不了 / 还没加载）
        let note = Notification::error(suggested);
        window.push_notification(note, cx);
        return;
    };
    let dir = std::env::current_dir().unwrap_or_default();
    let rx = cx.prompt_for_new_path(&dir, Some(&suggested));
    // 这里只有 `App`（浮层回调给的就是 `&mut App`），用窗口句柄回推提示
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        if let Ok(Ok(Some(path))) = rx.await {
            let note = match std::fs::write(&path, &bytes) {
                Ok(()) => Notification::success(format!("已保存：{}", path.display())),
                Err(err) => Notification::error(format!("保存失败：{err}")),
            };
            let _ = handle.update(cx, |_, window, cx| window.push_notification(note, cx));
        }
    })
    .detach();
}

/// 步骤行左侧的图标格：固定宽度居中，转圈 / 种类图标 / 失败叉都放这里，文字才对得齐。
pub(super) fn icon_cell(icon: impl IntoElement) -> Div {
    div().w(ICON_COL).flex_shrink_0().flex().justify_center().items_center().child(icon)
}

/// 一行过程步骤的骨架：图标格 + 内容，13px 次要色，单行。
pub(super) fn step_line(icon: impl IntoElement) -> Div {
    div()
        .h_flex()
        .items_center()
        .gap_2()
        .min_w_0()
        .text_size(TEXT_SMALL)
        .text_color(rgb(INK_SOFT))
        .child(icon_cell(icon))
}

/// 单行省略的文字段（悬停看全文，见 [`text_tip`]）。
pub(super) fn ellipsis(id: impl Into<ElementId>, text: SharedString) -> Stateful<Div> {
    let tip = text.clone();
    ellipsis_plain(id, text).tooltip(move |window, cx| text_tip(&tip, window, cx))
}

/// 单行省略、不带悬停提示（全文在别处看得到时用，如点开后的工具行）。
pub(super) fn ellipsis_plain(id: impl Into<ElementId>, text: SharedString) -> Stateful<Div> {
    div().id(id).min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(text)
}

/// 多行文字在单行里只显示第一行，后面还有就接个省略号。
fn first_line(text: &str) -> SharedString {
    let text = text.trim();
    match text.split_once('\n') {
        Some((first, _)) => format!("{} …", first.trim_end()).into(),
        None => text.to_string().into(),
    }
}

/// 悬停提示里最多放的字符数（再长的去点开看）
const TIP_MAX_CHARS: usize = 300;
/// 悬停提示的最大宽度，超出折行
const TIP_W: Rems = rems(28.);
/// 工具行点开后完整命令最多列的行数（heredoc 之类的长脚本）
const COMMAND_PREVIEW_LINES: usize = 20;

/// 长文本的悬停提示：限宽折行、超长截断。gpui-kit 的 `Tooltip` 不限宽，
/// 一条长命令会横着铺满整个窗口。
pub(super) fn text_tip(text: &str, window: &mut Window, cx: &mut App) -> AnyView {
    let text = text.trim();
    let text: SharedString = match text.char_indices().nth(TIP_MAX_CHARS) {
        Some((cut, _)) => format!("{}…", &text[..cut]).into(),
        None => text.to_string().into(),
    };
    Tooltip::element(move |_, _| div().max_w(TIP_W).child(text.clone())).build(window, cx)
}

/// 工具行末尾的授权标记：小盾牌 + 短标签（「已允许」「总是允许」…），悬停看选的是哪一项。
/// 原先是工具行下面单独一行「权限请求 … · 允许一次」，顺序像是先跑后问，还每个工具多占一行。
/// 拒绝时结果里已经写了「已拒绝」就不重复。
fn grant_badge(id: impl Into<ElementId>, card: &ToolCard) -> Option<Stateful<Div>> {
    let grant = card.grant.as_ref()?;
    if grant.kind == GrantKind::Denied && card.result.contains("拒绝") {
        return None;
    }
    let icon = match grant.kind {
        GrantKind::Denied => IconAll::ShieldBan,
        GrantKind::Cancelled => IconAll::Shield,
        _ => IconAll::ShieldCheck,
    };
    let tip: SharedString = match grant.kind {
        GrantKind::Auto => format!("按设置自动批准：{}", grant.label).into(),
        GrantKind::Cancelled => "等你授权时这一轮停了，没有运行".into(),
        _ => format!("你选了「{}」", grant.label).into(),
    };
    Some(
        div()
            .id(id)
            .flex_shrink_0()
            .h_flex()
            .items_center()
            .gap_1()
            .text_color(rgb(INK_SOFT))
            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
            .child(Icon::new(icon).size_3())
            .child(grant.short()),
    )
}

/// 工具的状态图标：进行中转圈，失败红叉，完成显示种类图标。
/// 转圈换成结果图标时淡入、从小放大到原尺寸（`settle` 见 [`motion::settle`]）。
pub(super) fn tool_status_icon(card: &ToolCard, settle: f32) -> AnyElement {
    let icon = match card.phase {
        ToolPhase::Running => return motion::spinner().into_any_element(),
        ToolPhase::Failed => Icon::new(IconAll::CircleX).size_3p5().text_color(rgb(RED)),
        ToolPhase::Done => {
            Icon::new(tool_icon(card.kind_key())).size_3p5().text_color(rgb(INK_SOFT))
        }
    };
    let scale = 0.6 + 0.4 * settle;
    div()
        .opacity(settle)
        .child(icon.transform(Transformation::scale(size(scale, scale))))
        .into_any_element()
}

/// 回复 / 思考共用的 Markdown 样式。
pub(super) fn markdown_style() -> TextViewStyle {
    TextViewStyle::default()
        .paragraph_gap(rems(0.75))
        // 标题接口只收 Pixels，所以按当前缩放换算一次
        // 正文 15px：一级 24 / 二级 20 / 三级 17，四级起和正文同大、只靠加粗分（相邻两档拉开 3px 以上，
        // 长回复扫一眼能看出分节）。标题上方的段间距由组件按标题字号算，字号拉开留白也跟着变大
        .heading_font_size(|level, _| {
            match level {
                1 => TEXT_DISPLAY,
                2 => TEXT_HEADING,
                3 => TEXT_LEAD,
                _ => TEXT_LARGE,
            }
            .to_pixels(crate::theme::root_font_size())
        })
        .inline_code(HighlightStyle {
            background_color: Some(rgb(CODE_BG).into()),
            ..Default::default()
        })
        // 顶部多留一截给右上角的语言名 + 复制按钮，免得压住第一行代码（2rem = 32px）
        .code_block(StyleRefinement::default().bg(rgb(SAND)).pt(rems(2.)))
}

/// 代码块右上角的一组：语言名（有的话）+ 「复制」按钮。
/// 叠在代码块的沙色底上，按钮悬停用 `SELECTED`。
fn code_block_header(
    lang: Option<SharedString>,
    copied: Option<Instant>,
    on_copy: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &App,
) -> Div {
    let (icon, label) =
        if copied.is_some() { (IconAll::Check, "已复制") } else { (IconAll::Copy, "复制") };
    let pop = copied.and_then(|t| motion::enter(t.elapsed(), window, cx));
    div()
        .h_flex()
        .items_center()
        .gap_1()
        .bg(rgb(SAND))
        .text_size(TEXT_META)
        .text_color(rgb(INK_SOFT))
        .font_family(sans())
        .children(lang.filter(|l| !l.is_empty()).map(|l| div().px_1().child(l)))
        .child(
            ChatApp::quiet_button("copy-code", icon, Surface::Sand, cx)
                .xsmall()
                .px_1p5()
                .label(label)
                .when_some(pop, |b, (opacity, _)| b.opacity(opacity))
                .on_click(on_copy),
        )
}

/// 计划的一条：完成的打勾、变淡；进行中的橙点、加粗；待办空心圈。
/// 完成项不划线：删除线按拉丁字体的度量画，压在中文上偏低，像下划线。
fn plan_row(row: &PlanRow) -> Div {
    let (icon, color) = match row.status {
        PlanStatus::Done => (IconAll::CircleCheck, GREEN),
        PlanStatus::InProgress => (IconAll::CircleDot, CORAL),
        PlanStatus::Pending => (IconAll::Circle, INK_FAINT),
    };
    div()
        .h_flex()
        .items_start()
        .gap_2()
        // 图标对齐第一行文字的中线（14px 字 × 1.6 行高）
        .child(icon_cell(Icon::new(icon).size_3p5().text_color(rgb(color))).mt(px(4.)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(TEXT_BODY)
                .line_height(LEADING)
                .map(|d| match row.status {
                    PlanStatus::Done => d.text_color(rgb(INK_SOFT)),
                    PlanStatus::InProgress => {
                        d.text_color(rgb(INK)).font_weight(FontWeight::MEDIUM)
                    }
                    PlanStatus::Pending => d.text_color(rgb(INK)),
                })
                .child(row.text.clone()),
        )
}

/// 计划卡片的标题行：图标 + 「计划」+ 进度。
fn plan_header(rows: &[PlanRow]) -> Div {
    let (done, total, _) = plan_progress(rows);
    div()
        .h_flex()
        .items_center()
        .gap_2()
        .text_size(TEXT_SMALL)
        .child(icon_cell(Icon::new(IconAll::ListTodo).size_3p5()))
        .child("计划")
        .child(div().flex_1())
        .child(format!("{done}/{total}"))
}

impl ChatApp {
    /// 对话里的一行：可能先画过程组的组头，再画自己（组内步骤缩进、挂细线），最后留行距。
    /// `reveal` 是组内步骤的展开进度（不在组里的行恒为 1）：收起没走完时 `layout.body` 已经是
    /// false，但还要按进度裁切着画完。
    pub(super) fn render_row(
        &self,
        ix: usize,
        item: &ChatItem,
        layout: RowLayout,
        reveal: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let mut col = div().v_flex();
        if let Some(group) = layout.head {
            // 组头下面紧跟着组内步骤时只隔一小段
            let follows = layout.body || layout.tight;
            col =
                col.child(self.render_group_head(ix, group, cx).when(follows, |d| d.pb(STEP_GAP)));
        }
        if layout.body || reveal > 0. && layout.member {
            let body = self.render_item(ix, item, layout, window, cx);
            // 正在收起的步骤按展开时的样子画（缩进、挂细线、和下一步收紧）
            let (nested, tight) =
                if layout.body { (layout.nested, layout.tight) } else { (true, true) };
            let body = if nested {
                // 组内步骤：缩进到组头文字下面，左侧一条细线串起来；
                // 行距算在细线里面，相邻两行的线才连得上
                // 细线落在图标格中线上（左边距 + 1px 线宽 + 右边距 ≈ `STEP_INDENT`）
                div()
                    .ml(rems(0.4375))
                    .pl(rems(1.))
                    .border_l_1()
                    .border_color(rgb(LINE))
                    .when(tight, |d| d.pb(STEP_GAP))
                    .child(body)
            } else {
                body
            };
            col = col.child(if reveal < 1. {
                Reveal::new(("step-body", ix), reveal, body).into_any_element()
            } else {
                body.into_any_element()
            });
        }
        // 正在收起、自己已不显示的步骤不留块间距（组头那行会留）
        let gap = Self::row_gap(layout);
        col.when(gap != Rems::ZERO, |d| d.pb(gap))
    }

    /// 这一行底下留多高的块间距：`content.rs` 里给高亮描边留位置也要用同一个值，
    /// 所以判断只放在这里（别再抄一份条件过去）。
    pub(super) fn row_gap(layout: RowLayout) -> Rems {
        // 正在收起、自己已不显示的步骤不留块间距（组头那行会留）
        if !layout.tight && (layout.body || layout.head.is_some()) { BLOCK_GAP } else { Rems::ZERO }
    }

    /// 过程组的组头：一行摘要（读了几个文件、跑了几条命令…），点开 / 收起；详细视图总是展开、不给开关。
    fn render_group_head(
        &self,
        ix: usize,
        group: StepGroup,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let s = self.session();
        let (summary, failed) =
            group_summary(s.transcript[ix..group.end].iter().filter(|i| i.has_content()));
        let toggle = self.settings.view_mode != ViewMode::Verbose;
        div()
            .id(("step-group", ix))
            .when(toggle, |d| d.role(Role::Button).aria_expanded(group.open))
            .h_flex()
            .items_center()
            .gap_2()
            .min_w_0()
            .text_size(TEXT_SMALL)
            .text_color(rgb(INK_SOFT))
            .when(toggle, |d| {
                d.cursor_pointer().hover(|d| d.text_color(rgb(INK))).on_click(cx.listener(
                    move |this, _, _, cx| {
                        let active = this.active;
                        this.sessions[active].toggle_group(ix);
                        cx.notify();
                    },
                ))
            })
            .child(icon_cell(Icon::new(IconAll::ListChecks).size_3p5()))
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(summary),
            )
            .when(failed > 0, |d| {
                d.child(
                    div().flex_shrink_0().text_color(rgb(RED)).child(format!("· {failed} 步失败")),
                )
            })
            .when(toggle, |d| {
                d.child(
                    Icon::new(if group.open {
                        IconAll::ChevronDown
                    } else {
                        IconAll::ChevronRight
                    })
                    .size_3(),
                )
            })
    }

    /// 思考：一行「已思考 12 秒 ›」，点开看正文（Markdown，次要色小一号字）；
    /// 流式时扫光「思考中…」；`verbose`（详细视图）时总是展开、不给开关。
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_thinking(
        idx: usize,
        text: &str,
        streaming: bool,
        open: bool,
        elapsed_ms: Option<u64>,
        verbose: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let has_text = !text.trim().is_empty();
        let toggle = !verbose && has_text;
        let open = (open || verbose) && has_text;
        let header = step_line(Icon::new(IconAll::Brain).size_3p5().text_color(rgb(INK_SOFT)))
            .id(("think-toggle", idx))
            .when(toggle, |d| d.role(Role::Button).aria_expanded(open))
            .when(toggle, |d| {
                d.cursor_pointer().hover(|d| d.text_color(rgb(INK))).on_click(cx.listener(
                    move |this, _, _, cx| {
                        if let Some(ChatItem::Thinking { open, .. }) =
                            this.sessions[this.active].transcript.get_mut(idx)
                        {
                            *open = !*open;
                            cx.notify();
                        }
                    },
                ))
            })
            .child(if streaming {
                // 流式中高光扫过；减少动态效果时停在静态文字
                motion::shimmer_text("思考中…", INK_SOFT, INK).into_any_element()
            } else {
                // 用时是会变的数字：等宽，免得和别的思考块对不齐
                div()
                    .font_features(tabular_nums())
                    .child(thought_label(elapsed_ms))
                    .into_any_element()
            })
            .when(toggle, |d| {
                d.child(
                    Icon::new(if open { IconAll::ChevronDown } else { IconAll::ChevronRight })
                        .size_3(),
                )
            });

        let mut col = div().v_flex().child(header);
        // 展开 / 收起走弹簧；收起没走完之前正文照样画、按进度裁切。
        // 和标题之间的空隙算在正文里，收到底时不剩一截
        let progress = motion::reveal_progress(("think", idx), open, window, cx);
        if progress > 0. && has_text {
            let text = reading_markdown(text.trim(), streaming);
            // 字号 / 颜色要设在 TextView 自己身上（它按主题取默认值，不继承外层）
            let body = div().pt_1().pl(STEP_INDENT).child(
                TextView::markdown(("think-md", idx), text)
                    .style(markdown_style().paragraph_gap(rems(0.5)))
                    .markdown_extensions(crate::app::math::extensions())
                    .text_size(TEXT_SMALL)
                    .text_color(rgb(INK_SOFT))
                    .line_height(LEADING),
            );
            col = col.child(Reveal::new(("think-body", idx), progress, body));
        }
        col
    }

    /// 启动信息块（agent 在首条提示前推送的 Skills / Extensions 列表）；`verbose` 时总是展开。
    pub(super) fn render_info(
        idx: usize,
        text: &str,
        open: bool,
        verbose: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let open = open || verbose;
        let header = step_line(Icon::new(IconAll::Info).size_3p5().text_color(rgb(INK_SOFT)))
            .id(("info-toggle", idx))
            .when(!verbose, |d| {
                d.cursor_pointer().hover(|d| d.text_color(rgb(INK))).on_click(cx.listener(
                    move |this, _, _, cx| {
                        if let Some(ChatItem::Info { open, .. }) =
                            this.sessions[this.active].transcript.get_mut(idx)
                        {
                            *open = !*open;
                            cx.notify();
                        }
                    },
                ))
            })
            .child("启动信息")
            .when(!verbose, |d| {
                d.child(
                    Icon::new(if open { IconAll::ChevronDown } else { IconAll::ChevronRight })
                        .size_3(),
                )
            });
        let mut col = div().v_flex().child(header);
        let progress = motion::reveal_progress(("info", idx), open, window, cx);
        if progress > 0. {
            let body = div()
                .pt_1()
                .pl(STEP_INDENT)
                .text_size(TEXT_SMALL)
                .font_family(MONO)
                .text_color(rgb(INK_SOFT))
                .line_height(LEADING)
                .child(text.trim().to_string());
            col = col.child(Reveal::new(("info-body", idx), progress, body));
        }
        col
    }

    /// 工具：一行「图标 名称 参数 · 结果」；带 diff 时 diff 卡片的表头兼当这一行。
    pub(super) fn render_tool(
        idx: usize,
        card: &ToolCard,
        cwd: &Path,
        verbose: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        if card.diffs.is_empty() {
            return Self::render_tool_plain(idx, card, verbose, window, cx);
        }
        // 文件改动：默认每份只显示前若干行，超出的部分展开 / 收起走弹簧
        let preview = (!verbose).then_some(DIFF_PREVIEW_ROWS);
        let rest = motion::reveal_progress(("diff", idx), card.expanded || verbose, window, cx);
        let settle = motion::settle(("tool", idx), card.phase != ToolPhase::Running, window, cx);
        let mut col = div().v_flex().gap_2();
        for (k, diff) in card.diffs.iter().enumerate() {
            let rest = (ElementId::from(("diff-rest", idx * 64 + k)), rest);
            col =
                col.child(Self::render_diff(diff, cwd, preview, Some(rest), Some((card, settle))));
        }
        // diff 已经说明了改了什么；结果摘要只在失败时有用（为什么没改成）
        if card.phase == ToolPhase::Failed && !card.result.is_empty() {
            col = col.child(
                div()
                    .pl(STEP_INDENT)
                    .text_size(TEXT_SMALL)
                    .text_color(rgb(RED))
                    .child(card.result.clone()),
            );
        }
        let hidden: usize =
            card.diffs.iter().map(|d| d.lines.len().saturating_sub(DIFF_PREVIEW_ROWS)).sum();
        if !verbose && (hidden > 0 || card.expanded) {
            let label = if card.expanded {
                "收起".to_string()
            } else {
                format!("展开其余 {hidden} 行")
            };
            col = col.child(
                div()
                    .id(("diff-toggle", idx))
                    .pl_3()
                    .cursor_pointer()
                    .text_size(TEXT_SMALL)
                    .text_color(rgb(INK_SOFT))
                    .hover(|d| d.text_color(rgb(INK)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(ChatItem::Tool(card)) =
                            this.sessions[this.active].transcript.get_mut(idx)
                        {
                            card.expanded = !card.expanded;
                            cx.notify();
                        }
                    }))
                    .child(label),
            );
        }
        col
    }

    /// 不带 diff 的工具：工具行；有输出时点开看预览（读到的内容 / 命令输出，
    /// 内核已截到前若干行）。`verbose`（详细视图）时总是展开、不给开关。
    fn render_tool_plain(
        idx: usize,
        card: &ToolCard,
        verbose: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let has_output = card.can_open();
        let toggle = has_output && !verbose;
        let open = has_output && (card.expanded || verbose);
        let settle = motion::settle(("tool", idx), card.phase != ToolPhase::Running, window, cx);
        let line =
            Self::render_tool_line(idx, card, settle).id(("tool-toggle", idx)).when(toggle, |d| {
                d.cursor_pointer()
                    .hover(|d| d.text_color(rgb(INK)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(ChatItem::Tool(card)) =
                            this.sessions[this.active].transcript.get_mut(idx)
                        {
                            card.expanded = !card.expanded;
                            cx.notify();
                        }
                    }))
                    .child(
                        div().flex_shrink_0().child(
                            Icon::new(if open {
                                IconAll::ChevronDown
                            } else {
                                IconAll::ChevronRight
                            })
                            .size_3(),
                        ),
                    )
            });
        let mut col = div().v_flex().child(line);
        let progress = motion::reveal_progress(("tool", idx), open, window, cx);
        if progress > 0. && has_output {
            col = col.child(Reveal::new(("tool-body", idx), progress, Self::tool_output(card)));
        }
        col
    }

    /// 工具行点开后的内容（等宽、卡片底），缩进到工具名下面；和标题之间的空隙算在里面。
    /// 命令先列完整命令（`$` 开头，折行），下面是输出（跑着时是到目前为止的最后几行）。
    pub(super) fn tool_output(card: &ToolCard) -> Div {
        // 空行给个空格占住行高
        let line =
            |l: &str| div().child(if l.is_empty() { " ".to_string() } else { l.to_string() });
        let output = card.output.trim_matches('\n');
        let running = card.phase == ToolPhase::Running;
        let command =
            card.shows_command().then(|| {
                let lines: Vec<&str> = card.arg.trim().lines().collect();
                let shown = lines.len().min(COMMAND_PREVIEW_LINES);
                div()
                    .v_flex()
                    .text_color(rgb(INK))
                    .children(lines[..shown].iter().enumerate().map(|(i, l)| {
                        line(&if i == 0 { format!("$ {l}") } else { (*l).to_string() })
                    }))
                    .when(lines.len() > shown, |d| {
                        d.child(
                            div()
                                .text_color(rgb(INK_SOFT))
                                .child(format!("… 其余 {} 行未显示", lines.len() - shown)),
                        )
                    })
            });
        let has_command = command.is_some();
        div().pt_1().pl(STEP_INDENT).child(
            div()
                .v_flex()
                .gap_2()
                .px_3()
                .py_2()
                .rounded_lg()
                .border_1()
                .border_color(rgb(LINE))
                .bg(rgb(CARD))
                .font_family(MONO)
                .text_size(TEXT_SMALL)
                .line_height(LEADING)
                .text_color(rgb(INK_SOFT))
                .children(command)
                .when(!output.is_empty(), |d| {
                    d.child(div().v_flex().children(output.lines().map(line)))
                })
                .when(has_command && output.is_empty() && running, |d| {
                    d.child(div().text_color(rgb(INK_FAINT)).child("运行中，还没有输出…"))
                }),
        )
    }

    /// 不带 diff 的工具：一行，名称稍深、参数等宽、结果摘要接在后面（都单行省略，悬停看全文）。
    pub(super) fn render_tool_line(idx: usize, card: &ToolCard, settle: f32) -> Div {
        let failed = card.phase == ToolPhase::Failed;
        step_line(tool_status_icon(card, settle))
            .child(
                div()
                    .flex_shrink_0()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(rgb(INK))
                    .child(card.name.clone()),
            )
            // 参数和结果按各自长度一起收缩（不给固定比例，免得截断后空出一截）
            // 能点开的（命令）点开就有全文，不再挂悬停提示
            .when(!card.arg.is_empty(), |d| {
                let arg = if card.shows_command() {
                    ellipsis_plain(("tool-arg", idx), first_line(&card.arg))
                } else {
                    ellipsis(("tool-arg", idx), card.arg.clone())
                };
                d.child(arg.font_family(MONO))
            })
            .when(!card.result.is_empty(), |d| {
                d.child(
                    ellipsis(("tool-result", idx), format!("· {}", card.result).into())
                        .when(failed, |d| d.text_color(rgb(RED))),
                )
            })
            .children(grant_badge(("tool-grant", idx), card).map(|b| b.ml_1()))
    }

    /// 自动批准的权限请求：作为过程中的一步，不打断。
    fn render_auto_approved(idx: usize, title: &SharedString, chosen: &SharedString) -> Div {
        step_line(Icon::new(IconAll::ShieldCheck).size_3p5().text_color(rgb(INK_SOFT)))
            .child(div().flex_shrink_0().child("已自动批准"))
            .child(ellipsis(("auto-title", idx), plain_title(title)).font_family(MONO))
            .child(div().flex_shrink_0().child(format!("· {chosen}")))
    }

    /// 图片大图：见自由函数 [`open_image`]（`remote_image.rs` 也要用，那边只有 `&mut App`）。
    pub(super) fn open_image(image: ImageView, window: &mut Window, cx: &mut App) {
        open_image(image, window, cx)
    }

    /// 上下文压缩：两侧细线、中间一行字，标出「从这里往前的对话换成了摘要」；完成后点一下展开摘要。
    fn render_compaction(
        idx: usize,
        state: &CompactState,
        open: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let done = matches!(state, CompactState::Done(_));
        let (indicator, label): (AnyElement, SharedString) = match state {
            CompactState::Running => {
                (motion::spinner().into_any_element(), "正在压缩上下文…".into())
            }
            CompactState::Done(_) => (
                Icon::new(IconAll::ListCollapse).size_3p5().into_any_element(),
                "上下文已压缩，较早的对话换成了摘要".into(),
            ),
            CompactState::Failed(message) => (
                Icon::new(IconAll::CircleAlert).size_3p5().text_color(rgb(RED)).into_any_element(),
                format!("压缩上下文失败：{message}").into(),
            ),
        };
        let rule = || div().flex_1().min_w(px(24.)).h(px(1.)).bg(rgb(LINE));
        let center = div()
            .id(("compaction", idx))
            .flex_shrink(1.)
            .min_w_0()
            .h_flex()
            .items_center()
            .gap_1p5()
            .text_size(TEXT_SMALL)
            .text_color(rgb(INK_SOFT))
            .child(indicator)
            .child(div().min_w_0().line_clamp(2).child(label))
            .when(done, |d| {
                d.cursor_pointer()
                    .hover(|d| d.text_color(rgb(INK)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(ChatItem::Compaction { open, .. }) =
                            this.sessions[this.active].transcript.get_mut(idx)
                        {
                            *open = !*open;
                            cx.notify();
                        }
                    }))
                    .child(
                        Icon::new(if open { IconAll::ChevronDown } else { IconAll::ChevronRight })
                            .size_3(),
                    )
            });
        let mut col = div()
            .v_flex()
            .child(div().h_flex().items_center().gap_3().child(rule()).child(center).child(rule()));
        let progress = motion::reveal_progress(("compaction", idx), open && done, window, cx);
        if let (true, CompactState::Done(summary)) = (progress > 0., state) {
            let body = div().pt_2().child(
                div()
                    .px_4()
                    .py_3()
                    .rounded_xl()
                    .border_1()
                    .border_color(rgb(LINE))
                    .bg(rgb(CARD))
                    .child(
                        TextView::markdown(
                            ("compaction-summary", idx),
                            reading_markdown(summary, false),
                        )
                        .style(markdown_style())
                        .markdown_extensions(crate::app::math::extensions())
                        .text_size(TEXT_BODY)
                        .text_color(rgb(INK))
                        .line_height(LEADING),
                    ),
            );
            col = col.child(Reveal::new(("compaction-body", idx), progress, body));
        }
        col
    }

    /// 失败卡片：发生了什么（原因原文）+ 可以怎么办（重试；超出上下文时先建议新开会话）。
    /// 重试 = 把这一轮的用户消息原样再发一遍；正在跑别的一轮时按钮变淡、点了没反应。
    fn render_failed(
        &self,
        idx: usize,
        message: &SharedString,
        kind: FailureKind,
        cx: &mut Context<Self>,
    ) -> Div {
        let s = self.session();
        let busy = s.busy;
        // 失败前已经有进展时给「继续」（接着做），「重试」会把做过的再做一遍
        // （判断在 Session 里：虚拟列表的测量指纹也要读它，见 `Session::row_key`）
        let resume = s.failure_resume(idx);
        // 已经翻篇的失败卡片只留说明和原文，不给按钮
        let latest = s.failure_is_latest(idx);
        // 主操作只占一个，按「先解决根因、再接着做」排：
        // 上下文溢出只能新开会话，Key 不对只能去改设置，其它情况有进展就继续、没进展就重试
        let primary = if kind == FailureKind::Overflow {
            "fail-new"
        } else if matches!(kind, FailureKind::Auth | FailureKind::Endpoint) {
            "fail-settings"
        } else if resume {
            "fail-resume"
        } else {
            "fail-retry"
        };
        // 卡片自带边框，按钮不再描边：主操作赤陶实底，其余沙色实底（悬停 SELECTED）
        let button = |id: &'static str, icon: IconAll, label: &'static str| {
            Button::new((id, idx))
                .small()
                .px_3()
                .icon(icon)
                .label(label)
                .cursor_pointer()
                .disabled(busy)
                .with_variant(if id == primary {
                    ButtonVariant::Primary
                } else {
                    ButtonVariant::Secondary
                })
        };
        div()
            .v_flex()
            .gap_2()
            .px_4()
            .py_3()
            .rounded_xl()
            .border_1()
            // 出错卡片：状态色描边取浅档（40%），实色描边会和里面的红字抢
            .border_color(state_line(RED))
            .bg(rgb(CARD))
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .child(icon_cell(Icon::new(IconAll::CircleAlert).size_4().text_color(rgb(RED))))
                    .child(
                        div()
                            .text_size(TEXT_BODY)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(INK))
                            .child(kind.title()),
                    ),
            )
            .children(
                kind.hint().map(|hint| div().text_size(TEXT_BODY).text_color(rgb(INK)).child(hint)),
            )
            // 原文：网关 / 模型报的错，最多四行（自己补的卡片没有原文）
            .when(!message.is_empty(), |d| {
                d.child(
                    div()
                        .text_size(TEXT_SMALL)
                        .text_color(rgb(INK_SOFT))
                        .line_clamp(4)
                        .child(message.clone()),
                )
            })
            .when(latest, |card| {
                card.child(
                    div()
                        .h_flex()
                        .gap_2()
                        .pt_1()
                        .when(kind == FailureKind::Overflow, |d| {
                            d.child(
                                button("fail-new", IconAll::MessageSquarePlus, "新开会话")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.add_session(cx);
                                        this.focus_input(window, cx);
                                    })),
                            )
                        })
                        // 配置上的问题：去设置页改（底栏写着该改哪一项）
                        .when(kind.settings_fix(), |d| {
                            d.child(
                                button("fail-settings", IconAll::Settings, "打开设置").on_click(
                                    cx.listener(move |this, _, window, cx| {
                                        this.open_settings_for_failure(kind, window, cx)
                                    }),
                                ),
                            )
                        })
                        .when(resume, |d| {
                            d.child(
                                button("fail-resume", IconAll::Play, "继续")
                                    .on_click(cx.listener(|this, _, _, cx| this.resume_turn(cx))),
                            )
                        })
                        .child(
                            button("fail-retry", IconAll::RotateCcw, "重试").on_click(
                                cx.listener(move |this, _, _, cx| this.retry_turn(idx, cx)),
                            ),
                        ),
                )
            })
    }

    /// 权限卡：内核给的选项按钮之后是「拒绝并说明」。`keyed` = 这是最早一条待定的请求，
    /// 按钮上带 Alt+数字提示（见 keys.rs 的 `answer_key`）。
    ///
    /// 这次改动能按块同意时（`hunks`），先按文件列出每个块，块头可以点掉——放行的块才会写盘，
    /// 剩下的留着不动。默认全部放行（「允许一次」不勾也行）。
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_permission(
        idx: usize,
        title: &SharedString,
        options: &[PermissionOptionView],
        resolved: &Option<SharedString>,
        hunks: Option<&PermissionHunks>,
        keep: Option<&Vec<u32>>,
        keyed: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        if let Some(label) = resolved {
            return step_line(Icon::new(IconAll::Shield).size_3p5().text_color(rgb(INK_SOFT)))
                .child(div().flex_shrink_0().child("权限请求"))
                .child(ellipsis(("perm-title", idx), plain_title(title)).font_family(MONO))
                .child(div().flex_shrink_0().child(format!("· {label}")));
        }

        // 按钮文字后面淡淡跟一个按键提示（不画 Kbd 底框：主按钮是赤陶实底）
        let key_hint = |n: usize| {
            super::keys::PERMISSION_KEYS.get(n).filter(|_| keyed).map(|key| {
                div().text_size(TEXT_META).opacity(0.7).child(Self::kbd(key).appearance(false))
            })
        };
        // 同一个键给读屏（不是最早那条待定请求时没有快捷键，就不挂这个属性）
        let key_name = |n: usize| {
            super::keys::PERMISSION_KEYS.get(n).filter(|_| keyed).map(|key| Self::key_text(key))
        };
        let mut buttons = div().h_flex().flex_wrap().gap_2();
        for (n, opt) in options.iter().enumerate() {
            let allow = opt.kind.starts_with("allow");
            let primary = opt.kind == "allow_once";
            let (id, name) = (opt.id.clone(), opt.name.clone());
            buttons = buttons.child(
                div()
                    .id(("perm", idx * 16 + n))
                    .h_flex()
                    .items_center()
                    .gap_1p5()
                    .px_3()
                    .py_1p5()
                    .rounded_lg()
                    .cursor_pointer()
                    .text_size(TEXT_BODY)
                    .when(primary, |d| {
                        d.bg(rgb(CORAL_DEEP))
                            .text_color(rgb(ON_ACCENT))
                            .hover(|d| d.bg(rgb(CORAL_DEEP_HOVER)))
                    })
                    // 卡片已经有一圈边框，里面的按钮不再描边（单层边框）：其余允许项用沙色实底；
                    // 拒绝只留红字、悬停才出底色（红字在沙色底上只有 4.0:1，白底上 4.7:1）
                    .when(!primary && allow, |d| {
                        d.bg(rgb(SAND)).hover(|d| d.bg(rgb(SELECTED))).text_color(rgb(INK))
                    })
                    .when(!allow, |d| d.hover(|d| d.bg(rgb(HOVER))).text_color(rgb(RED)))
                    .when_some(key_name(n), |d, key| d.aria_keyshortcuts(key))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.resolve_permission(idx, Some((id.clone(), name.clone())), cx);
                    }))
                    .child(opt.name.clone())
                    .children(key_hint(n)),
            );
        }
        // 拒绝并说明：停下这一轮，请用户在输入框里说要怎么改（输入框里已经写了就直接发）
        buttons = buttons.child(
            div()
                .id(("perm-steer", idx))
                .h_flex()
                .items_center()
                .gap_1p5()
                .px_3()
                .py_1p5()
                .rounded_lg()
                .cursor_pointer()
                .text_size(TEXT_BODY)
                .text_color(rgb(INK_SOFT))
                .hover(|d| d.bg(rgb(HOVER)).text_color(rgb(INK)))
                .when_some(key_name(options.len()), |d, key| d.aria_keyshortcuts(key))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.reject_and_steer(idx, window, cx);
                }))
                .child("拒绝并说明…")
                .children(key_hint(options.len())),
        );

        div()
            .v_flex()
            .gap_3()
            .p_4()
            .rounded_xl()
            .border_1()
            // 状态描边取浅档：拿赤陶实色描边太抢，40% 就够指出「这行需要你」
            .border_color(state_line(CORAL))
            .bg(rgb(CARD))
            .child(
                div()
                    .v_flex()
                    .gap_1()
                    .child(
                        div()
                            .h_flex()
                            .items_center()
                            .gap_2()
                            .child(icon_cell(
                                Icon::new(IconAll::ShieldAlert).size_4().text_color(rgb(CORAL)),
                            ))
                            .child(
                                div()
                                    .text_size(TEXT_BODY)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("要允许这一步吗？"),
                            ),
                    )
                    .child(
                        div()
                            .pl(STEP_INDENT)
                            .text_size(TEXT_SMALL)
                            .font_family(MONO)
                            .text_color(rgb(INK_SOFT))
                            .line_height(LEADING)
                            .child(plain_title(title)),
                    ),
            )
            .when_some(hunks.filter(|_| resolved.is_none()), |d, hunks| {
                d.child(div().pl(STEP_INDENT).child(Self::render_hunk_picker(idx, hunks, keep, cx)))
            })
            .child(div().pl(STEP_INDENT).child(buttons))
    }

    /// 提问卡片（`ask_user`）：每题一组选项，选完点「就这样」交给模型。
    /// 答完收成一行（和权限卡一样），点开能看到当时问了什么。
    pub(super) fn render_question(
        idx: usize,
        questions: &[claudesktop_core::QuestionView],
        picks: &[Option<usize>],
        answer: &Option<claudesktop_core::QuestionAnswer>,
        keyed: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        if let Some(answer) = answer {
            let first = questions.first().map(|q| q.question.clone()).unwrap_or_default();
            let picked = match answer {
                claudesktop_core::QuestionAnswer::Choices { choices } => {
                    let parts: Vec<String> = questions
                        .iter()
                        .enumerate()
                        .filter_map(|(i, q)| {
                            let option = choices.get(i).and_then(|n| q.options.get(*n))?;
                            Some(option.label.clone())
                        })
                        .collect();
                    parts.join("；")
                }
                other => other.label().to_string(),
            };
            return step_line(
                Icon::new(IconAll::MessageSquare).size_3p5().text_color(rgb(INK_SOFT)),
            )
            .child(div().flex_shrink_0().child("问了一下"))
            .child(ellipsis(("question-title", idx), first.into()))
            .child(div().flex_shrink_0().child(format!("· {picked}")));
        }

        let key_hint = |n: usize| {
            super::keys::PERMISSION_KEYS.get(n).filter(|_| keyed).map(|key| {
                div().text_size(TEXT_META).opacity(0.7).child(Self::kbd(key).appearance(false))
            })
        };
        let key_name = |n: usize| {
            super::keys::PERMISSION_KEYS.get(n).filter(|_| keyed).map(|key| Self::key_text(key))
        };

        let mut body = div().v_flex().gap_3().pl(STEP_INDENT);
        for (q, question) in questions.iter().enumerate() {
            let mut block = div().v_flex().gap_1p5().child(
                div().text_size(TEXT_BODY).text_color(rgb(INK)).child(question.question.clone()),
            );
            let mut options = div().v_flex().gap_1();
            for (o, option) in question.options.iter().enumerate() {
                let selected = picks.get(q).copied().flatten() == Some(o);
                options = options.child(
                    div()
                        .id(("question-opt", idx * 16 + q * 4 + o))
                        .v_flex()
                        .gap_0p5()
                        .px_3()
                        .py_1p5()
                        .rounded_lg()
                        .cursor_pointer()
                        .text_size(TEXT_BODY)
                        .when(selected, |d| {
                            d.bg(rgb(SELECTED)).border_1().border_color(state_line(CORAL))
                        })
                        .when(!selected, |d| d.bg(rgb(SAND)).hover(|d| d.bg(rgb(SELECTED))))
                        .when_some(key_name(o), |d, key| d.aria_keyshortcuts(key))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.pick_question(idx, q, o, cx);
                        }))
                        .child(
                            div()
                                .h_flex()
                                .items_center()
                                .gap_1p5()
                                .child(
                                    div().flex_shrink_0().child(
                                        Icon::new(if selected {
                                            IconAll::CircleCheck
                                        } else {
                                            IconAll::Circle
                                        })
                                        .size_3p5()
                                        .text_color(
                                            if selected { rgb(CORAL) } else { rgb(INK_FAINT) },
                                        ),
                                    ),
                                )
                                .child(div().min_w_0().child(option.label.clone()))
                                .children(key_hint(o)),
                        )
                        .children(option.description.clone().map(|d| {
                            div().pl_5().text_size(TEXT_META).text_color(rgb(INK_SOFT)).child(d)
                        })),
                );
            }
            block = block.child(options);
            body = body.child(block);
        }

        let ready = picks.iter().all(Option::is_some);
        let mut actions = div().h_flex().flex_wrap().items_center().gap_2().child(
            Self::text_button(("question-submit", idx), "就这样", true)
                .with_variant(ButtonVariant::Primary)
                .disabled(!ready)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.submit_question(idx, cx);
                })),
        );
        // 「自己说」：这一轮停在这里，用户下一条消息就是答案
        actions = actions.child(
            Self::text_button(("question-self", idx), "自己说", false)
                .with_variant(ButtonVariant::Secondary)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.answer_question(idx, claudesktop_core::QuestionAnswer::SelfAnswer, cx);
                })),
        );
        // 「你来定」：让模型自己假设一个继续（免得它反复问）
        actions = actions.child(
            Self::text_button(("question-model", idx), "你来定", false)
                .with_variant(ButtonVariant::Secondary)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.answer_question(idx, claudesktop_core::QuestionAnswer::ModelDecides, cx);
                })),
        );

        div()
            .v_flex()
            .gap_3()
            .p_4()
            .rounded_xl()
            .border_1()
            .border_color(state_line(CORAL))
            .bg(rgb(CARD))
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .child(icon_cell(
                        Icon::new(IconAll::MessageSquare).size_4().text_color(rgb(CORAL)),
                    ))
                    .child(
                        div()
                            .text_size(TEXT_BODY)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("需要你定一下"),
                    ),
            )
            .child(body)
            .child(div().pl(STEP_INDENT).child(actions))
    }

    /// 权限卡上的按块选择：每个文件一段，块头点一下切换「这块写不写」。
    /// 已经放行的块正常画，没放行的淡下去并划一道线；上面写清还剩几块。
    fn render_hunk_picker(
        idx: usize,
        hunks: &PermissionHunks,
        keep: Option<&Vec<u32>>,
        cx: &mut Context<Self>,
    ) -> Div {
        let all: Vec<u32> =
            hunks.files.iter().flat_map(|f| f.hunks.iter().map(|h| h.index)).collect();
        let total = all.len();
        let kept: Vec<u32> = match keep {
            Some(k) => all.iter().copied().filter(|i| k.contains(i)).collect(),
            None => all.clone(),
        };
        let mut body = div().v_flex().gap_1().text_size(TEXT_META).text_color(rgb(INK_SOFT)).child(
            if kept.len() == total {
                format!("这次改 {total} 处：整段允许，或者点掉不想写的几块")
            } else {
                format!(
                    "只写勾着的 {} 处（其余 {} 处不动），点一下切换",
                    kept.len(),
                    total - kept.len()
                )
            },
        );
        for file in &hunks.files {
            let name = file.path.clone();
            body =
                body.child(
                    div().text_size(TEXT_SMALL).font_family(MONO).text_color(rgb(INK)).child(
                        format!("{}{}", name, if file.whole_file { "（新文件）" } else { "" }),
                    ),
                );
            for hunk in &file.hunks {
                let on = kept.contains(&hunk.index);
                let current = kept.clone();
                let index = hunk.index;
                let toggle = cx.listener(move |this: &mut ChatApp, _, _, cx| {
                    let mut next = current.clone();
                    if let Some(at) = next.iter().position(|i| *i == index) {
                        next.remove(at);
                    } else {
                        next.push(index);
                        next.sort_unstable();
                    }
                    if let Some(ChatItem::Permission { keep, .. }) =
                        this.sessions[this.active].transcript.get_mut(idx)
                    {
                        *keep = Some(next);
                    }
                    cx.notify();
                });
                let head = div().text_color(if on { rgb(INK_SOFT) } else { rgb(INK_FAINT) }).child(
                    format!(
                        "第 {} 行 · +{} −{}",
                        hunk.range.new_start + 1,
                        hunk.added(),
                        hunk.removed()
                    ),
                );
                body = body.child(
                    div()
                        .v_flex()
                        .gap_0p5()
                        .rounded_md()
                        .when(on, |d| d)
                        .when(!on, |d| d.opacity(0.55))
                        .child(
                            div()
                                .id(ElementId::Name(
                                    format!("perm-hunk-{idx}-{}", hunk.index).into(),
                                ))
                                .h_flex()
                                .items_center()
                                .gap_1p5()
                                .cursor_pointer()
                                .px_1()
                                .rounded_md()
                                .hover(|d| d.bg(rgb(HOVER)))
                                .on_click(toggle)
                                .child(
                                    Icon::new(if on { IconAll::Check } else { IconAll::Square })
                                        .size_3()
                                        .text_color(rgb(if on { CORAL } else { INK_FAINT })),
                                )
                                .child(head),
                        )
                        .child(
                            div()
                                .id(ElementId::Name(
                                    format!("perm-hunk-diff-{idx}-{}", hunk.index).into(),
                                ))
                                .v_flex()
                                .rounded_sm()
                                .overflow_hidden()
                                .font_family(MONO)
                                .line_height(LEADING)
                                .max_h(rems(11.))
                                .overflow_y_scroll()
                                .children(hunk.lines.iter().map(diff_line)),
                        ),
                );
            }
        }
        body
    }

    /// 回复末尾的改动卡片（对照 Codex）：表头「改动了 N 个文件 +a −b」，最新一轮带「撤回」和「审查」；
    /// 下面每个文件一行（文件名 + 所在目录 + 增删行数），点了跳到它的 diff。
    /// 文件多时先列 [`CHANGES_PREVIEW`] 个，其余收在「显示其余 N 个」里（展开状态记在会话里，进行指纹）。
    fn render_changes_card(
        &self,
        idx: usize,
        files: Vec<TurnFile>,
        latest: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let s = self.session();
        let (added, removed) = files.iter().fold((0, 0), |(a, r), f| (a + f.added, r + f.removed));
        // 撤回 / 审查要读写日志：最新一轮跑完了、有保存记录才给
        let actionable = latest && self.store.is_some() && s.meta.is_some();
        let undo = if actionable { s.latest_undo() } else { None };
        let title = match undo {
            Some(u) if u.all => format!("撤回了 {} 个文件的改动", files.len()),
            Some(_) => format!("改动了 {} 个文件 · 撤回了一部分", files.len()),
            None => format!("改动了 {} 个文件", files.len()),
        };
        let header = div()
            .h_flex()
            .items_center()
            .gap_2()
            .pl_3()
            .pr_2()
            .py_1p5()
            .child(
                Icon::new(if undo.is_some_and(|u| u.all) {
                    IconAll::Undo2
                } else {
                    IconAll::FileDiff
                })
                .size_3p5()
                .flex_shrink_0()
                .text_color(rgb(INK_SOFT)),
            )
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(TEXT_BODY)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(rgb(INK))
                    .child(title),
            )
            .when(undo.is_none_or(|u| !u.all), |d| d.child(diff_stat(added, removed)))
            .child(div().flex_1())
            .when(actionable, |d| {
                let all_undone = undo.is_some_and(|u| u.all);
                d.child(
                    Self::quiet_button(
                        ("changes-undo", idx),
                        if all_undone { IconAll::Redo2 } else { IconAll::Undo2 },
                        Surface::Paper,
                        cx,
                    )
                    .small()
                    .px_2()
                    .label(if all_undone { "恢复" } else { "撤回" })
                    .tooltip(if all_undone {
                        "把撤回的改动全部写回去"
                    } else {
                        "把这一轮改的文件全部还原（不动对话；「审查」里可以逐块恢复）"
                    })
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.undo_turn(idx, !all_undone, cx)),
                    ),
                )
                .child(
                    Self::text_button(("changes-review", idx), "审查", false)
                        .small()
                        .with_variant(ButtonVariant::Secondary)
                        .tooltip("按块查看这一轮的改动，不要的单独撤回")
                        .on_click(cx.listener(move |this, _, _, cx| this.open_review(idx, cx))),
                )
            });

        let cwd = s.cwd.clone();
        let total = files.len();
        let open = s.changes_open.contains(&idx);
        let shown = if open || total <= CHANGES_PREVIEW + 1 { total } else { CHANGES_PREVIEW };
        let faded = undo.is_some_and(|u| u.all);
        let rows = files.into_iter().take(shown).enumerate().map(|(n, f)| {
            let item = f.item;
            let (name, dir) =
                split_path(&claudesktop_core::review::display_path(&f.path, Some(&cwd)));
            div()
                .id(("changes-file", idx * 64 + n))
                .group("changes-file")
                .h_flex()
                .items_center()
                .gap_2()
                .px_3()
                .py_1()
                .cursor_pointer()
                .hover(|d| d.bg(rgb(HOVER)))
                .text_size(TEXT_SMALL)
                .child(
                    Icon::new(if f.created { IconAll::FilePlus } else { IconAll::FileText })
                        .size_3p5()
                        .flex_shrink_0()
                        .text_color(rgb(INK_SOFT)),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .whitespace_nowrap()
                        .text_color(rgb(if faded { INK_SOFT } else { INK }))
                        .child(name),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_color(rgb(INK_SOFT))
                        .children(dir),
                )
                .when(f.created, |d| d.child(new_file_tag()))
                .child(diff_stat(f.added, f.removed).when(faded, |d| d.opacity(0.6)))
                // 悬停才露出的箭头：点这一行是跳到它的 diff
                .child(
                    div()
                        .flex_shrink_0()
                        .opacity(0.)
                        .group_hover("changes-file", |d| d.opacity(1.))
                        .child(Icon::new(IconAll::ChevronRight).size_3().text_color(rgb(INK_SOFT))),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.reveal_change(item, cx)))
        });
        let more = (shown < total || (open && total > CHANGES_PREVIEW + 1)).then(|| {
            let text = if open {
                "收起".to_string()
            } else {
                format!("显示其余 {} 个", total - shown)
            };
            div()
                .id(("changes-more", idx))
                .h_flex()
                .items_center()
                .gap_2()
                .px_3()
                .py_1()
                .cursor_pointer()
                .hover(|d| d.bg(rgb(HOVER)))
                .text_size(TEXT_SMALL)
                .text_color(rgb(INK_SOFT))
                .child(
                    Icon::new(if open { IconAll::ChevronUp } else { IconAll::ChevronDown })
                        .size_3p5()
                        .flex_shrink_0(),
                )
                .child(text)
                .on_click(cx.listener(move |this, _, _, cx| {
                    let active = this.active;
                    let s = &mut this.sessions[active];
                    if !s.changes_open.remove(&idx) {
                        s.changes_open.insert(idx);
                    }
                    cx.notify();
                }))
        });
        div()
            .v_flex()
            .rounded_xl()
            .border_1()
            .border_color(rgb(LINE))
            .bg(rgb(CARD))
            .shadow_xs()
            .overflow_hidden()
            .child(header)
            .child(
                div()
                    .v_flex()
                    .py_1()
                    .border_t_1()
                    .border_color(rgb(LINE))
                    .children(rows)
                    .children(more),
            )
    }

    /// 从改动卡片跳到某个文件的 diff：摘要视图里 diff 卡片是收起的。
    /// 只在内存里换回标准视图——这是「为看一眼」的临时切换，不能当成偏好写回设置文件
    /// （见 [`Self::set_view_mode`]）。
    fn reveal_change(&mut self, item: usize, cx: &mut Context<Self>) {
        if self.settings.view_mode == ViewMode::Summary {
            self.settings.view_mode = ViewMode::Normal;
            self.view_changed(cx);
        }
        self.reveal_message(self.active, item, cx);
    }

    /// 一个文件的 diff 卡片；`preview` = 默认露出的行数（None = 全部）。
    /// 超出的部分由 `rest`（元素 id, 展开进度）决定：进度 > 0 时按进度裁切着画出来。
    /// `tool`（工具, 结果图标的入场进度）给了时表头带上工具的状态图标和名称（兼当工具行）。
    pub(super) fn render_diff(
        diff: &DiffView,
        cwd: &Path,
        preview: Option<usize>,
        rest: Option<(ElementId, f32)>,
        tool: Option<(&ToolCard, f32)>,
    ) -> Div {
        let line_el = diff_line;
        let shown = preview.map_or(diff.lines.len(), |n| n.min(diff.lines.len()));
        let truncated = diff.truncated.then(|| {
            div()
                .px_3()
                .py_1()
                .font_family(sans())
                .text_color(rgb(INK_SOFT))
                .child("改动过大，其余部分未显示")
        });
        let mut body = div()
            .v_flex()
            .py_1()
            .font_family(MONO)
            .text_size(TEXT_SMALL)
            .line_height(LEADING)
            .children(diff.lines[..shown].iter().map(line_el));
        let hidden = &diff.lines[shown..];
        if hidden.is_empty() {
            body = body.children(truncated);
        } else if let Some((id, progress)) = rest.filter(|(_, p)| *p > 0.) {
            let more = div().v_flex().children(hidden.iter().map(line_el)).children(truncated);
            body = body.child(Reveal::new(id, progress, more));
        }
        let icon = match tool {
            Some((card, settle)) => tool_status_icon(card, settle),
            None => {
                Icon::new(IconAll::FileText).size_3p5().text_color(rgb(INK_SOFT)).into_any_element()
            }
        };
        let (name, dir) =
            split_path(&claudesktop_core::review::display_path(&diff.path, Some(cwd)));
        let header = div()
            .h_flex()
            .items_center()
            .gap_2()
            .px_2()
            .py_1p5()
            .bg(rgb(SAND))
            .border_b_1()
            .border_color(rgb(LINE))
            .text_size(TEXT_SMALL)
            .text_color(rgb(INK_SOFT))
            .child(icon_cell(icon))
            .children(tool.map(|(card, _)| {
                div()
                    .flex_shrink_0()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(rgb(INK))
                    .child(card.name.clone())
            }))
            // 文件名在前、所在目录淡色跟在后面（同回复末尾的改动卡片），路径按工作目录写短
            .child(div().flex_shrink_0().whitespace_nowrap().text_color(rgb(INK)).child(name))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .children(dir),
            )
            .when(diff.created, |d| d.child(new_file_tag()))
            .children(tool.and_then(|(card, _)| {
                grant_badge(ElementId::Name(format!("diff-grant-{}", diff.path).into()), card)
            }))
            .child(
                div()
                    .flex_shrink_0()
                    .h_flex()
                    .gap_1p5()
                    .font_family(MONO)
                    .child(div().text_color(rgb(DIFF_ADD_FG)).child(format!("+{}", diff.added)))
                    .child(div().text_color(rgb(DIFF_DEL_FG)).child(format!("−{}", diff.removed))),
            );
        div()
            .v_flex()
            .rounded_xl()
            .border_1()
            .border_color(rgb(LINE))
            .bg(rgb(CARD))
            .overflow_hidden()
            .child(header)
            .child(body)
    }

    /// 对话里的计划：一轮进行中时它钉在输入框上方，这里只留一行进度（`compact`）；
    /// 结束后在这里完整显示最终状态。
    pub(super) fn render_plan(rows: &[PlanRow], compact: bool) -> Div {
        if compact {
            let (done, total, current) = plan_progress(rows);
            return step_line(Icon::new(IconAll::ListTodo).size_3p5().text_color(rgb(INK_SOFT)))
                .child(div().flex_shrink_0().child(format!("更新了计划 · {done}/{total}")))
                .children(current.map(|row| {
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(format!("· 正在：{}", row.text))
                }));
        }
        div()
            .v_flex()
            .gap_1()
            .px_3()
            .py_2p5()
            .rounded_xl()
            .border_1()
            .border_color(rgb(LINE))
            .bg(rgb(CARD))
            .child(plan_header(rows).text_color(rgb(INK_SOFT)).pb_0p5())
            .children(rows.iter().map(plan_row))
    }

    /// 输入框上方钉着的计划（进行中的一轮才有）：最多露出 [`PINNED_PLAN_ROWS`] 条，
    /// 窗口跟着正在做的那一项走；点标题收成一行（只显示进度和当前项）。
    pub(super) fn render_pinned_plan(&self, cx: &mut Context<Self>) -> Option<Div> {
        let s = self.session();
        let ChatItem::Plan(rows) = &s.transcript[s.pinned_plan()?] else {
            return None;
        };
        if rows.is_empty() {
            return None;
        }
        let folded = s.plan_folded;
        let (_, total, current) = plan_progress(rows);
        let header = plan_header(rows)
            .id("pinned-plan")
            .text_color(rgb(INK_SOFT))
            .cursor_pointer()
            .hover(|d| d.text_color(rgb(INK)))
            .on_click(cx.listener(|this, _, _, cx| {
                let active = this.active;
                let s = &mut this.sessions[active];
                s.plan_folded = !s.plan_folded;
                cx.notify();
            }))
            .child(
                Icon::new(if folded { IconAll::ChevronUp } else { IconAll::ChevronDown }).size_3(),
            );
        let mut card = div()
            .v_flex()
            .gap_1()
            .px_3()
            .py_2()
            .rounded_xl()
            .border_1()
            .border_color(rgb(LINE))
            .bg(rgb(CARD))
            .shadow_xs()
            .child(header);
        if folded {
            card = card.children(current.map(plan_row));
        } else {
            // 窗口：当前项前面留一条已完成的作参照
            let focus = rows.iter().position(|r| r.status != PlanStatus::Done).unwrap_or(total - 1);
            let start = focus.saturating_sub(1).min(total.saturating_sub(PINNED_PLAN_ROWS));
            let end = (start + PINNED_PLAN_ROWS).min(total);
            card = card.children(rows[start..end].iter().map(plan_row));
            let hidden = total - (end - start);
            if hidden > 0 {
                card = card.child(
                    div()
                        .pl(STEP_INDENT)
                        .text_size(TEXT_META)
                        .text_color(rgb(INK_SOFT))
                        .child(format!("另有 {hidden} 项")),
                );
            }
        }
        Some(card)
    }

    /// 切换视图模式并写回设置文件。
    pub(crate) fn set_view_mode(&mut self, mode: ViewMode, cx: &mut Context<Self>) {
        if self.settings.view_mode == mode {
            return;
        }
        // 视图菜单 / 命令面板里两项都勾在内存里的那一项上，切完得让根视图重画
        // （列表那边由 sync_list 的测量指纹自己跟上）
        self.save_ui_pref(move |s| s.view_mode = mode, cx);
        self.view_changed(cx);
    }

    /// 视图模式换过之后：重算对话列表的分组 / 测量指纹（`Session::sync_list` 才看得到新模式）。
    pub(super) fn view_changed(&mut self, cx: &mut Context<Self>) {
        let view = self.settings.view_mode;
        let active = self.active;
        self.sessions[active].sync_list(view);
        cx.notify();
    }

    /// 往上翻过之后，对话区底部居中的「回到底部」圆钮；底下来了新内容时右上角带个赤陶点。
    /// 点一下跳回底部并恢复自动跟随。
    pub(super) fn render_jump_to_bottom(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let s = self.session();
        // 出现 / 消失淡入淡出、略微升起（每帧都要采样，状态才留得住）
        let presence = Presence::new("jump-to-bottom", s.scrolled_away())
            .transition(Transition::new(cx.theme().motion_tokens().duration_fast))
            .sample(window, cx);
        if !presence.should_render() {
            return None;
        }
        let shown = presence.progress;
        let unseen = s.has_unseen();
        let button = div()
            .id("jump-to-bottom")
            .relative()
            .size(px(32.))
            .rounded_full()
            .bg(rgb(CARD))
            .border_1()
            .border_color(rgb(LINE))
            // 浮在对话上的圆按钮：两层阴影（近距接触 + 远距悬浮）
            .shadow(float_shadow())
            .flex()
            .justify_center()
            .items_center()
            .cursor_pointer()
            .hover(|d| d.bg(rgb(HOVER)))
            .tooltip(move |window, cx| {
                Tooltip::new(if unseen { "有新内容，回到底部" } else { "回到底部" })
                    .build(window, cx)
            })
            .opacity(shown)
            .top(px(6.) * (1. - shown))
            .on_click(cx.listener(|this, _, _, cx| this.start_jump_to_bottom(cx)))
            .child(Icon::new(IconAll::ArrowDown).size_4().text_color(rgb(INK_SOFT)))
            .when(unseen, |d| {
                d.child(
                    div()
                        .absolute()
                        .top(px(-1.))
                        .right(px(-1.))
                        .size(px(9.))
                        .rounded_full()
                        .bg(rgb(CORAL))
                        .border_2()
                        .border_color(rgb(PAPER)),
                )
            });
        // 外层只占按钮那一条、不挂事件，不挡两侧的滚轮
        Some(div().absolute().bottom_3().left_0().right_0().flex().justify_center().child(button))
    }

    /// 点「回到底部」：离得远（超过一屏半）先跳到离底一屏的地方，剩下的一屏滑下去，
    /// 到底后恢复自动跟随。减少动态效果时直接到底。
    pub(super) fn start_jump_to_bottom(&mut self, cx: &mut Context<Self>) {
        let s = &mut self.sessions[self.active];
        s.wheel = None;
        let remaining = distance_to_end(&s.list);
        if cx.reduce_motion() || remaining <= px(1.) {
            s.follow_tail();
            cx.notify();
            return;
        }
        let viewport = s.list.viewport_bounds().size.height;
        if remaining > viewport * 1.5 {
            s.list.scroll_by(remaining - viewport);
        }
        let now = Instant::now();
        self.jump = Some(motion::JumpAnim { token: s.token, started: now, last: now });
        cx.notify();
    }

    /// 平滑回到底部的一帧（`render` 开头调）：剩下的距离按指数衰减地滑过去，
    /// 够近了或走了太久就交给自动跟随。切走会话、用户自己滚到底了就停。
    pub(super) fn step_jump_to_bottom(&mut self, window: &mut Window) {
        let Some(mut anim) = self.jump.take() else {
            return;
        };
        let s = &mut self.sessions[self.active];
        if anim.token != s.token || s.list.is_following_tail() {
            return;
        }
        let remaining = distance_to_end(&s.list);
        if remaining <= px(1.) || anim.started.elapsed() > motion::JUMP_MAX {
            s.follow_tail();
            return;
        }
        let now = Instant::now();
        let dt = (now - anim.last).as_secs_f32();
        anim.last = now;
        let k = 1. - (-dt / motion::JUMP_TAU).exp();
        s.list.scroll_by((remaining * k).max(px(1.)).min(remaining));
        self.jump = Some(anim);
        window.request_animation_frame();
    }

    /// 滚轮往末尾滚了 `delta`（负 = 往开头）：只推进目标，由 [`Self::step_wheel`] 每帧滑过去，
    /// 一格不再硬跳 60px。触控板的精确位移本来就连续，和减少动态效果时一样直接滚到位。
    /// 只通知内容区：滚动不影响输入区。
    pub(super) fn wheel_by(&mut self, delta: Pixels, precise: bool, cx: &mut Context<Self>) {
        self.jump = None;
        let s = &mut self.sessions[self.active];
        let top = scroll_top(&s.list);
        let max = s.list.max_offset_for_scrollbar().y.max(px(0.));
        if precise || cx.reduce_motion() {
            s.wheel = None;
            s.list.scroll_by((top + delta).clamp(px(0.), max) - top);
        } else {
            // 接着上一格没滑完的目标往前推；反过来滚时从当前位置算，不先把剩下那段走完
            let base = match s.wheel {
                Some(w) if (w.target > top) == (delta > px(0.)) => w.target,
                _ => top,
            };
            let now = Instant::now();
            // 新的一串从「上一帧」算起，第一帧就走一帧的量，不是从零起步
            let last = s.wheel.map_or(now.checked_sub(FRAME).unwrap_or(now), |w| w.last);
            s.wheel =
                Some(WheelScroll { target: (base + delta).clamp(px(0.), max), started: now, last });
        }
        self.content_view.update(cx, |_, cx| cx.notify());
    }

    /// 滚轮平滑的一帧（`render_content` 开头调）：剩下的距离按指数衰减地滑过去。
    pub(super) fn step_wheel(&mut self, window: &mut Window) {
        let s = &mut self.sessions[self.active];
        let Some(mut w) = s.wheel.take() else {
            return;
        };
        if w.started.elapsed() > motion::WHEEL_MAX {
            return;
        }
        let now = Instant::now();
        let dt = (now - w.last).as_secs_f32().min(0.1);
        w.last = now;
        // 期间有行重测过，可滚的范围会变：目标跟着收进去，免得够不着一直排帧
        w.target = w.target.min(s.list.max_offset_for_scrollbar().y.max(px(0.)));
        let rest = w.target - scroll_top(&s.list);
        let step = rest * (1. - (-dt / motion::WHEEL_TAU).exp());
        if (rest - step).abs() < px(0.5) {
            s.list.scroll_by(rest);
            return;
        }
        s.list.scroll_by(step);
        s.wheel = Some(w);
        window.request_animation_frame();
    }

    /// 鼠标进出对话里的一条消息（下方操作按钮的淡入淡出跟着它）。
    fn set_hovered_msg(&mut self, idx: usize, hovered: bool, cx: &mut Context<Self>) {
        let next = if hovered {
            Some(idx)
        } else if self.hovered_msg == Some(idx) {
            None
        } else {
            self.hovered_msg
        };
        if next != self.hovered_msg {
            self.hovered_msg = next;
            cx.notify();
        }
    }

    /// 「复制」按钮：点了把 `text(当前会话, idx)` 放进剪贴板，图标短暂变成对勾、文字变「已复制」。
    fn copy_action(
        &self,
        id: impl Into<ElementId>,
        idx: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
        text: impl Fn(&Session, usize) -> String + 'static,
    ) -> Button {
        let copied = self.copied == Some((self.session().token, idx, None));
        let (icon, label) =
            if copied { (IconAll::Check, "已复制") } else { (IconAll::Copy, "复制") };
        // 变成对勾时淡入一下，确认点上了
        let pop =
            self.copied_at.filter(|_| copied).and_then(|t| motion::enter(t.elapsed(), window, cx));
        Self::message_action(id, icon, label, cx)
            .when_some(pop, |b, (opacity, _)| b.opacity(opacity))
            .on_click(cx.listener(move |this, _, _, cx| {
                let content = text(this.session(), idx);
                this.copy_to_clipboard(idx, None, content, cx);
            }))
    }

    /// 写剪贴板并记下「刚复制的是哪条（哪个代码块）」，1.5 秒后自动清掉对勾。
    pub(crate) fn copy_to_clipboard(
        &mut self,
        idx: usize,
        block: Option<usize>,
        text: String,
        cx: &mut Context<Self>,
    ) {
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        let key = (self.session().token, idx, block);
        self.copied = Some(key);
        self.copied_at = Some(Instant::now());
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(1500)).await;
            let _ = this.update(cx, |app, cx| {
                if app.copied == Some(key) {
                    app.copied = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// 「编辑」：把这条用户消息放回输入框（输入框里已有草稿就接在后面，不覆盖），光标放到末尾。
    pub(crate) fn edit_message(&mut self, idx: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ChatItem::User { text, .. }) = self.session().transcript.get(idx) else {
            return;
        };
        let text = text.to_string();
        self.input.update(cx, |state, cx| {
            let draft = state.value().to_string();
            let next = if draft.trim().is_empty() { text } else { format!("{draft}\n{text}") };
            let last_line = next.matches('\n').count() as u32;
            state.set_value(next, window, cx);
            // 列号超出会被截到行尾
            state.set_cursor_position(Position::new(last_line, u32::MAX), window, cx);
        });
        cx.notify();
    }

    /// 用户消息气泡；悬停时下方淡入「复制 / 编辑 / 回退 / 在另一个内核上重跑」。刚发出的一条从下方淡入升起。
    fn render_user(
        &self,
        idx: usize,
        text: &SharedString,
        images: &[PathBuf],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        // 回退浮层开着时操作行常显（鼠标移到浮层上就离开了这条消息）
        let rewinding = self.rewind.as_ref().is_some_and(|p| p.item() == idx);
        let fade = motion::hover_opacity(
            ("user-actions", idx),
            self.hovered_msg == Some(idx) || rewinding,
            window,
            cx,
        );
        let current = self.session().kernel.clone();
        // 只给配置过的内核：启用的每一条 agent 档案都是候选（新会话在它上面开）
        let acp_agents: Vec<KernelChoice> =
            self.settings.acp.agents.iter().map(|a| KernelChoice::Acp(a.id.clone())).collect();
        let others = std::iter::once(KernelChoice::Builtin).chain(acp_agents).filter(|k| {
            *k != current
                && match k {
                    KernelChoice::Builtin => self.settings.builtin.is_configured(),
                    KernelChoice::Acp(_) => true,
                    KernelChoice::Demo => false,
                }
        });
        let edit_tip = if images.is_empty() {
            "放回输入框，改完重新发送"
        } else {
            "放回输入框（图片要重新附上），改完重新发送"
        };
        let mut actions = div()
            .h_flex()
            .justify_end()
            .gap_1()
            .opacity(fade)
            .child(self.copy_action(("copy-user", idx), idx, window, cx, |s, idx| {
                match &s.transcript[idx] {
                    ChatItem::User { text, .. } => text.to_string(),
                    _ => String::new(),
                }
            }))
            .child(
                Self::message_action(("edit-user", idx), IconAll::Pencil, "编辑", cx)
                    .tooltip(edit_tip)
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.edit_message(idx, window, cx)),
                    ),
            )
            .child(self.rewind_action(idx, cx));
        for (n, kind) in others.into_iter().enumerate() {
            actions = actions.child(
                Self::message_action(
                    ("rerun", idx * 4 + n),
                    IconAll::RefreshCw,
                    format!("在「{}」上重跑", kind.label(&self.settings.acp)),
                    cx,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    let kind = kind.clone();
                    this.rerun_on(idx, kind, cx)
                })),
            );
        }
        // 气泡：有文字才画文本气泡，图片另起一行缩略图（丢了文件就跳过）。
        let bubble = if text.is_empty() {
            None
        } else {
            Some(
                div()
                    .max_w(relative(0.85))
                    .bg(rgb(SAND))
                    .text_color(rgb(INK))
                    .rounded_2xl()
                    .px_4()
                    .py_2p5()
                    .text_size(TEXT_LARGE)
                    .line_height(LEADING)
                    .child(text.clone()),
            )
        };
        let thumbs = if images.is_empty() {
            None
        } else {
            Some(div().h_flex().justify_end().gap_1p5().flex_wrap().max_w(relative(0.85)).children(
                images.iter().filter(|p| p.exists()).enumerate().map(|(n, p)| {
                    let path = p.clone();
                    // 点缩略图看大图（Esc / 点遮罩 / 右上角 × 关闭）
                    div()
                        .id(("user-image", idx * 16 + n))
                        .cursor_pointer()
                        .rounded_md()
                        .hover(|d| d.opacity(0.85))
                        .on_click(cx.listener(move |_, _, window, cx| {
                            Self::open_image(ImageView::Path(path.clone()), window, cx)
                        }))
                        .child(img(p.clone()).size_20().rounded_md().object_fit(ObjectFit::Cover))
                }),
            ))
        };
        let block = div()
            .id(("user-msg", idx))
            .v_flex()
            .items_end()
            .gap_1()
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                this.set_hovered_msg(idx, *hovered, cx)
            }))
            .children(thumbs)
            .children(bubble)
            .child(actions);
        motion::entering(block, self.session().fresh_user(idx), window, cx)
    }

    pub(super) fn render_item(
        &self,
        idx: usize,
        item: &ChatItem,
        layout: RowLayout,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let verbose = self.settings.view_mode == ViewMode::Verbose;
        match item {
            ChatItem::User { text, images, .. } => self.render_user(idx, text, images, window, cx),
            ChatItem::Thinking { text, streaming, open, elapsed_ms, .. } => Self::render_thinking(
                idx,
                text,
                *streaming,
                *open,
                *elapsed_ms,
                verbose,
                window,
                cx,
            ),
            ChatItem::Info { text, open } => {
                Self::render_info(idx, text, *open, verbose, window, cx)
            }
            ChatItem::Tool(card) if card.task => self.render_task_card(idx, card, window, cx),
            ChatItem::Tool(card) => {
                Self::render_tool(idx, card, &self.session().cwd, verbose, window, cx)
            }
            ChatItem::AutoApproved { title, chosen } => {
                Self::render_auto_approved(idx, title, chosen)
            }
            ChatItem::Permission { title, options, resolved, hunks, keep, .. } => {
                let keyed = self.session().pending_permission() == Some(idx);
                Self::render_permission(
                    idx,
                    title,
                    options,
                    resolved,
                    hunks.as_ref(),
                    keep.as_ref(),
                    keyed,
                    cx,
                )
            }
            ChatItem::Question { questions, picks, answer, .. } => {
                let keyed = self.session().pending_question() == Some(idx);
                Self::render_question(idx, questions, picks, answer, keyed, cx)
            }
            ChatItem::Plan(rows) => Self::render_plan(rows, layout.compact),
            ChatItem::Agent(text) => {
                let s = self.session();
                let streaming = s.busy && idx + 1 == s.transcript.len();
                let text = reading_markdown(text, streaming);
                // 代码块右上角：语言名 + 「复制」（点完短暂变对勾，和消息下方的复制同一套状态）
                let token = s.token;
                let copied_block = match self.copied {
                    Some((t, i, block)) if t == token && i == idx => block,
                    _ => None,
                };
                let copied_at = self.copied_at;
                let app = cx.entity().downgrade();
                let body = div().min_w_0().text_size(TEXT_LARGE).line_height(LEADING).child(
                    TextView::markdown(("agent", idx), text)
                        .style(markdown_style())
                        .markdown_extensions(crate::app::math::extensions())
                        .stream_fade(true)
                        .code_block_actions(move |block, window, cx| {
                            let start = block.span.map(|span| span.start).unwrap_or(0);
                            let code = block.code();
                            let app = app.clone();
                            code_block_header(
                                block.lang(),
                                copied_at.filter(|_| copied_block == Some(start)),
                                move |_, _, cx| {
                                    let _ = app.update(cx, |this, cx| {
                                        this.copy_to_clipboard(
                                            idx,
                                            Some(start),
                                            code.to_string(),
                                            cx,
                                        )
                                    });
                                },
                                window,
                                cx,
                            )
                        }),
                );
                if !s.is_turn_end(idx) {
                    return body;
                }
                // 一轮回复的末尾挂「复制」和这一轮的用时 / 用量：最后一轮常显，
                // 更早的悬停才淡入（只改透明度，不挤布局）
                let latest =
                    !s.transcript[idx + 1..].iter().any(|i| matches!(i, ChatItem::User { .. }));
                let hovered = self.hovered_msg == Some(idx);
                let fade = (!latest)
                    .then(|| motion::hover_opacity(("agent-actions", idx), hovered, window, cx));
                let meta = s.turn_summary(idx).and_then(super::status::turn_meta);
                let files = s.turn_files(idx);
                let has_files = !files.is_empty();
                let actions = div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .pt_2()
                    .child(self.copy_action(("copy-agent", idx), idx, window, cx, |s, idx| {
                        s.turn_text(idx)
                    }))
                    .children(meta.map(|m| {
                        // 和运行中的状态行同一套数字字形，一轮结束换过来时不变样
                        div()
                            .text_size(TEXT_META)
                            .text_color(rgb(INK_SOFT))
                            .font_features(tabular_nums())
                            .child(m)
                    }))
                    .when_some(fade, |d, opacity| d.opacity(opacity));
                // 这一轮改过文件：正文和操作行之间一张改动卡片（常显，不跟着悬停淡入淡出）
                let changes = has_files
                    .then(|| div().pt_3().child(self.render_changes_card(idx, files, latest, cx)));
                div().child(
                    div()
                        .id(("agent-msg", idx))
                        .v_flex()
                        .min_w_0()
                        .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                            this.set_hovered_msg(idx, *hovered, cx)
                        }))
                        .child(body)
                        .children(changes)
                        .child(actions),
                )
            }
            ChatItem::Failed { message, kind } => self.render_failed(idx, message, *kind, cx),
            ChatItem::Compaction { state, open } => {
                Self::render_compaction(idx, state, *open, window, cx)
            }
            // 一轮层面的提示（续接、停止…）：和正文左对齐，图标打头
            ChatItem::Note(text) => {
                step_line(Icon::new(IconAll::Info).size_3p5().text_color(rgb(INK_SOFT)))
                    .child(div().flex_1().min_w_0().child(text.clone()))
            }
        }
    }
}

/// 显示用路径拆成（文件名，所在目录）；目录为空（就在工作目录下）时为 None。
fn split_path(shown: &str) -> (String, Option<String>) {
    let dir = std::path::Path::new(shown)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .filter(|p| !p.is_empty());
    (file_name(shown), dir)
}

/// 「新文件」小标签（改动卡片、diff 卡片表头）：描边不填底，卡片底和沙色底上都看得清
fn new_file_tag() -> Div {
    div()
        .flex_shrink_0()
        .px_1p5()
        .rounded_sm()
        .border_1()
        .border_color(rgb(LINE_STRONG))
        .text_size(TEXT_META)
        .text_color(rgb(INK_SOFT))
        .child("新文件")
}

/// 改动卡片先列几个文件（多一个时直接全列，不为一行折叠）
const CHANGES_PREVIEW: usize = 5;

/// diff 的一行：行号、+/- 标记、内容；`'~'` 是两个 hunk 之间省略的部分。
pub(super) fn diff_line(line: &claudesktop_core::DiffLineView) -> Div {
    if line.tag == '~' {
        return div()
            .h(px(18.))
            .pl(px(40.))
            .text_size(TEXT_META)
            .text_color(rgb(INK_FAINT))
            .bg(rgb(SAND))
            .child("⋯");
    }
    let (bg, fg) = match line.tag {
        '+' => (Some(DIFF_ADD_BG), DIFF_ADD_FG),
        '-' => (Some(DIFF_DEL_BG), DIFF_DEL_FG),
        _ => (None, INK_SOFT),
    };
    div()
        .h_flex()
        .items_start()
        .when_some(bg, |d, c| d.bg(rgb(c)))
        .child(
            div()
                .w(px(40.))
                .flex_shrink_0()
                .pr_2()
                .flex()
                .justify_end()
                .text_color(rgb(INK_FAINT))
                .child(format!("{}", line.no)),
        )
        .child(div().w(px(14.)).flex_shrink_0().text_color(rgb(fg)).child(line.tag.to_string()))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .pr_3()
                .text_color(rgb(if bg.is_some() { fg } else { INK }))
                .child(line.text.clone()),
        )
}

/// 列表离底部还有多远（按已量过的行高估）。
/// 一帧的时长（滚轮平滑起步时用）
const FRAME: Duration = Duration::from_millis(16);

/// 列表当前滚到离顶部多远（像素）。
fn scroll_top(list: &ListState) -> Pixels {
    -list.scroll_px_offset_for_scrollbar().y
}

fn distance_to_end(list: &ListState) -> Pixels {
    (list.max_offset_for_scrollbar().y + list.scroll_px_offset_for_scrollbar().y).max(px(0.))
}

/// 回复 / 思考 / 摘要交给 `TextView` 前的整理：流式中补齐半截标记，把 `\( … \)` / `\[ … \]` 这类
/// 写法统一成 `$…$`（代码里不动，见 `math::normalize_markup`），再收掉全角标点旁的空格。
pub(super) fn reading_markdown(text: &str, streaming: bool) -> SharedString {
    let text = if streaming { close_dangling_markup(text) } else { Cow::Borrowed(text) };
    let text = claudesktop_core::math::normalize_markup(&text);
    SharedString::from(tidy_cjk_spacing(&text).into_owned())
}

/// 流式输出停在半截标记上时（`` `foo `` 或没收尾的 ``` 围栏、`**粗体`），渲染前临时补齐，
/// 免得标记原样露出来、等收尾后整段又跳成代码 / 粗体样式。
pub(super) fn close_dangling_markup(text: &str) -> Cow<'_, str> {
    let mut in_fence = false;
    // 当前段落（围栏外）里的反引号个数；行内代码不跨段落
    let mut ticks = 0;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            ticks = 0;
        } else if !in_fence {
            if line.trim().is_empty() {
                ticks = 0;
            } else {
                ticks += line.matches('`').count();
            }
        }
    }
    if in_fence {
        Cow::Owned(format!("{text}\n```"))
    } else if ticks % 2 == 1 {
        Cow::Owned(format!("{text}`"))
    } else {
        close_dangling_bold(text)
    }
}

/// 全角标点（自带半个字宽的留白）：旁边再有半角空格，间隙就显得比别处宽。
fn is_fullwidth_punct(c: char) -> bool {
    "，。、；：？！（）【】《》「」『』〔〕〈〉〖〗".contains(c)
}

/// 渲染前去掉紧挨全角标点的半角空格（`提交： \`a\``、`grants （24`），全角标点自己的留白够了。
/// 代码（围栏、行内）原样不动；行首的缩进和块标记（`- `、`1. `、`> `、`# `）、行尾空格（硬换行）不动，
/// 只删两边都有字的行中空格。复制仍取原文。
pub(super) fn tidy_cjk_spacing(text: &str) -> Cow<'_, str> {
    if !text.chars().any(is_fullwidth_punct) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut in_fence = false;
    for line in text.split_inclusive('\n') {
        let fence = line.trim_start();
        if fence.starts_with("```") || fence.starts_with("~~~") {
            in_fence = !in_fence;
            out.push_str(line);
            continue;
        }
        if in_fence {
            out.push_str(line);
            continue;
        }
        let start = block_prefix_len(line);
        out.push_str(&line[..start]);
        tidy_line(&line[start..], &mut out);
    }
    if out == text { Cow::Borrowed(text) } else { Cow::Owned(out) }
}

/// 行首缩进 + 块标记（引用、列表、标题）连同后面的空格一共多少字节：这段不碰，
/// 否则 `- （注）` 删了空格就不再是列表。
fn block_prefix_len(line: &str) -> usize {
    let bytes = line.as_bytes();
    let spaces = |mut i: usize| {
        while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
            i += 1;
        }
        i
    };
    let mut i = spaces(0);
    loop {
        let rest = &bytes[i..];
        let marker = match rest.first() {
            Some(b'>') => 1,
            Some(b'-' | b'*' | b'+') if rest.get(1).is_some_and(|b| *b == b' ') => 1,
            Some(b'#') => {
                let n = rest.iter().take_while(|b| **b == b'#').count();
                if rest.get(n).is_some_and(|b| *b == b' ') { n } else { 0 }
            }
            Some(b'0'..=b'9') => {
                let n = rest.iter().take_while(|b| b.is_ascii_digit()).count();
                let dot = rest.get(n).is_some_and(|b| *b == b'.' || *b == b')');
                if dot && rest.get(n + 1).is_some_and(|b| *b == b' ') { n + 1 } else { 0 }
            }
            _ => 0,
        };
        if marker == 0 {
            return i;
        }
        i = spaces(i + marker);
    }
}

/// 一行正文（块标记之后）：行内代码原样抄，代码外删紧挨全角标点的空格。
fn tidy_line(line: &str, out: &mut String) {
    let mut rest = line;
    // 上一个写出去的字（代码段算普通字，不是全角标点）
    let mut prev: Option<char> = None;
    while let Some(c) = rest.chars().next() {
        if c == '`' {
            let ticks = rest.len() - rest.trim_start_matches('`').len();
            let fence = &rest[..ticks];
            // 找恰好同样长度的一串反引号收尾；找不到就当普通字符
            let body = &rest[ticks..];
            let close = body.match_indices(fence).find(|(at, _)| {
                !body[at + ticks..].starts_with('`') && !body[..*at].ends_with('`')
            });
            let span = match close {
                Some((at, _)) => ticks + at + ticks,
                None => ticks,
            };
            out.push_str(&rest[..span]);
            prev = Some('`');
            rest = &rest[span..];
            continue;
        }
        if c == ' ' {
            let run = rest.len() - rest.trim_start_matches(' ').len();
            let next = rest[run..].chars().next().filter(|n| *n != '\n' && *n != '\r');
            let squeeze = match (prev, next) {
                (Some(p), Some(n)) => is_fullwidth_punct(p) || is_fullwidth_punct(n),
                _ => false,
            };
            if !squeeze {
                out.push_str(&rest[..run]);
            }
            rest = &rest[run..];
            continue;
        }
        out.push(c);
        prev = Some(c);
        rest = &rest[c.len_utf8()..];
    }
}

/// 最后一行里没收尾的 `**`：后面已经有字就补上收尾（先去掉末尾空白，否则不算收尾），
/// 后面还没字（`- **`）就先不显示这个标记。只看最后一行，行内代码里的星号不算。
fn close_dangling_bold(text: &str) -> Cow<'_, str> {
    let start = text.rfind('\n').map_or(0, |i| i + 1);
    // 按反引号切开，偶数段在行内代码外
    let (mut count, mut open, mut at) = (0, None, start);
    for (i, part) in text[start..].split('`').enumerate() {
        if i % 2 == 0 {
            for (j, _) in part.match_indices("**") {
                count += 1;
                open = Some(at + j);
            }
        }
        at += part.len() + 1;
    }
    match open {
        Some(open) if count % 2 == 1 && text[open + 2..].trim().is_empty() => {
            Cow::Borrowed(&text[..open])
        }
        Some(_) if count % 2 == 1 => Cow::Owned(format!("{}**", text.trim_end())),
        _ => Cow::Borrowed(text),
    }
}

/// 权限请求的标题：内核常把命令写成行内代码（`` `ls -la` ``），这里本来就用等宽字，去掉包着的反引号。
fn plain_title(title: &SharedString) -> SharedString {
    let trimmed = title.trim();
    match trimmed.strip_prefix('`').and_then(|t| t.strip_suffix('`')) {
        Some(inner) if !inner.is_empty() && !inner.contains('`') => inner.to_string().into(),
        _ => title.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[core::prelude::v1::test]
    fn dangling_code_is_closed_while_streaming() {
        assert_eq!(close_dangling_markup("事件来自 ACP 的 `sess"), "事件来自 ACP 的 `sess`");
        assert_eq!(close_dangling_markup("用 `a` 和 `b`"), "用 `a` 和 `b`");
        assert_eq!(close_dangling_markup("```rust\nfn a() {"), "```rust\nfn a() {\n```");
        // 上一段的单个反引号不影响下一段
        assert_eq!(close_dangling_markup("a ` b\n\nc"), "a ` b\n\nc");
        assert_eq!(close_dangling_markup("```\n`\n```\nok"), "```\n`\n```\nok");
    }

    #[core::prelude::v1::test]
    fn dangling_bold_is_closed_or_hidden_while_streaming() {
        assert_eq!(close_dangling_markup("- **思考"), "- **思考**");
        assert_eq!(close_dangling_markup("- **思考 "), "- **思考**");
        // 标记后面还没字：先藏起来
        assert_eq!(close_dangling_markup("说明：\n- **"), "说明：\n- ");
        assert_eq!(close_dangling_markup("**a** 和 **b"), "**a** 和 **b**");
        assert_eq!(close_dangling_markup("**a** 和 **b**"), "**a** 和 **b**");
        // 行内代码里的星号、上一行的星号不算
        assert_eq!(close_dangling_markup("用 `**` 表示粗体"), "用 `**` 表示粗体");
        assert_eq!(close_dangling_markup("**a\nb"), "**a\nb");
    }

    /// 中文避头尾是 vendor 里 gpui-pre 的补丁（见 `vendor/README.md`）：升级 gpui-kit 时补丁丢了这里会挂。
    /// 用 `NoopTextSystem`（每个字等宽）驱动真的 `LineWrapper`。
    #[core::prelude::v1::test]
    fn cjk_wrap_follows_kinsoku() {
        let system = std::sync::Arc::new(TextSystem::new(std::sync::Arc::new(NoopTextSystem)));
        let mut wrapper = system.line_wrapper(font("Inter"), px(16.));
        // 第一行之后各行的开头：按 `fits` 那么宽折行（`NoopTextSystem` 里每个字都是 0.6em）
        let mut wrap = |text: &str, fits: &str| {
            let width = px(16. * 0.6 * fits.chars().count() as f32 + 0.5);
            wrapper
                .wrap_line(&[LineFragment::text(text)], width, IndentAdjustment::default())
                .map(|boundary| text[boundary.ix..].to_string())
                .collect::<Vec<_>>()
        };
        // 普通汉字之间照样能断
        assert_eq!(wrap("一二三四五六", "一二三"), ["四五六"]);
        // 收尾标点不落行首：前一个字跟着下去
        assert_eq!(wrap("一二三，四", "一二三"), ["三，四"]);
        assert_eq!(wrap("一二三）、", "一二三"), ["三）、"]);
        // 开头标点不留行尾
        assert_eq!(wrap("一二三（四五", "一二三（"), ["（四五"]);
        // `——` 不断开
        assert_eq!(wrap("一二——三", "一二—"), ["——三"]);
    }

    #[core::prelude::v1::test]
    fn spaces_beside_fullwidth_punct_are_squeezed() {
        let tidy = |s| tidy_cjk_spacing(s).into_owned();
        assert_eq!(
            tidy("- 提交： `8486c35` （24 个文件，+12 / −2）"),
            "- 提交：`8486c35`（24 个文件，+12 / −2）"
        );
        assert_eq!(tidy("通过 。下一步"), "通过。下一步");
        // 没有全角标点：原样
        assert!(matches!(tidy_cjk_spacing("a  b `c`"), Cow::Borrowed(_)));
        // 块标记后面的空格、缩进、行尾硬换行不动
        assert_eq!(
            tidy("- （注）\n1. 「a」\n> ：b\n## 《c》\n  - （d）"),
            "- （注）\n1. 「a」\n> ：b\n## 《c》\n  - （d）"
        );
        assert_eq!(tidy("完成。  \n下一行"), "完成。  \n下一行");
        // 代码里的不动
        assert_eq!(tidy("用 `a ，b` 和 ``x （`y`） z`` 。"), "用 `a ，b` 和 ``x （`y`） z``。");
        assert_eq!(
            tidy("```\nlet s = \"a ，b\";\n```\n完 。"),
            "```\nlet s = \"a ，b\";\n```\n完。"
        );
        // 没收尾的反引号当普通字
        assert_eq!(tidy("单个 ` ，号"), "单个 `，号");
    }
}

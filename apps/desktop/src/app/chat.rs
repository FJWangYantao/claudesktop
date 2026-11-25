//! 对话记录渲染：过程组（思考 / 工具 / 自动批准）、diff、权限卡、计划、正文、备注。
//!
//! 层级：回复正文最显眼；过程步骤一律一行灰色小字（左侧固定宽的图标格对齐），
//! 连续的几步收进可折叠的组（分组规则在 `Session::compute_layout`）。

use super::ChatApp;
use super::body_cache::{AgentBodyView, BodyKind, CodeCopy};
use super::menu::{MenuList, diff_stat, menu_item, menu_panel};
use super::motion::{self, Reveal};
use super::rewind::file_name;
use super::widgets::Surface;
use crate::model::*;
use crate::session::{RowLayout, Session, StepGroup, WheelInput};
use crate::settings::KernelChoice;
use crate::theme::rgb;
use crate::theme::*;
use claudesktop_core::{DiffView, PermissionHunks, PermissionOptionView};
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::base::motion::{Presence, Transition};
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::input::Position;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::popover::Popover;
use gpui_kit::component::scroll::{Scrollbar, ScrollbarMode};
use gpui_kit::component::text::TextViewStyle;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// 同组相邻两步之间的行距
pub(super) const STEP_GAP: Rems = rems(0.375);
/// 已定型正文的渲染缓存条数（见 `reading_markdown`）
const MARKDOWN_CACHE_LEN: usize = 256;

thread_local! {
    /// (原文哈希, 原文, 渲染结果)
    static MARKDOWN_CACHE: std::cell::RefCell<Vec<(u64, String, SharedString)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}
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
    /// 工具结果里带回来的图（`read_file` 读到的、MCP 返回的）：资源表按内容寻址
    Memory { name: String, ext: &'static str },
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
            ImageView::Memory { name, .. } => {
                img(name.clone()).size_full().object_fit(ObjectFit::ScaleDown).into_any_element()
            }
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
        ImageView::Memory { name, ext } => match crate::assets::resource_bytes(name) {
            Some(bytes) => (Some(bytes), format!("tool-image.{ext}")),
            None => (None, "读不了这张图：资源已不可用".into()),
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
pub(super) fn first_line(text: &str) -> SharedString {
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
    // 自动放行保持安静：不画徽章（失败的工具行自己会说明被拦下的原因）
    if grant.kind == GrantKind::Auto {
        return None;
    }
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
    markdown_styles(false)
}

pub(super) fn think_markdown_style() -> TextViewStyle {
    markdown_styles(true)
}

fn markdown_styles(thinking: bool) -> TextViewStyle {
    type StyleKey = (u32, usize, bool);
    thread_local! {
        static STYLES: std::cell::RefCell<Option<(StyleKey, TextViewStyle, TextViewStyle)>> =
            const { std::cell::RefCell::new(None) };
    }
    let key = (f32::from(root_font_size()).to_bits(), palette_index(), is_dark());
    STYLES.with_borrow_mut(|cache| {
        if cache.as_ref().is_none_or(|(cached, _, _)| *cached != key) {
            let style = build_markdown_style();
            let think = style.clone().paragraph_gap(rems(0.75));
            *cache = Some((key, style, think));
        }
        let (_, agent, think) = cache.as_ref().unwrap();
        if thinking { think.clone() } else { agent.clone() }
    })
}

fn build_markdown_style() -> TextViewStyle {
    TextViewStyle::default()
        .paragraph_gap(rems(1.25))
        // 标题接口只收 Pixels，所以按当前缩放换算一次
        // 正文 15px：一级 24 / 二级 20 / 三级 17，四级起和正文同大、只靠加粗分（相邻两档拉开 3px 以上，
        // 长回复扫一眼能看出分节）。标题上方的段间距由组件按标题字号算，字号拉开留白也跟着变大
        .heading_font_size(|level, _| {
            match level {
                1 => TEXT_DISPLAY,
                2 => TEXT_HEADING,
                3 => TEXT_LEAD,
                _ => TEXT_BODY,
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
pub(super) fn code_block_header(
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
            col = col.child(if layout.member {
                // 展开着也一直挂着（即使进度已是 1.0）：Reveal 要量过一次高度，收起的第一帧才不会按 0 高塌一下
                Reveal::new(("step-body", ix), reveal, body).into_any_element()
            } else {
                body.into_any_element()
            });
        }
        // 这一轮没有正文回复（被打断，或者只跑了工具）：回合末尾的改动卡片和用时挂在最后一步下面。
        // 作为顶层的兄弟节点，不跟组内步骤缩进，也不受 `Reveal` 收起裁切的影响
        if !matches!(item, ChatItem::Agent(_))
            && self.session().is_turn_fallback_tail(ix, self.settings.view_mode)
        {
            let (latest, meta, tip, files) = {
                let s = self.session();
                let latest =
                    !s.transcript[ix + 1..].iter().any(|i| matches!(i, ChatItem::User { .. }));
                let summary = s.turn_summary(ix);
                (
                    latest,
                    summary.and_then(super::status::turn_meta),
                    summary.and_then(|t| super::status::turn_meta_tooltip(&t.usage)),
                    s.turn_files(ix),
                )
            };
            if !files.is_empty() {
                col =
                    col.child(div().pt_3().child(self.render_changes_card(ix, files, latest, cx)));
            }
            if let Some(meta) = meta {
                let row = div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .pt_1()
                    .text_size(TEXT_META)
                    .text_color(rgb(INK_SOFT))
                    .font_features(tabular_nums());
                col = col.child(match tip {
                    Some(tip) => row
                        .id(("fallback-meta", ix))
                        .tooltip(move |window, cx| text_tip(&tip, window, cx))
                        .child(meta)
                        .into_any_element(),
                    None => row.child(meta).into_any_element(),
                });
            }
        }
        // 用户消息和回合末尾正文下面已经挂着操作行，块间距收成 STEP_GAP，避免每轮留出过多空白
        let gap = if matches!(item, ChatItem::User { .. })
            || (matches!(item, ChatItem::Agent(_)) && self.session().is_turn_end(ix))
            || (!matches!(item, ChatItem::Agent(_))
                && self.session().is_turn_fallback_tail(ix, self.settings.view_mode))
        {
            if layout.tight { Rems::ZERO } else { STEP_GAP }
        } else {
            Self::row_gap(layout)
        };
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
        let view = self.settings.view_mode;
        // 摘要模式下过程行本来就不画，组头仍按全部步骤统计（它就是那一栏的全部信息）
        let (summary, failed) = group_summary(
            s.transcript[ix..group.end]
                .iter()
                .filter(|i| i.has_content() && (view == ViewMode::Summary || i.visible_in(view))),
        );
        let toggle = view != ViewMode::Verbose;
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
                        this.sessions[active].pin_row(ix);
                        this.sessions[active].toggle_group(ix);
                        cx.notify();
                    },
                ))
            })
            .child(icon_cell(if group.live {
                Icon::new(IconAll::Activity).size_3p5().text_color(rgb(CORAL))
            } else {
                Icon::new(IconAll::ListChecks).size_3p5()
            }))
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
        &self,
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
        let header = step_line(Icon::new(IconAll::Sparkles).size_3p5().text_color(rgb(INK_SOFT)))
            .id(("think-toggle", idx))
            .when(toggle, |d| d.role(Role::Button).aria_expanded(open))
            .when(toggle, |d| {
                d.cursor_pointer().hover(|d| d.text_color(rgb(INK))).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.sessions[this.active].pin_row(idx);
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
            let body = div().pt_1().pl(STEP_INDENT).child(self.markdown_body(
                idx,
                BodyKind::Think,
                text,
                streaming,
                None,
                window,
                cx,
            ));
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
                        this.sessions[this.active].pin_row(idx);
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
        &self,
        idx: usize,
        card: &ToolCard,
        cwd: &Path,
        verbose: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        if card.diffs.is_empty() {
            return self.render_tool_plain(idx, card, verbose, window, cx);
        }
        // 文件改动：默认每份只显示前若干行，超出的部分展开 / 收起走弹簧
        let preview = (!verbose).then_some(DIFF_PREVIEW_ROWS);
        let rest = motion::reveal_progress(("diff", idx), card.expanded || verbose, window, cx);
        let settle = motion::settle(("tool", idx), card.phase != ToolPhase::Running, window, cx);
        let mut col = div().v_flex().gap_2();
        for (k, diff) in card.diffs.iter().enumerate() {
            let rest = (ElementId::from(("diff-rest", idx * 64 + k)), rest);
            col = col.child(Self::render_diff(
                diff,
                cwd,
                preview,
                Some(rest),
                Some((idx * 1024 + k, card, settle)),
            ));
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
        // ACP 一个工具可以同时给 diff、文字和图片，不能只画 diff
        if !card.content.trim().is_empty() || !card.images.is_empty() {
            col = col.child(self.tool_output(idx, card, window, cx));
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
                        this.sessions[this.active].pin_row(idx);
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
        &self,
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
                        this.sessions[this.active].pin_row(idx);
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
            col = col.child(Reveal::new(
                ("tool-body", idx),
                progress,
                self.tool_output(idx, card, window, cx),
            ));
        }
        col
    }

    /// 工具行点开后的内容（等宽、卡片底），缩进到工具名下面；和标题之间的空隙算在里面。
    /// 命令先列完整命令（`$` 开头，折行），下面是输出（跑着时是到目前为止的最后几行），
    /// 命令和输出各挂一个「复制」；输出超过一屏高就在卡片里滚，别把整段都铺进对话流。
    pub(super) fn tool_output(
        &self,
        idx: usize,
        card: &ToolCard,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        // 复制按钮的状态按「哪条消息的哪个块」记：命令和输出各占一个不会和代码块撞的编号
        const CMD_BLOCK: usize = usize::MAX;
        const OUT_BLOCK: usize = usize::MAX - 1;
        // 空行给个空格占住行高
        let line =
            |l: &str| div().child(if l.is_empty() { " ".to_string() } else { l.to_string() });
        let output = if card.content.trim().is_empty() {
            card.output.trim_matches('\n')
        } else {
            card.content.trim_matches('\n')
        };
        let running = card.phase == ToolPhase::Running;
        let command = card.shows_command().then(|| {
            let lines: Vec<&str> = card.arg.trim().lines().collect();
            let shown = lines.len().min(COMMAND_PREVIEW_LINES);
            div()
                .v_flex()
                .text_color(rgb(INK))
                .child(
                    div()
                        .h_flex()
                        .items_start()
                        .gap_2()
                        .child(div().flex_1().min_w_0().v_flex().children(
                            lines[..shown].iter().enumerate().map(|(i, l)| {
                                line(&if i == 0 { format!("$ {l}") } else { (*l).to_string() })
                            }),
                        ))
                        .child(self.tool_copy(
                            ("tool-copy-cmd", idx),
                            (idx, CMD_BLOCK),
                            card.arg.trim().to_string(),
                            "复制命令",
                            window,
                            cx,
                        )),
                )
                .when(lines.len() > shown, |d| {
                    d.child(
                        div()
                            .text_color(rgb(INK_SOFT))
                            .child(format!("… 其余 {} 行未显示", lines.len() - shown)),
                    )
                })
        });
        let has_command = command.is_some();
        // 工具带回来的图片（`read_file` 读到的图、MCP 的多模态结果）：点开看大图
        let thumbs = (!card.images.is_empty()).then(|| {
            div().h_flex().gap_1p5().flex_wrap().children(
                card.images.iter().cloned().enumerate().map(|(n, thumb)| {
                    let big = ImageView::Memory { name: thumb.name.clone(), ext: thumb.ext };
                    div()
                        .id(("tool-image", n))
                        .cursor_pointer()
                        .rounded_md()
                        .hover(|d| d.opacity(0.85))
                        .on_click(move |_, window, cx| open_image(big.clone(), window, cx))
                        .child(
                            img(thumb.name.clone())
                                .image_cache(&self.image_cache)
                                .h_20()
                                .max_w_48()
                                .rounded_md()
                                .object_fit(ObjectFit::Cover),
                        )
                }),
            )
        });
        let output_body = (!output.is_empty()).then(|| {
            let text = output.to_string();
            // 长输出在卡片里滚：对话列表的滚轮捕获层会截走竖向滚轮，所以把区域和句柄登记给
            // `ChatApp`，指针在上面时让它先滚（见 `content.rs` 的滚轮处理）
            let scroll = window
                .use_keyed_state(("tool-out-scroll", idx), cx, |_, _| ScrollHandle::new())
                .read(cx)
                .clone();
            let regions = self.tool_scroll_regions.clone();
            let paint_handle = scroll.clone();
            div()
                .v_flex()
                // 命令和输出之间一条细线；只有输出时不用画
                .when(has_command, |d| d.border_t_1().border_color(rgb(LINE)).pt_2())
                .child(
                    div()
                        .relative()
                        .child(
                            div()
                                .id(("tool-out-scroll", idx))
                                .max_h(rems(18.))
                                .overflow_y_scroll()
                                .track_scroll(&scroll)
                                // 常显的滚动条占右侧一条：文字别铺到它底下
                                .pr_4()
                                .child(div().v_flex().children(output.lines().map(line))),
                        )
                        .child(
                            canvas(
                                move |bounds, window, _| {
                                    // 只登记真正露在视口里的那部分：卡片滚出去一半时，
                                    // 被裁掉的那半不还能命中滚轮
                                    let visible = bounds.intersect(&window.content_mask().bounds);
                                    regions.borrow_mut().push((visible, paint_handle.clone(), idx))
                                },
                                |_, _, _, _| {},
                            )
                            .absolute()
                            .inset_0(),
                        )
                        .child(Scrollbar::vertical(&scroll).mode(ScrollbarMode::Always)),
                )
                .child(div().h_flex().justify_end().child(self.tool_copy(
                    ("tool-copy-out", idx),
                    (idx, OUT_BLOCK),
                    text,
                    "复制输出",
                    window,
                    cx,
                )))
        });
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
                .children(thumbs)
                .children(output_body)
                .when(has_command && output.is_empty() && running, |d| {
                    d.child(div().text_color(rgb(INK_SOFT)).child("运行中，还没有输出…"))
                }),
        )
    }

    /// 指针落在工具输出上：在 capture 阶段直接把这一格滚掉。返回 `None` 表示内层吃掉了，
    /// 返回 `Some(剩余)` 表示该交给外层列表（没有任何内层命中，或者这一格粘滞期已过、内层已在尽头）。
    ///
    /// 滚到底之后有 250ms 的粘滞期：同一行继续滚仍然吞掉，不穿透到外层（`overscroll-behavior: contain`
    /// 的等价物）；停顿之后再滚才交还。交还时调用方仍要 `stop_propagation()`，否则
    /// `overflow_y_scroll` 和列表的 bubble 监听器都会再滚一次。
    pub(super) fn scroll_tool_output(
        &mut self,
        pos: Point<Pixels>,
        dy: Pixels,
        cx: &mut Context<Self>,
    ) -> Option<Pixels> {
        /// 内层到达尽头之后的粘滞时长
        const NESTED_LOCK: Duration = Duration::from_millis(250);
        let now = Instant::now();
        let hit = self.tool_scroll_regions.borrow().iter().find_map(|(bounds, handle, idx)| {
            bounds.contains(&pos).then(|| (handle.clone(), *idx))
        });
        let Some((handle, idx)) = hit else {
            return Some(dy);
        };
        // 滚到底的路上别越界：越界后滚轮还要还给外层列表
        let offset = handle.offset().y;
        let max = handle.max_offset().y.max(px(0.));
        let next = (offset + dy).max(-max).min(px(0.));
        if next != offset {
            handle.set_offset(point(handle.offset().x, next));
            // 外层平滑滚动的余量作废：这一格给了内层，别再补到对话上
            self.sessions[self.active].wheel = None;
            self.nested_scroll_lock = Some((idx, now + NESTED_LOCK));
            self.content_view.update(cx, |_, cx| cx.notify());
            // 这一格已经消费在内层（含刚撞到边界的那一格），整格吞掉；
            // 余量留给下一次输入，等粘滞期过了再交还外层，别在边界处先弹一下再卡住
            return None;
        }
        if let Some((locked, until)) = self.nested_scroll_lock
            && locked == idx
            && now < until
        {
            // 还在同一次连续滚动里：吞掉，粘滞期跟着续
            self.nested_scroll_lock = Some((idx, now + NESTED_LOCK));
            return None;
        }
        self.nested_scroll_lock = None;
        Some(dy)
    }

    /// 工具输出里的「复制」：和消息下方的复制共用同一份「刚复制了什么」状态。
    /// `at` 是（这一条消息的下标, 块编号）。
    fn tool_copy(
        &self,
        id: impl Into<ElementId>,
        at: (usize, usize),
        text: String,
        tip: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Button {
        let (idx, block) = at;
        let copied = self.copied == Some((self.session().token, idx, Some(block)));
        let (icon, tip) = if copied { (IconAll::Check, "已复制") } else { (IconAll::Copy, tip) };
        let pop =
            self.copied_at.filter(|_| copied).and_then(|t| motion::enter(t.elapsed(), window, cx));
        Self::quiet_button(id, icon, Surface::Sand, cx)
            .xsmall()
            .tooltip(tip)
            .when_some(pop, |b, (opacity, _)| b.opacity(opacity))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.copy_to_clipboard(idx, Some(block), text.clone(), cx);
            }))
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
        &self,
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
                    .child(self.markdown_body(
                        idx,
                        BodyKind::Compaction,
                        summary,
                        false,
                        None,
                        window,
                        cx,
                    )),
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
                                    .child(permission_prompt(title)),
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

    /// 提问（`ask_user`）：交互在输入框正上方的面板里（`composer.rs` 的
    /// `render_question_panel`），对话流里只留一行说明问过什么；答完这一行写上所选的。
    pub(super) fn render_question(
        idx: usize,
        questions: &[claudesktop_core::QuestionView],
        answer: &Option<claudesktop_core::QuestionAnswer>,
    ) -> Div {
        let first: SharedString =
            questions.first().map(|q| q.question.clone()).unwrap_or_default().into();
        let picked = answer.as_ref().map(|answer| match answer {
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
        });
        match picked {
            Some(picked) => {
                step_line(Icon::new(IconAll::MessageSquare).size_3p5().text_color(rgb(INK_SOFT)))
                    .child(div().flex_shrink_0().child("已回答"))
                    .child(ellipsis(("question-title", idx), first))
                    .child(ellipsis(("question-picked", idx), format!("· {picked}").into()))
            }
            // 还没答：面板挂在输入框上方，这里只说一句去哪选
            None => step_line(Icon::new(IconAll::MessageSquare).size_3p5().text_color(rgb(CORAL)))
                .child(div().flex_shrink_0().child("等待回答"))
                .child(ellipsis(("question-title", idx), first))
                .child(div().flex_shrink_0().text_color(rgb(INK_SOFT)).child("· 请在下方选择")),
        }
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
        let origin = (s.token, s.diff_epoch);
        let total = files.len();
        let open = s.changes_open.contains(&idx);
        let shown = if open || total <= CHANGES_PREVIEW + 1 { total } else { CHANGES_PREVIEW };
        let faded = undo.is_some_and(|u| u.all);
        let rows = files.into_iter().take(shown).enumerate().map(|(n, f)| {
            let location = f.location;
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
                .on_click(cx.listener(move |this, _, _, cx| {
                    let session = this.session();
                    if (session.token, session.diff_epoch) == origin {
                        this.reveal_change(location, cx);
                    }
                }))
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
                    this.sessions[active].pin_last_row(idx);
                    let s = &mut this.sessions[active];
                    s.toggle_changes(idx);
                    cx.notify();
                }))
        });
        div()
            .v_flex()
            .rounded_xl()
            .border_1()
            .border_color(rgb(LINE))
            .bg(rgb(CARD))
            .shadow(raised_shadow())
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
    fn reveal_change(&mut self, location: DiffLocation, cx: &mut Context<Self>) {
        if self.settings.view_mode == ViewMode::Summary {
            self.settings.view_mode = ViewMode::Normal;
            self.view_changed(cx);
        }
        self.open_right_diff(location, cx);
    }

    /// 一个文件的 diff 卡片；`preview` = 默认露出的行数（None = 全部）。
    /// 超出的部分由 `rest`（元素 id, 展开进度）决定：进度 > 0 时按进度裁切着画出来。
    /// `tool`（行号 / diff 编号, 工具, 结果图标的入场进度）给了时表头带上工具的状态图标和名称（兼当工具行）；
    /// 编号同时用来给授权标记起元素 id——同一条路径可能在不同的行里各出现一次，不带行号会撞。
    pub(super) fn render_diff(
        diff: &DiffView,
        cwd: &Path,
        preview: Option<usize>,
        rest: Option<(ElementId, f32)>,
        tool: Option<(usize, &ToolCard, f32)>,
    ) -> Div {
        let shown = preview.map_or(diff.lines.len(), |n| n.min(diff.lines.len()));
        let truncated = diff.truncated.then(diff_truncated);
        let mut body = div().v_flex().py_1().children(diff.lines[..shown].iter().map(diff_line));
        let hidden = &diff.lines[shown..];
        if hidden.is_empty() {
            body = body.children(truncated);
        } else if let Some((id, progress)) = rest.filter(|(_, p)| *p > 0.) {
            let more = div().v_flex().children(hidden.iter().map(diff_line)).children(truncated);
            body = body.child(Reveal::new(id, progress, more));
        }
        Self::diff_card(diff, cwd, tool, body)
    }

    pub(super) fn diff_card(
        diff: &DiffView,
        cwd: &Path,
        tool: Option<(usize, &ToolCard, f32)>,
        body: impl IntoElement,
    ) -> Div {
        let icon = match tool {
            Some((_, card, settle)) => tool_status_icon(card, settle),
            None => {
                Icon::new(IconAll::FileText).size_3p5().text_color(rgb(INK_SOFT)).into_any_element()
            }
        };
        let (name, dir) =
            split_path(&claudesktop_core::review::display_path(&diff.path, Some(cwd)));
        let header = div()
            .h_flex()
            .flex_shrink_0()
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
            .children(tool.map(|(_, card, _)| {
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
            .children(tool.and_then(|(key, card, _)| grant_badge(("diff-grant", key), card)))
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
            .shadow(raised_shadow())
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
        s.release_pin();
        // 用户自己点了「回到底部」：发消息后的强跟随窗口作废，按普通跳转走
        s.drop_force_follow();
        let remaining = distance_to_end(&s.list);
        if cx.reduce_motion() || remaining <= px(1.) {
            s.follow_tail();
            self.content_view.update(cx, |_, cx| cx.notify());
            return;
        }
        let viewport = s.list.viewport_bounds().size.height;
        if remaining > viewport * 1.5 {
            s.list.scroll_by(remaining - viewport);
        }
        let now = Instant::now();
        self.jump = Some(motion::JumpAnim { token: s.token, started: now, last: now });
        self.content_view.update(cx, |_, cx| cx.notify());
    }

    /// 平滑回到底部的一帧（`render` 开头调）：固定 320ms cubic-out 滑过去，
    /// 到底或走完时长就交给自动跟随。切走会话、用户自己滚 / 拖滚动条了就停。
    pub(super) fn step_jump_to_bottom(&mut self, window: &mut Window) -> bool {
        let Some(mut anim) = self.jump.take() else {
            return false;
        };
        let s = &mut self.sessions[self.active];
        if anim.token != s.token || s.list.is_following_tail() || s.list.is_scrollbar_dragging() {
            return false;
        }
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(anim.started);
        let dt = now.saturating_duration_since(anim.last);
        anim.last = now;
        let remaining = distance_to_end(&s.list);
        if remaining <= px(1.) || elapsed >= motion::JUMP_DURATION {
            s.follow_tail();
            return true;
        }
        let k = motion::jump_step(elapsed, dt);
        // 每帧至少走半像素，别在长距离末尾拖出几十个亚像素帧
        let step = (remaining * k).max(px(0.5)).min(remaining);
        s.list.scroll_by(step);
        self.jump = Some(anim);
        window.request_animation_frame();
        true
    }

    /// 键盘翻页：折算成一页高度的滚轮输入，和滚轮共用同一套阻尼（两套手感会立刻被察觉）。
    pub(super) fn scroll_page(&mut self, direction: isize, cx: &mut Context<Self>) {
        if self.settings_open {
            cx.propagate();
            return;
        }
        let viewport = self.session().list.viewport_bounds().size.height;
        if viewport <= px(0.) || direction == 0 {
            cx.propagate();
            return;
        }
        self.wheel_by(viewport * 0.85 * direction as f32, false, cx);
    }

    /// Ctrl+Home：回到这段对话的开头（停止跟底，不播放长距离动画）。
    pub(super) fn scroll_to_top(&mut self, cx: &mut Context<Self>) {
        if self.settings_open {
            cx.propagate();
            return;
        }
        self.jump = None;
        let s = &mut self.sessions[self.active];
        s.release_pin();
        s.wheel = None;
        s.note_input(false);
        s.list.pause_following_tail();
        s.list.scroll_to(ListOffset { item_ix: 0, offset_in_item: px(0.) });
        s.quiet_follow(Duration::from_millis(700));
        self.content_view.update(cx, |_, cx| cx.notify());
    }

    /// 滚轮往末尾滚了 `delta`（正 = 往下，负 = 往上）：把离散输入累计到
    /// [`WheelInput`] 的相对余量里，由 [`Self::step_wheel`] 每帧沿 $C^1$ 三次贝塞尔曲线
    /// 推进（默认段长 100–200ms，连滚可再缩短）交给列表。
    /// 只记「用户还要滚多少」，不再记滚动条的绝对目标——上方行补量抬高读数时不会
    /// 凭空多走一段（见 HANDOFF 里「绝对像素目标在补量行高后失效」）。
    /// 触控板的精确位移本来就连续，和「减少动态效果」时一样直接滚到位。
    /// 只通知内容区：滚动不影响输入区。
    pub(super) fn wheel_by(&mut self, delta: Pixels, precise: bool, cx: &mut Context<Self>) {
        self.hover_settle_task = None;
        self.jump = None;
        let now = Instant::now();
        let direct = precise || cx.reduce_motion();
        let s = &mut self.sessions[self.active];
        // 用户自己滚了：展开 / 收起时钉住的那一行不用再补偿
        s.release_pin();
        // 往上翻就当场离开底部：不然流式追加会把位置拉回末尾。
        // 方向记进跟随状态机——只有「往下滚回距底 80px 内」才重新吸附
        s.note_input(delta > px(0.));
        if delta < px(0.) {
            s.list.pause_following_tail();
        }
        if direct {
            s.wheel = None;
            let (applied, _) = apply_relative(&s.list, delta);
            if applied != px(0.) {
                self.scrolling_until = Some(now + super::content::MESSAGE_HOVER_SETTLE);
            }
        } else {
            match &mut s.wheel {
                Some(w) => w.push(now, delta),
                // 新的一串从此刻起算：首帧固定按一个刷新周期起步（不受事件到达相位影响），
                // 之后沿曲线圆顶加速再收到零，干脆落位
                None => s.wheel = Some(WheelInput::new(now, delta)),
            }
        }
        // 第一格输入就让内容区失效，推进从下一次刷新开始
        self.content_view.update(cx, |_, cx| cx.notify());
    }

    /// 滚轮平滑的一帧（`render_content` 里 `sync_list` 之后调，定位先完成再推进当帧滚轮）：
    /// 按显示器刷新周期推进一段 $C^1$ 贝塞尔曲线，按物理像素取整后交给列表，
    /// 再按当前布局裁掉越界部分，剩下的下一帧继续。只要还有余量，每帧只追加一次动画请求。
    pub(super) fn step_wheel(&mut self, window: &mut Window, cx: &App) -> bool {
        let s = &mut self.sessions[self.active];
        let Some(mut w) = s.wheel.take() else {
            return false;
        };
        // 用户拖滚动条：平滑让位，别两边抢（拖动结束也不会接着滑）
        if s.list.is_scrollbar_dragging() {
            return false;
        }
        let now = Instant::now();
        let frame_dt = window.refresh_interval(cx).unwrap_or(crate::session::DEFAULT_FRAME_DT);
        let scale = window.scale_factor();
        let step = w.advance(now, window.frame_vsync_at(), frame_dt, scale);
        let mut moved = false;
        if step != px(0.) {
            let (applied, edge) = apply_relative(&s.list, step);
            moved = applied != px(0.);
            // 先记账再 `block`：`block` 会把欠量清零，反过来相减会留下负的余量。
            // `applied == 0`（一步没走成）由 `consume` 重锚曲线并开始计「卡住」的时长
            w.consume(applied, now);
            // 这个方向真到头了：朝外的欠量作废，别每帧空转。
            // 位移方向永远和输入一致，不会有回弹帧
            if edge {
                w.block(step > px(0.));
            }
        }
        if w.expired(now) {
            // 等太久了还是走不出去（列表放不出这段距离）：丢掉余量，别再排帧
            w.clear();
        }
        if w.is_done(now) {
            return moved;
        }
        s.wheel = Some(w);
        window.request_animation_frame();
        moved
    }

    fn message_hover_suppressed(&self) -> bool {
        self.transcript_scrolling
            || self.scrolling_until.is_some_and(|until| Instant::now() < until)
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
            // 滚动时继续记录指针落点，等停稳再更新操作按钮。
            if !self.message_hover_suppressed() {
                self.content_view.update(cx, |_, cx| cx.notify());
            }
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
            (!self.message_hover_suppressed() && self.hovered_msg == Some(idx)) || rewinding,
            window,
            cx,
        );
        let current = self.session().kernel.clone();
        // 只给配置过的内核：启用的每一条 agent 档案都是候选（新会话在它上面开）
        let acp_agents: Vec<KernelChoice> =
            self.settings.acp.agents.iter().map(|a| KernelChoice::Acp(a.id.clone())).collect();
        let others: Vec<KernelChoice> = std::iter::once(KernelChoice::Builtin)
            .chain(acp_agents)
            .filter(|k| {
                *k != current
                    && match k {
                        KernelChoice::Builtin => self.settings.builtin.is_configured(),
                        KernelChoice::Acp(_) => true,
                        KernelChoice::Demo => false,
                    }
            })
            .collect();
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
        // 只有一个别的内核就直接给按钮；多了收进菜单，别把操作行排成一串按钮
        if others.len() == 1 {
            let kind = others[0].clone();
            actions = actions.child(
                Self::message_action(
                    ("rerun", idx * 4),
                    IconAll::RefreshCw,
                    format!("在「{}」上重跑", kind.label(&self.settings.acp)),
                    cx,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    let kind = kind.clone();
                    this.rerun_on(idx, kind, cx)
                })),
            );
        } else if others.len() > 1 {
            let kinds = others;
            let handle = cx.entity();
            let focus = self.menu_focus.clone();
            let trigger = Self::message_action(
                ("rerun-menu-btn", idx),
                IconAll::RefreshCw,
                "在其他内核上重跑",
                cx,
            );
            actions = actions.child(
                menu_panel(Popover::new(("rerun-menu", idx)), &self.menu_focus)
                    .anchor(Anchor::TopRight)
                    .w(rems(16.))
                    .trigger(trigger)
                    .content(move |_, window, cx| {
                        let popover = cx.entity();
                        let app = handle.read(cx);
                        let mut list = MenuList::new(("rerun-menu-list", idx), &handle, &popover)
                            .without_shortcuts();
                        for (n, kind) in kinds.iter().enumerate() {
                            let kind = kind.clone();
                            let label = format!("在「{}」上重跑", kind.label(&app.settings.acp));
                            list = list.pick(
                                menu_item(
                                    ("rerun-item", idx * 8 + n),
                                    IconAll::RefreshCw,
                                    label,
                                    "",
                                    false,
                                ),
                                move |this, cx| this.rerun_on(idx, kind.clone(), cx),
                            );
                        }
                        list.finish(&focus, window, cx)
                    }),
            );
        }
        // 气泡：有文字才画文本气泡，图片另起一行缩略图（丢了文件就跳过）。
        let bubble = if text.is_empty() {
            None
        } else {
            Some(
                div()
                    .max_w(relative(0.78))
                    .bg(rgb(USER_BUBBLE))
                    .text_color(rgb(INK))
                    .rounded(COMPOSER_INNER_RADIUS)
                    .px_4()
                    .py_2p5()
                    .text_size(TEXT_BODY)
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
                        .child(
                            img(p.clone())
                                .image_cache(&self.image_cache)
                                .size_20()
                                .rounded_md()
                                .object_fit(ObjectFit::Cover),
                        )
                }),
            ))
        };
        let block = div()
            .id(("user-msg", idx))
            .v_flex()
            .items_end()
            .gap_0p5()
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
            ChatItem::Thinking { text, streaming, open, elapsed_ms, .. } => {
                self.render_thinking(idx, text, *streaming, *open, *elapsed_ms, verbose, window, cx)
            }
            ChatItem::Info { text, open } => {
                Self::render_info(idx, text, *open, verbose, window, cx)
            }
            ChatItem::Tool(card) if card.task => self.render_task_card(idx, card, window, cx),
            ChatItem::Tool(card) => {
                self.render_tool(idx, card, &self.session().cwd, verbose, window, cx)
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
            ChatItem::Question { questions, answer, .. } => {
                Self::render_question(idx, questions, answer)
            }
            ChatItem::Plan(rows) => Self::render_plan(rows, layout.compact),
            ChatItem::Agent(text) => self.render_agent(idx, None, text, window, cx),
            ChatItem::Failed { message, kind } => self.render_failed(idx, message, *kind, cx),
            ChatItem::Compaction { state, open, .. } => {
                self.render_compaction(idx, state, *open, window, cx)
            }
            // 一轮层面的提示（续接、停止…）：和正文左对齐，图标打头
            ChatItem::Note(text) => {
                step_line(Icon::new(IconAll::Info).size_3p5().text_color(rgb(INK_SOFT)))
                    .child(div().flex_1().min_w_0().child(text.clone()))
            }
        }
    }

    /// 拆开的大回复里的第 `part` 块。
    pub(super) fn render_agent_part(
        &self,
        idx: usize,
        part: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let s = self.session();
        let Some(text) = s.part_text(idx, part) else { return div() };
        self.render_agent(idx, Some(part), &text, window, cx)
    }

    /// 一条回复的 Markdown 正文（[`ChatItem::Agent`]）。`part` = 很大的已完成回复被拆到时
    /// 这一块的下标（见 `session::Row`）；整条画时是 None。
    pub(super) fn render_agent(
        &self,
        idx: usize,
        part: Option<usize>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let s = self.session();
        // 正在流式写的是最后一条记录，这时不拆块（`Row::Whole`）
        let streaming = part.is_none() && s.busy && idx + 1 == s.transcript.len();
        // 元素 id 用显示行下标：拆开的每一块都不一样，整条时等于记录下标
        let key = s.row_of(idx) + part.unwrap_or(0);
        let last = part.is_none_or(|p| s.part_count(idx).is_some_and(|count| p + 1 == count));
        // 代码块右上角：语言名 + 「复制」（点完短暂变对勾，和消息下方的复制同一套状态）。
        // 拆开的大回复里每块的 span 都从 0 起，加上这块在整条回复里的起始字节才是唯一标识
        let base = part.map_or(0, |p| s.part_start(idx, p));
        let token = s.token;
        let copied = match self.copied {
            Some((t, i, Some(block))) if t == token && i == idx => {
                self.copied_at.map(|at| (block, at))
            }
            _ => None,
        };
        let body = div().min_w_0().child(self.markdown_body(
            idx,
            BodyKind::Agent(part),
            text,
            streaming,
            Some(CodeCopy { base, copied }),
            window,
            cx,
        ));
        if !last || !s.is_turn_end(idx) {
            return body;
        }
        // 一轮回复的末尾挂「复制」和这一轮的用时 / 用量：最后一轮常显，
        // 更早的悬停才淡入（只改透明度，不挤布局）
        let latest = !s.transcript[idx + 1..].iter().any(|i| matches!(i, ChatItem::User { .. }));
        let hovered = !self.message_hover_suppressed() && self.hovered_msg == Some(idx);
        let fade =
            (!latest).then(|| motion::hover_opacity(("agent-actions", key), hovered, window, cx));
        let summary = s.turn_summary(idx);
        let meta = summary.and_then(super::status::turn_meta);
        let meta_tip = summary.and_then(|t| super::status::turn_meta_tooltip(&t.usage));
        let files = s.turn_files(idx);
        let has_files = !files.is_empty();
        let actions = div()
            .h_flex()
            .items_center()
            .gap_2()
            .pt_1()
            .child(
                self.copy_action(("copy-agent", key), idx, window, cx, |s, idx| s.turn_text(idx)),
            )
            .children(meta.map(|m| {
                // 和运行中的状态行同一套数字字形，一轮结束换过来时不变样
                let cell = div()
                    .text_size(TEXT_META)
                    .text_color(rgb(INK_SOFT))
                    .font_features(tabular_nums());
                match meta_tip {
                    Some(tip) => cell
                        .id(("agent-meta", key))
                        .tooltip(move |window, cx| text_tip(&tip, window, cx))
                        .child(m)
                        .into_any_element(),
                    None => cell.child(m).into_any_element(),
                }
            }))
            .when_some(fade, |d, opacity| d.opacity(opacity));
        // 这一轮改过文件：正文和操作行之间一张改动卡片（常显，不跟着悬停淡入淡出）
        let changes =
            has_files.then(|| div().pt_3().child(self.render_changes_card(idx, files, latest, cx)));
        div().child(
            div()
                .id(("agent-msg", key))
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

    #[allow(clippy::too_many_arguments)]
    fn markdown_body(
        &self,
        src: usize,
        kind: BodyKind,
        text: &str,
        streaming: bool,
        copy: Option<CodeCopy>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let session = self.session();
        let body = session.body_cache.body(src, kind, text, streaming, copy, cx);
        AgentBodyView::element(body, session.list_metrics(), self.transcript_scrolling, window, cx)
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

pub(super) fn diff_truncated() -> Div {
    div()
        .px_3()
        .py_1()
        .font_family(sans())
        .text_size(TEXT_SMALL)
        .line_height(LEADING)
        .text_color(rgb(INK_SOFT))
        .child("改动过大，其余部分未显示")
}

/// diff 的一行：行号、+/- 标记、内容；`'~'` 是两个 hunk 之间省略的部分。
pub(super) fn diff_line(line: &claudesktop_core::DiffLineView) -> Div {
    let row = div().font_family(MONO).text_size(TEXT_SMALL).line_height(LEADING);
    if line.tag == '~' {
        return row
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
    row.h_flex()
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

/// 列表当前滚到离顶部多远（像素，按列表自己的记账口径）。
fn scroll_top(list: &ListState) -> Pixels {
    -list.scroll_px_offset_for_scrollbar().y
}

/// 当前布局下最多能滚多远（未测行按已有的量算，下一帧真实布局会更新）。
fn max_scroll(list: &ListState) -> Pixels {
    list.max_offset_for_scrollbar().y.max(px(0.))
}

fn distance_to_end(list: &ListState) -> Pixels {
    (max_scroll(list) - scroll_top(list)).max(px(0.))
}

/// 把 `delta` 交给列表做相对滚动，返回实际消费的像素和「这个方向是不是真到头了」。
///
/// 列表自己的 `scroll_by` 只夹顶部，底部靠布局夹——布局会把落点收回末行，但那样
/// 「这一步走没走到」就无从判断。所以这里用同一份布局读数先把单步裁到**已量出的范围**：
///
/// - 跟着末尾时往下没有可走的距离，直接判到底（列表继续跟随）。
/// - 跟随时逻辑位置记在末尾之后（多出一个视口），往上先归位到可滚范围：屏幕上同一位置。
///   不归位的话 `scroll_by` 会先拿这段「溢出」抵掉，表现为开滚前空滑几帧。
/// - 往下：单步夹到 `max`（已量出的底部）。未测行记零高，`max` 因此偏小，但列表给未测行
///   留了 600px 的尾部 overdraw：正常平滑（≤48px）和触控板输入都碰不到，只有真的甩到
///   已量底部才被拦下，正好落在最后一个已测行上，不会一步跨过整片零高的未测行瞬移到末尾。
/// - 边界按**滚动之后**的读数判断：上方读数到 0 且逻辑行回到 0 才算真正到顶（折叠成零高的
///   前缀行、刚跳转过来时读数到 0 但还在第 N 行，那不是到顶）；往下只有列表量完了
///   （`is_scrolled_to_end` 有确定答案）才算到底，量不完的交给下一帧布局或 `WHEEL_HOLD`。
///
/// 单步永远在本帧请求量和 0 之间，不会有反方向的补偿帧。
fn apply_relative(list: &ListState, delta: Pixels) -> (Pixels, bool) {
    let down = delta > px(0.);
    if down && list.is_following_tail() {
        return (px(0.), true);
    }
    let max = max_scroll(list);
    // 短对话：没有可滚的距离，别把欠量一直挂着（`is_scrolled_to_end` 这时也给不出答案）
    if max <= px(0.) {
        return (px(0.), true);
    }
    if !down && scroll_top(list) > max + px(0.5) {
        list.scroll_by(max - scroll_top(list));
    }
    let top = scroll_top(list).max(px(0.));
    let applied = if down { ((top + delta).min(max) - top).max(px(0.)) } else { delta.max(-top) };
    if applied != px(0.) {
        list.scroll_by(applied);
    }
    let edge = if down {
        // 估高把「底部」提前了：末尾真测到才算到底，否则欠量会在估高误差里被吞掉
        list.is_scrolled_to_end() == Some(true) && list.is_tail_measured()
    } else {
        scroll_top(list) <= px(0.5) && list.logical_scroll_top().item_ix == 0
    };
    (applied, edge)
}

/// 回复 / 思考 / 摘要交给 `TextView` 前的整理：流式中补齐半截标记，把 `\( … \)` / `\[ … \]` 这类
/// 写法统一成 `$…$`（代码里不动，见 `math::normalize_markup`），再收掉全角标点旁的空格。
pub(super) fn reading_markdown(text: &str, streaming: bool) -> SharedString {
    // 不流式的正文已经定型：计时器、悬停、滚动都会让它重画，每帧重算规范化没有意义
    if !streaming && let Some(hit) = markdown_cache_get(text) {
        return hit;
    }
    let original = text;
    let text = if streaming { close_dangling_markup(text) } else { Cow::Borrowed(text) };
    let text = claudesktop_core::math::normalize_markup(&text);
    let out: SharedString = tidy_cjk_spacing(&text).into_owned().into();
    if !streaming {
        markdown_cache_put(original, out.clone());
    }
    out
}

/// 已定型正文的渲染缓存（按原文哈希落格、原串比对）：有界 256 条，满了换掉最旧的一条。
fn markdown_cache_get(text: &str) -> Option<SharedString> {
    let hash = hash_text(text);
    MARKDOWN_CACHE
        .with_borrow(|cache| cache.iter().find(|e| e.0 == hash && e.1 == text).map(|e| e.2.clone()))
}

fn markdown_cache_put(text: &str, out: SharedString) {
    let hash = hash_text(text);
    MARKDOWN_CACHE.with_borrow_mut(|cache| {
        if let Some(e) = cache.iter_mut().find(|e| e.0 == hash && e.1 == text) {
            e.2 = out;
            return;
        }
        if cache.len() >= MARKDOWN_CACHE_LEN {
            cache.remove(0);
        }
        cache.push((hash, text.to_string(), out));
    });
}

fn hash_text(text: &str) -> u64 {
    use std::hash::{Hash as _, Hasher as _};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

/// 流式输出停在半截标记上时（`` `foo `` 或没收尾的 ``` 围栏、`**粗体`），渲染前临时补齐，
/// 免得标记原样露出来、等收尾后整段又跳成代码 / 粗体样式。
pub(super) fn close_dangling_markup(text: &str) -> Cow<'_, str> {
    let mut in_fence = false;
    // 当前段落（围栏外）里还没配对的行内代码分隔符：反引号串长度（CommonMark 按同长配对）
    let mut open_ticks: Option<usize> = None;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            open_ticks = None;
        } else if !in_fence {
            if line.trim().is_empty() {
                open_ticks = None;
            } else {
                let mut rest = line;
                while let Some(at) = rest.find('`') {
                    let run = rest[at..].len() - rest[at..].trim_start_matches('`').len();
                    match open_ticks {
                        // 同长的收尾：这一对结束
                        Some(open) if open == run => open_ticks = None,
                        Some(_) => {}
                        None => open_ticks = Some(run),
                    }
                    rest = &rest[at + run..];
                }
            }
        }
    }
    if in_fence {
        Cow::Owned(format!("{text}\n```"))
    } else if let Some(run) = open_ticks {
        // 补齐时用同长的反引号串，否则包不住（` ``a`b `` 需要两个反引号收尾）
        Cow::Owned(format!("{text}{}", "`".repeat(run)))
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
    let (mut count, mut open) = (0, None);
    // 行内代码按**同长**的反引号串配对（CommonMark 的规则）：` ``a`b`` ` 里那个单个反引号
    // 不是分隔符。早先按单个 ` 切开数奇偶，遇到双反引号包单反引号就把段算反，
    // 代码里的 `**` 被当成正文的粗体标记。
    let line = &text[start..];
    let mut at = 0;
    while at < line.len() {
        let rest = &line[at..];
        if rest.starts_with('`') {
            let run = rest.len() - rest.trim_start_matches('`').len();
            match find_tick_run(line, at + run, run) {
                Some(end) => at = end + run,
                // 没有配对的收尾围栏：后面都被它包住，不再算正文
                None => break,
            }
            continue;
        }
        if rest.starts_with("**") {
            count += 1;
            open = Some(start + at);
            at += 2;
            continue;
        }
        at += rest.chars().next().map_or(1, char::len_utf8);
    }
    match open {
        Some(open) if count % 2 == 1 && text[open + 2..].trim().is_empty() => {
            Cow::Borrowed(&text[..open])
        }
        Some(_) if count % 2 == 1 => Cow::Owned(format!("{}**", text.trim_end())),
        _ => Cow::Borrowed(text),
    }
}

/// 从 `from` 起找长度正好是 `len` 的反引号串，返回它的起始下标（找不到 = 没有配对）。
fn find_tick_run(line: &str, from: usize, len: usize) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut i = from;
    while i < bytes.len() {
        if bytes[i] != b'`' {
            i += 1;
            continue;
        }
        let end = bytes[i..].iter().take_while(|b| **b == b'`').count() + i;
        if end - i == len {
            return Some(i);
        }
        i = end;
    }
    None
}

/// 权限卡的标题：按动作类型说清楚要允许什么，别一律「这一步」。
/// 标题是内核给的工具名 / 命令原文，只能按关键词粗分；认不出就退回落款。
fn permission_prompt(title: &str) -> &'static str {
    let t = title.to_lowercase();
    let has = |keys: &[&str]| keys.iter().any(|k| t.contains(k));
    // 以「运行命令 / shell」开头的先归命令：参数里的 edit / remove 是文件名，不是动作。
    // 只有命令里真带了删除 / 下载动作（`rm `、`curl`…）才落到下面两档
    let command_head = has(&["运行命令", "shell", "bash", "terminal", "执行"])
        && !has(&["rm ", "rmdir", "curl", "wget", "delete"]);
    if command_head {
        "要允许运行这条命令吗？"
    } else if has(&["删除", "移除", "rm ", "rmdir", "delete", "remove", "unlink"]) {
        "要允许删除文件吗？"
    } else if has(&["http", "fetch", "curl", "wget", "网络", "网页", "web"]) {
        "要允许访问网络吗？"
    } else if has(&["命令", "command", "run "]) {
        "要允许运行这条命令吗？"
    } else if has(&["写入", "修改", "编辑", "write", "edit", "patch", "replace"]) {
        "要允许修改这个文件吗？"
    } else {
        "要允许这一步吗？"
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
    fn permission_prompt_recognizes_actions() {
        assert_eq!(permission_prompt("要删除 build/"), "要允许删除文件吗？");
        assert_eq!(permission_prompt("Write src/main.rs"), "要允许修改这个文件吗？");
        assert_eq!(permission_prompt("Fetch https://example.com"), "要允许访问网络吗？");
        assert_eq!(permission_prompt("运行命令 `cargo test`"), "要允许运行这条命令吗？");
        // 命令参数里的 edit / remove / web 是文件名，不是动作
        assert_eq!(permission_prompt("运行命令 `cargo test edit_test`"), "要允许运行这条命令吗？");
        assert_eq!(
            permission_prompt("运行命令 `cargo test remove_test`"),
            "要允许运行这条命令吗？"
        );
        assert_eq!(permission_prompt("运行命令 `cargo test web_test`"), "要允许运行这条命令吗？");
        // 命令里真带了删除 / 下载动作才换说法
        assert_eq!(permission_prompt("运行命令 `rm -rf build`"), "要允许删除文件吗？");
        assert_eq!(permission_prompt("Bash"), "要允许运行这条命令吗？");
        assert_eq!(permission_prompt("mcp__custom__tool"), "要允许这一步吗？");
    }

    #[core::prelude::v1::test]
    fn reading_markdown_caches_settled_text() {
        let first = reading_markdown("正文里**有**标记", false);
        let second = reading_markdown("正文里**有**标记", false);
        // 定型后同一段文字命中缓存（拿到同一份），结果不变
        assert_eq!(first, second);
        // 流式的不进缓存，补齐半截标记后才对
        assert_eq!(reading_markdown("`abc", true).to_string(), "`abc`");
    }

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

    /// 行内代码按同长反引号串配对：`` `` `` 里夹一个单反引号时，代码里的 `**`
    /// 不能被算成正文的粗体标记（早先按单个 ` 数奇偶会把段算反）。
    #[core::prelude::v1::test]
    fn inline_code_pairs_backtick_runs_of_equal_length() {
        // 双反引号包裹、里面含单个反引号：整体是代码，末尾的 `- **待补` 才是正文
        assert_eq!(close_dangling_markup("见 ``a`b`` 然后 - **待补"), "见 ``a`b`` 然后 - **待补**");
        // 双反引号里的 `**` 不算正文标记
        assert_eq!(close_dangling_markup("见 ``x**y`` 结束"), "见 ``x**y`` 结束");
        // 单反引号里含 `**`：照旧不算
        assert_eq!(close_dangling_markup("`a**b` 结束"), "`a**b` 结束");
        // 没收尾的双反引号用同长串补齐（上游实现是补一个 `，包不住双反引号）
        assert_eq!(close_dangling_markup("``x**y 还没收"), "``x**y 还没收``");
        // 单反引号与双反引号混排，配对不串
        assert_eq!(close_dangling_markup("`a` 和 ``b**c`` 和 **粗"), "`a` 和 ``b**c`` 和 **粗**");
    }

    /// `**加粗链接**` 结尾的星号不能被 GFM 裸链接吞进 URL（vendor/markdown 补丁，
    /// 见 `vendor/README.md`）：AST 里必须是 Strong -> Link，正文没有落单的 `**`。
    #[core::prelude::v1::test]
    fn bold_autolink_keeps_strong_and_drops_trailing_stars() {
        use markdown::{ParseOptions, mdast::Node};

        // 段落里 Strong 节点的文本（顺带证明它没有散成 Text("**")）
        let strong_of = |source: &str| -> Vec<String> {
            let mut options = ParseOptions::gfm();
            options.constructs.math_text = true;
            let ast = markdown::to_mdast(source, &options).expect("parse");
            let mut texts = Vec::new();
            fn text_of(node: &Node, buf: &mut String) {
                match node {
                    Node::Text(text) => buf.push_str(&text.value),
                    _ => {
                        if let Some(children) = node.children() {
                            for child in children {
                                text_of(child, buf);
                            }
                        }
                    }
                }
            }
            fn walk(node: &Node, texts: &mut Vec<String>) {
                if let Node::Strong(strong) = node {
                    let mut buf = String::new();
                    for child in &strong.children {
                        text_of(child, &mut buf);
                    }
                    texts.push(buf);
                }
                if let Some(children) = node.children() {
                    for child in children {
                        walk(child, texts);
                    }
                }
            }
            walk(&ast, &mut texts);
            texts
        };

        // 引号里是用户实际报的那句：加粗成立、URL 不带尾随星号
        assert_eq!(
            strong_of("请访问 **http://localhost:4390/**（等价）"),
            ["http://localhost:4390/".to_string()]
        );
        assert_eq!(strong_of("请访问 **http://a.b/** (半角)"), ["http://a.b/".to_string()]);
        assert_eq!(strong_of("请访问 **http://a.b/**。"), ["http://a.b/".to_string()]);
        assert_eq!(strong_of("请访问 **http://a.b/** "), ["http://a.b/".to_string()]);
        // 路径中间的星号属于 URL，不能裁（`http://a.b/x*y` 整条仍是链接文本）
        assert_eq!(strong_of("见 **http://a.b/x*y**"), ["http://a.b/x*y".to_string()]);
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

    /// GFM 自动链接遇全角标点收尾是 vendor/markdown 的补丁（见 `vendor/README.md`）：
    /// 上游只认 ASCII 空白为终点，`http://a.b/（注）` 会把全角括号连同后面的字吞进 URL，
    /// `**http://a.b/**（注）` 的星号也因此露出来。升级 markdown crate 后补丁没打上这里会挂。
    /// 用和 gpui-base 相同的选项（gfm + math）直接看 AST。
    #[core::prelude::v1::test]
    fn autolink_ends_at_fullwidth_punctuation() {
        use markdown::{ParseOptions, mdast::Node};

        let link_urls = |source: &str| -> Vec<String> {
            let mut options = ParseOptions::gfm();
            options.constructs.math_text = true;
            options.constructs.math_flow = true;
            let ast = markdown::to_mdast(source, &options).expect("parse");
            let mut urls = Vec::new();
            fn walk(node: &Node, urls: &mut Vec<String>) {
                if let Node::Link(link) = node {
                    urls.push(link.url.clone());
                }
                if let Some(children) = node.children() {
                    for child in children {
                        walk(child, urls);
                    }
                }
            }
            walk(&ast, &mut urls);
            urls
        };
        let strong_texts = |source: &str| -> Vec<String> {
            let mut options = ParseOptions::gfm();
            options.constructs.math_text = true;
            options.constructs.math_flow = true;
            let ast = markdown::to_mdast(source, &options).expect("parse");
            let mut texts = Vec::new();
            fn subtree_text(node: &Node, buf: &mut String) {
                match node {
                    Node::Text(t) => buf.push_str(&t.value),
                    _ => {
                        if let Some(children) = node.children() {
                            for child in children {
                                subtree_text(child, buf);
                            }
                        }
                    }
                }
            }
            fn walk(node: &Node, texts: &mut Vec<String>) {
                if let Node::Strong(strong) = node {
                    let mut buf = String::new();
                    for child in &strong.children {
                        subtree_text(child, &mut buf);
                    }
                    texts.push(buf);
                }
                if let Some(children) = node.children() {
                    for child in children {
                        walk(child, texts);
                    }
                }
            }
            walk(&ast, &mut texts);
            texts
        };

        // 图二原样：URL 不再吞 `**（等价，200）`，加粗成立、无星号露出
        assert_eq!(
            link_urls("Chrome 里请用 **http://localhost:4390/**（等价，200）"),
            ["http://localhost:4390/".to_string()]
        );
        assert_eq!(
            strong_texts("Chrome 里请用 **http://localhost:4390/**（等价，200）"),
            ["http://localhost:4390/".to_string()]
        );
        // 斜体、下划线强调同病同治（URL 的尾斜线保留）
        assert_eq!(link_urls("*http://a.b/*（好）"), ["http://a.b/".to_string()]);
        assert_eq!(link_urls("__http://a.b/__（好）"), ["http://a.b/".to_string()]);
        // 裸链接后跟全角标点：只链 URL 本身
        assert_eq!(link_urls("打开 http://a.b/，好的"), ["http://a.b/".to_string()]);
        assert_eq!(link_urls("见 **文档 http://a.b/**（好）"), ["http://a.b/".to_string()]);
        // 半角括号 / 空格的旧行为不变
        assert_eq!(link_urls("x (http://a.b/) y"), ["http://a.b/".to_string()]);
        // URL 带全角字符（罕见）时仍整段链住，不会断成两半
        assert_eq!(
            link_urls("看 http://example.com/页面 名称"),
            ["http://example.com/页面".to_string()]
        );
    }
}

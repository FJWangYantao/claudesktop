//! 输入框工具栏的模型选择器：品牌图标 + 模型名 + 思考档位的 trigger，
//! 面板里是模型列表（图标 / 上下文 / 价格档位）、悬停说明卡和 Effort 分段器。
//!
//! 和设置页的默认模型下拉（`settings_view/model.rs`）互不影响：那边仍是中文的
//! `MenuChip`，这里专给输入框用，文案是英文。
//!
//! 面板内容是独立 entity（`ModelPickerContent`）：悬停换说明卡时只重绘这一块，
//! 不连带会话内容区一起重画——`Popover::content` 每次父视图渲染都会重跑，
//! 所以用 `use_keyed_state` 只创建一次。

use super::ChatApp;
use super::composer::session_thinking_spec;
use super::menu::menu_panel;
use super::settings_view::{active_thinking, thinking_options};
use super::widgets::Surface;
use crate::model_catalog::{self, ModelBrand};
use crate::settings::KernelChoice;
use crate::theme::rgb;
use crate::theme::*;
use claudesktop_builtin::ThinkingLevel;
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::base::{PopoverState, Selectable, StyledExt as _};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

/// 一行模型的数据：品牌、上下文、价格档位、能力都在面板构建时算好，
/// 渲染时只摆版，不再回读设置。
struct RowData {
    /// 面板内唯一（来源下标 + 模型下标），悬停预览按它对回这一行
    key: String,
    /// `Some(来源下标)` = 自研内核（跨来源挑），`None` = 外部 ACP agent
    profile: Option<usize>,
    /// 交给内核的模型值
    value: String,
    /// 显示名（ACP agent 会给一个友好名）
    name: String,
    brand: ModelBrand,
    context: Option<String>,
    tier: Option<u8>,
    active: bool,
    /// 说明卡正文：自研内核查静态目录，ACP 用 agent 上报的描述
    detail: Option<SharedString>,
    caps: Vec<&'static str>,
}

enum RowEntry {
    Header(SharedString),
    Row(RowData),
}

/// 悬停预览指向哪一行。
#[derive(Clone, PartialEq, Eq)]
struct PreviewTarget {
    key: String,
}

/// 两行之间的分组标题（多来源时才有）。
fn group_header(name: SharedString) -> Div {
    div()
        .flex_shrink_0()
        .px_2p5()
        .pt_1()
        .pb_0p5()
        .overflow_hidden()
        .text_ellipsis()
        .whitespace_nowrap()
        .text_size(TEXT_META)
        .text_color(rgb(INK_SOFT))
        .child(name)
}

fn hint_line(text: SharedString) -> Div {
    div().px_2p5().py_2().text_size(TEXT_SMALL).text_color(rgb(INK_SOFT)).child(text)
}

/// 说明卡的整体定高（两行文案 + 胶囊 + 内边距）：每行卡片一样高，面板不会跟着伸缩。
const CARD_H: Rems = rems(5.5);

/// 品牌图标：有内置 SVG 就单色着色，没有就画圆角首字母徽章；
/// 中继站的 `cn:xxx` 这类认不出品牌的用中性图标，字母徽章只会是噪音。
fn brand_icon(brand: ModelBrand, model: &str) -> Div {
    let slot = div().size_4().flex_shrink_0().flex().items_center().justify_center();
    match brand.svg_path() {
        Some(path) => {
            let color = brand
                .tint_hex(crate::theme::is_dark())
                .map(gpui_kit::rgb)
                .unwrap_or_else(|| rgb(INK_SOFT));
            slot.child(Icon::empty().path(path).size_3p5().text_color(color))
        }
        None if brand == ModelBrand::Generic => {
            slot.child(Icon::new(IconAll::Bot).size_3p5().text_color(rgb(INK_FAINT)))
        }
        None => slot
            .rounded_xs()
            .bg(rgb(SAND_ACTIVE))
            .text_size(rems(0.625))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(rgb(INK_SOFT))
            .child(brand.monogram(model).to_string()),
    }
}

/// 上下文窗口列：定宽、右对齐、等宽数字，没有数据也留空占位保持各列对齐。
fn context_column(context: Option<String>) -> Div {
    let col = div()
        .w(rems(3.25))
        .flex_shrink_0()
        .h_flex()
        .justify_end()
        .text_size(TEXT_SMALL)
        .text_color(rgb(INK_SOFT))
        .font_features(tabular_nums());
    match context {
        Some(text) => col.child(text),
        None => col,
    }
}

/// 价格档位列：固定 4 格 `$`，没有单价就整列留白（不猜）。
fn cost_column(tier: Option<u8>) -> Div {
    let mut col = div()
        .w(rems(2.5))
        .flex_shrink_0()
        .h_flex()
        .justify_end()
        .gap_0p5()
        .text_size(TEXT_META)
        .font_features(tabular_nums());
    if let Some(tier) = tier {
        for i in 1..=4u8 {
            let color = if i <= tier { INK_SOFT } else { INK_FAINT };
            col = col.child(div().text_color(rgb(color)).child("$"));
        }
    }
    col
}

/// 工具栏 trigger：`[品牌图标] 模型名 档位 ⌃/⌄`，行里放不下时模型名打省略号。
#[derive(IntoElement)]
pub(super) struct ModelPickerChip {
    brand: ModelBrand,
    model: SharedString,
    effort: Option<&'static str>,
    open: bool,
}

impl ModelPickerChip {
    pub(super) fn new(
        brand: ModelBrand,
        model: impl Into<SharedString>,
        effort: Option<&'static str>,
    ) -> Self {
        Self { brand, model: model.into(), effort, open: false }
    }
}

impl Selectable for ModelPickerChip {
    fn selected(mut self, selected: bool) -> Self {
        self.open = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.open
    }
}

impl RenderOnce for ModelPickerChip {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let mut chip = div()
            .id(ElementId::Name(format!("composer-model-chip-{}", self.model).into()))
            .role(Role::Button)
            .aria_expanded(self.open)
            .aria_label(self.model.clone())
            .h_flex()
            .items_center()
            .min_w_0()
            .gap_1p5()
            .py_1()
            .px_2()
            .rounded_md()
            // 一行排不下时先缩它：图标 / 档位 / 箭头固定，只有模型名让宽度
            .flex_shrink_1()
            .min_w(rems(5.5));
        chip = if self.open { chip.bg(rgb(HOVER)) } else { chip.hover(|d| d.bg(rgb(HOVER))) };
        chip = chip.child(brand_icon(self.brand, &self.model)).child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_size(TEXT_SMALL)
                .text_color(rgb(INK_SOFT))
                .child(self.model),
        );
        if let Some(effort) = self.effort {
            chip = chip.child(
                div().flex_shrink_0().text_size(TEXT_META).text_color(rgb(INK_FAINT)).child(effort),
            );
        }
        chip.child(
            Icon::new(if self.open { IconAll::ChevronUp } else { IconAll::ChevronDown })
                .size_3()
                .flex_shrink_0()
                .text_color(rgb(INK_SOFT)),
        )
    }
}

/// 面板本体（独立 entity，悬停只重绘自己）。
pub(super) struct ModelPickerContent {
    app: Entity<ChatApp>,
    popover: Entity<PopoverState>,
    hovered: Option<PreviewTarget>,
    /// 第一次打开时把当前模型滚进可视区，之后不再打扰用户的手动滚动
    scrolled_to_current: bool,
    /// 首帧没有子元素位置，滚动最多重试几帧
    scroll_attempts: u8,
    _app_observed: Subscription,
    _popover_observed: Subscription,
}

impl ModelPickerContent {
    fn new(app: Entity<ChatApp>, popover: Entity<PopoverState>, cx: &mut Context<Self>) -> Self {
        let _app_observed = cx.observe(&app, |_, _, cx| cx.notify());
        // 收起时清掉残留的悬停预览，下次打开从当前模型开始
        let _popover_observed = cx.observe(&popover, |this, _, cx| {
            if !this.popover.read(cx).is_open() {
                this.scrolled_to_current = false;
                this.scroll_attempts = 0;
                if this.hovered.take().is_some() {
                    cx.notify();
                }
            }
        });
        Self {
            app,
            popover,
            hovered: None,
            scrolled_to_current: false,
            scroll_attempts: 0,
            _app_observed,
            _popover_observed,
        }
    }

    /// 只在移入时换预览：行之间有空隙，移出就退回当前模型会让说明卡来回闪。
    /// 收起时由 `_popover_observed` 复位。
    fn set_hover(&mut self, target: PreviewTarget, cx: &mut Context<Self>) {
        if self.hovered.as_ref() == Some(&target) {
            return;
        }
        self.hovered = Some(target);
        cx.notify();
    }
}

/// 自研内核：所有来源里进菜单的模型平铺成行（设置里勾掉的除外）。
fn builtin_rows(app: &ChatApp) -> (Vec<RowEntry>, Option<String>) {
    let session = app.session();
    let current = session.model_label.clone();
    let is_chat = session.is_chat();
    let web = session.web;
    let active_profile = app.settings.builtin.active;
    let multi = app.settings.builtin.profiles.len() > 1;
    let mut rows = Vec::new();
    let mut any = false;
    for (pi, profile) in app.settings.builtin.profiles.iter().enumerate() {
        let models = profile.menu_models();
        if models.is_empty() {
            continue;
        }
        any = true;
        if multi {
            rows.push(RowEntry::Header(profile.display_name().into()));
        }
        let protocol = serde_json::to_string(&profile.protocol).unwrap_or_default();
        for (i, model) in models.iter().enumerate() {
            let banned =
                app.settings.no_vision_models.iter().any(|m| m.eq_ignore_ascii_case(model));
            rows.push(RowEntry::Row(RowData {
                key: format!("{pi}-{i}"),
                profile: Some(pi),
                value: (*model).clone(),
                name: (*model).clone(),
                brand: ModelBrand::detect(model, &profile.base_url),
                context: profile
                    .context_window(model)
                    .or_else(|| model_catalog::fallback_context(model))
                    .map(model_catalog::format_context),
                tier: model_catalog::cost_tier(app.settings.model_prices.as_deref(), model),
                active: pi == active_profile && current.as_ref() == model.as_str(),
                detail: model_catalog::summary(model).map(SharedString::from),
                caps: model_catalog::capabilities(&protocol, model, banned, is_chat, web),
            }));
        }
    }
    let hint = (!any).then(|| {
        if app.fetching_models {
            "Fetching models…"
        } else if app.settings.builtin.models().is_empty() {
            "No models yet — fetch them in Settings"
        } else {
            "All models are hidden — enable some in Settings"
        }
        .to_string()
    });
    (rows, hint)
}

/// 外部 ACP agent：列出它上报的 model 配置项。
fn agent_rows(app: &ChatApp) -> (Vec<RowEntry>, Option<String>) {
    let session = app.session();
    let Some(config) = session
        .config_options
        .iter()
        .find(|option| option.category == "model" || option.id == "model")
    else {
        return (
            Vec::new(),
            Some("The agent reports no model list — switch models in its own UI".into()),
        );
    };
    let mut rows = Vec::new();
    let mut group: Option<String> = None;
    for (i, choice) in config.options.iter().enumerate() {
        if choice.group != group {
            group = choice.group.clone();
            if let Some(name) = &group {
                rows.push(RowEntry::Header(name.clone().into()));
            }
        }
        rows.push(RowEntry::Row(RowData {
            key: format!("agent-{i}"),
            profile: None,
            value: choice.id.clone(),
            name: choice.name.clone(),
            brand: ModelBrand::detect(&choice.id, ""),
            context: None,
            tier: model_catalog::cost_tier(None, &choice.id),
            active: choice.id == config.current_value,
            detail: Some(
                choice
                    .description
                    .clone()
                    .filter(|d| !d.trim().is_empty())
                    .unwrap_or_else(|| choice.id.clone())
                    .into(),
            ),
            caps: Vec::new(),
        }));
    }
    (rows, None)
}

impl Render for ModelPickerContent {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 先把数据读成自有值，后面的 `cx.listener` 要可变借用 cx
        let (rows, hint, effort, default_key, builtin) = {
            let app = self.app.read(cx);
            let builtin = app.session().kernel == KernelChoice::Builtin;
            let (rows, hint) = if builtin { builtin_rows(app) } else { agent_rows(app) };
            let default_key = rows.iter().find_map(|entry| match entry {
                RowEntry::Row(row) if row.active => Some(row.key.clone()),
                _ => None,
            });
            // 认不出的模型也给底栏：只有「默认 / 兜底」两项，和设置页、旧输入框菜单一致
            let effort = builtin.then(|| {
                let spec = session_thinking_spec(app);
                (active_thinking(&spec, app.session().thinking), thinking_options(&spec))
            });
            (rows, hint, effort, default_key, builtin)
        };

        let hovered = self.hovered.clone().or(default_key.map(|key| PreviewTarget { key }));
        let card = hovered.as_ref().and_then(|target| {
            rows.iter().find_map(|entry| match entry {
                RowEntry::Row(row) if row.key == target.key => (row.detail.is_some()
                    || !row.caps.is_empty())
                .then(|| (row.detail.clone(), row.caps.clone())),
                _ => None,
            })
        });

        let scroll = window
            .use_keyed_state("model-menu-scroll", cx, |_, _| ScrollHandle::new())
            .read(cx)
            .clone();
        let mut list = div()
            .id("composer-model-list")
            .v_flex()
            .gap_0p5()
            .max_h(rems(15.))
            .overflow_y_scroll()
            .track_scroll(&scroll);
        let mut visible = false;
        let mut current_index = None;
        for (index, entry) in rows.into_iter().enumerate() {
            match entry {
                RowEntry::Header(name) => list = list.child(group_header(name)),
                RowEntry::Row(row) => {
                    visible = true;
                    if row.active && current_index.is_none() {
                        current_index = Some(index);
                    }
                    list = list.child(self.render_row(row, cx));
                }
            }
        }
        // 当前模型可能在折叠区外：第一次打开时滚到它那儿。
        // 首帧还没有子元素的位置，滚动请求会被丢掉，所以头几帧重复请求直到位移生效
        // （GPUI 向下滚之后 offset.y 是负的，别拿 `> 0` 判断）。
        if self.scrolled_to_current || scroll.offset().y != px(0.) {
            self.scrolled_to_current = true;
        } else if self.scroll_attempts < 3
            && let Some(index) = current_index
        {
            scroll.scroll_to_item(index);
            // 内容没脏时不会重新 prepaint，滚动位置就落不了地：补一帧
            window.request_animation_frame();
            self.scroll_attempts += 1;
        }
        if !visible && let Some(hint) = hint {
            list = list.child(hint_line(hint.into()));
        }

        let mut panel = div().v_flex().gap_0p5();
        if builtin {
            panel = panel.child(
                div().h_flex().justify_end().px_1().child(
                    ChatApp::quiet_button(
                        "composer-model-refresh",
                        IconAll::RefreshCw,
                        Surface::Paper,
                        cx,
                    )
                    .xsmall()
                    .tooltip("刷新模型列表")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.app.update(cx, |app, cx| app.fetch_models(false, cx));
                    })),
                ),
            );
        }
        panel = panel.child(list);
        if let Some((detail, caps)) = card {
            panel = panel.child(render_card(detail, caps));
        }
        if let Some((current, options)) = effort {
            panel = panel.child(self.render_effort(current, options, cx));
        }
        panel
    }
}

impl ModelPickerContent {
    fn render_row(&self, row: RowData, cx: &mut Context<Self>) -> Stateful<Div> {
        let RowData { key, profile, value, name, brand, context, tier, active, detail: _, caps: _ } =
            row;
        let target = PreviewTarget { key: key.clone() };
        let id = ElementId::Name(format!("composer-model-{key}").into());
        let click_value = value.clone();
        div()
            .id(id)
            .role(Role::MenuItem)
            .aria_selected(active)
            .h_flex()
            .items_center()
            .gap_2()
            .px_2p5()
            .py_1p5()
            .rounded_lg()
            .cursor_pointer()
            .when(active, |d| d.bg(rgb(SELECTED)).hover(|d| d.bg(rgb(SELECTED))))
            .when(!active, |d| d.hover(|d| d.bg(rgb(HOVER))))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if *hovered {
                    this.set_hover(target.clone(), cx);
                }
            }))
            .on_click(cx.listener(move |this, _, window, cx| {
                let value = click_value.clone();
                match profile {
                    Some(pi) => {
                        this.app.update(cx, |app, cx| app.choose_model_from(pi, value, cx));
                    }
                    None => {
                        this.app.update(cx, |app, cx| app.choose_model(value, cx));
                    }
                }
                this.popover.update(cx, |state, cx| state.dismiss(window, cx));
            }))
            .child(brand_icon(brand, &value))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_flex()
                    .items_center()
                    .gap_1p5()
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(TEXT_BODY)
                            .text_color(rgb(INK))
                            .child(name),
                    )
                    .when(active, |d| {
                        d.child(
                            Icon::new(IconAll::Check)
                                .size_3p5()
                                .flex_shrink_0()
                                .text_color(rgb(INK)),
                        )
                    }),
            )
            .child(context_column(context))
            .child(cost_column(tier))
    }

    fn render_effort(
        &self,
        current: Option<ThinkingLevel>,
        options: Vec<(Option<ThinkingLevel>, &'static str)>,
        cx: &mut Context<Self>,
    ) -> Div {
        let mut track =
            div().h_flex().items_center().gap_0p5().p_0p5().rounded_full().bg(rgb(SAND));
        for (i, (level, _)) in options.iter().enumerate() {
            let level = *level;
            let selected = current == level;
            let label = model_catalog::effort_label_en(level);
            let mut segment = div()
                .id(("composer-effort", i))
                .role(Role::MenuItem)
                .aria_selected(selected)
                .px_1p5()
                .py_0p5()
                .rounded_full()
                .cursor_pointer()
                .text_size(TEXT_META)
                .child(label);
            segment = if selected {
                segment
                    .bg(rgb(CARD))
                    .shadow(raised_shadow())
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(rgb(INK))
            } else {
                segment.text_color(rgb(INK_SOFT)).hover(|d| d.text_color(rgb(INK)))
            };
            let segment = segment.on_click(cx.listener(move |this, _, _, cx| {
                // 换档位不收起面板：接着挑模型
                this.app.update(cx, |app, cx| app.choose_thinking(level, cx));
            }));
            track = track.child(segment);
        }
        // 档位多（7、8 档）时省掉标签，先把分段器排下
        let show_label = options.len() <= 5;
        div()
            .h_flex()
            .items_center()
            .justify_between()
            .gap_2()
            .pt_2p5()
            .px_1p5()
            .when(show_label, |d| {
                d.child(
                    div()
                        .flex_shrink_0()
                        .text_size(TEXT_BODY)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(rgb(INK))
                        .child("Effort"),
                )
            })
            .child(track)
    }
}

/// 悬停说明卡：一句话定位 + 能力胶囊。
///
/// 整张卡定高并垂直居中：目录里没有的模型（中转站的 `cn:xxx`、新发模型）只有胶囊，
/// 换成别的行时卡片不能跟着长高变矮，否则列表会上下跳、鼠标底下的行跟着变。
fn render_card(detail: Option<SharedString>, caps: Vec<&'static str>) -> Div {
    // 没有说明的模型留一行淡色占位，卡片高度才不跟着悬停变
    let (text, color) = match detail {
        Some(text) => (text, INK_SOFT),
        None => ("No description available for this model.".into(), INK_FAINT),
    };
    div()
        .v_flex()
        .justify_center()
        .h(CARD_H)
        .mt_1p5()
        .px_3()
        .py_2p5()
        .rounded_lg()
        .bg(rgb(SAND))
        .child(
            div()
                .mb_2()
                .line_clamp(2)
                .text_size(TEXT_SMALL)
                .line_height(LEADING)
                .text_color(rgb(color))
                .child(text),
        )
        .when(!caps.is_empty(), |d| {
            d.child(div().h_flex().flex_wrap().gap_1p5().children(caps.into_iter().map(|cap| {
                div()
                    .px_2()
                    .py_0p5()
                    .rounded_md()
                    .bg(rgb(CARD))
                    .border_1()
                    .border_color(rgb(LINE))
                    .text_size(TEXT_META)
                    .text_color(rgb(INK))
                    .child(cap)
            })))
        })
}

impl ChatApp {
    /// 输入框工具栏的模型下拉：trigger 上带品牌图标、当前模型和生效的思考档位。
    /// 打开时顺手获取一次模型列表；面板宽 22.5rem。
    pub(super) fn render_model_picker(&self, cx: &mut Context<Self>) -> Popover {
        let session = self.session();
        let builtin = session.kernel == KernelChoice::Builtin;
        // ACP 会话不借自研档案的端点认品牌，认不出就用中性图标
        let base_url = if builtin { self.settings.builtin.current().base_url.as_str() } else { "" };
        let brand = ModelBrand::detect(&session.model_label, base_url);
        let effort = if builtin {
            let spec = session_thinking_spec(self);
            Some(model_catalog::effort_label_en(active_thinking(&spec, session.thinking)))
        } else {
            None
        };
        let chip = ModelPickerChip::new(brand, session.model_label.clone(), effort);
        let handle = cx.entity();
        let fetch = handle.clone();
        let content_app = handle.clone();
        menu_panel(Popover::new("model-menu"), &self.menu_focus)
            .anchor(Anchor::BottomRight)
            .w(rems(22.5))
            .p_2()
            .trigger_style(StyleRefinement::default().flex_shrink_1().min_w(rems(5.5)))
            .trigger(chip)
            .on_open_change(move |open, _, cx| {
                if *open {
                    fetch.update(cx, |app, cx| app.fetch_models_for_menu(cx));
                }
            })
            .default_open(
                cfg!(feature = "headless-screenshot")
                    && std::env::var("CLAUDESKTOP_OPEN").as_deref() == Ok("composer-model-menu"),
            )
            .content(move |_, window, cx| {
                let popover = cx.entity();
                // 面板内容只建一次：悬停换说明卡时由它自己 notify，不惊动会话区
                let content = window.use_keyed_state("model-menu-content", cx, |_, cx| {
                    cx.new(|cx| ModelPickerContent::new(content_app.clone(), popover, cx))
                });
                content.read(cx).clone()
            })
    }
}

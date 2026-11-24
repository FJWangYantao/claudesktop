//! 内容区：输入框上方那一块（对话列表 / 首页概览 / 设置页 / 子任务详情），是独立的缓存 view（[`ContentView`]）。
//!
//! 打字时输入框通知它自己的 view，祖先 `ChatApp` 跟着重画；内容区不是输入框的祖先，缓存照旧复用，
//! 所以打一个字只重画输入区、顶栏这些，不重画对话（长 markdown、代码块一帧好几毫秒）。
//! 失效规则很粗：`ChatApp` 每次 `cx.notify()` 都通知内容区重画（`ChatApp::new` 里的 `observe_self`），
//! 不用像侧栏那样维护指纹。反过来，只影响输入区的刷新要用 `ChatApp::refresh_composer`，
//! 别用 `cx.notify()`，否则又连对话一起重画。
//! 内容区里的动画（展开、悬停淡入、平滑滚动）通知的是当前 view，也就是内容区自己；
//! 常驻的循环动画（转圈、呼吸点、扫光）例外，由叠在上面的 `motion::LoopOverlay` 画（见 motion.rs 文件头）。

use super::widgets::edge_fade;
use super::{ChatApp, motion};
use crate::model::ChatItem;
use crate::session::{ListMetrics, Row, WheelScroll, scroll_handle_fades};
use crate::theme::rgb;
use crate::theme::*;
use gpui_kit::component::scroll::{Scrollbar, ScrollbarMode};
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::time::Instant;

/// 内容区的缓存 view：内容全由 `ChatApp::render_content` 画，这里只是把它隔成一块单独重画的区域。
pub(crate) struct ContentView {
    app: WeakEntity<ChatApp>,
}

impl ContentView {
    pub(super) fn new(app: WeakEntity<ChatApp>) -> Self {
        Self { app }
    }
}

impl Render for ContentView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        motion::begin_loop_spots();
        match self.app.upgrade() {
            Some(app) => app.update(cx, |app, cx| app.render_content(window, cx)),
            None => div().into_any_element(),
        }
    }
}

impl ChatApp {
    /// 内容区在 `ChatApp` 里的位置：占满输入区以上的高度（按 style 排版，不按内容量），
    /// 上面叠着画循环动画的覆盖层。
    pub(super) fn content_slot(&self) -> impl IntoElement + use<> {
        div()
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .child(
                image_cache(self.image_cache.clone()).size_full().child(
                    self.content_view.clone().cached(StyleRefinement::default().size_full()),
                ),
            )
            .child(self.loop_overlay.clone())
    }

    /// 设置页 / 模型清单的平滑滚动（指数逼近，参数见 `motion::WHEEL_*`）；模型清单有独立的内层滚动区。
    pub(super) fn settings_wheel_by(
        &mut self,
        delta: Pixels,
        precise: bool,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let model = &self.settings_form.model_scroll;
        let top = -model.offset().y;
        let max = model.max_offset().y.max(px(0.));
        let in_model = self.settings_form.tab == super::settings_view::MODEL_TAB
            && model.bounds().contains(&position)
            && if delta > px(0.) { top < max } else { top > px(0.) };
        let reduced = cx.reduce_motion();
        if in_model {
            self.settings_wheel = None;
            Self::push_page_wheel(model, &mut self.model_wheel, delta, precise || reduced);
        } else {
            self.model_wheel = None;
            Self::push_page_wheel(
                &self.settings_scroll,
                &mut self.settings_wheel,
                delta,
                precise || reduced,
            );
        }
        self.content_view.update(cx, |_, cx| cx.notify());
    }

    fn push_page_wheel(
        handle: &ScrollHandle,
        wheel: &mut Option<WheelScroll>,
        delta: Pixels,
        direct: bool,
    ) {
        let max = handle.max_offset().y.max(px(0.));
        let top = -handle.offset().y;
        if direct {
            *wheel = None;
            Self::set_page_top(handle, top + delta);
            return;
        }
        Self::set_page_top(handle, top + delta * motion::WHEEL_INSTANT);
        let top = -handle.offset().y;
        let base = match *wheel {
            Some(w) if (w.target > top) == (delta > px(0.)) => {
                if delta > px(0.) {
                    w.target.min(top + motion::WHEEL_MAX_LEAD)
                } else {
                    w.target.max(top - motion::WHEEL_MAX_LEAD)
                }
            }
            _ => top,
        };
        let now = std::time::Instant::now();
        let last = wheel
            .map_or(now.checked_sub(std::time::Duration::from_millis(16)).unwrap_or(now), |w| {
                w.last
            });
        *wheel = Some(WheelScroll {
            target: (base + delta * (1. - motion::WHEEL_INSTANT)).clamp(px(0.), max),
            started: now,
            last,
        });
    }

    fn set_page_top(handle: &ScrollHandle, top: Pixels) {
        let mut offset = handle.offset();
        offset.y = -top.clamp(px(0.), handle.max_offset().y.max(px(0.)));
        handle.set_offset(offset);
    }

    fn step_page_wheel(handle: &ScrollHandle, wheel: &mut Option<WheelScroll>) -> bool {
        let Some(mut w) = wheel.take() else { return false };
        if w.started.elapsed() > motion::WHEEL_MAX {
            return false;
        }
        let now = std::time::Instant::now();
        let dt = (now - w.last).as_secs_f32().min(0.1);
        w.last = now;
        w.target = w.target.clamp(px(0.), handle.max_offset().y.max(px(0.)));
        let top = -handle.offset().y;
        let rest = w.target - top;
        if rest.abs() > motion::WHEEL_MAX_LEAD {
            w.target = if rest > px(0.) {
                top + motion::WHEEL_MAX_LEAD
            } else {
                top - motion::WHEEL_MAX_LEAD
            };
        }
        let rest = w.target - top;
        let step = (rest * (1. - (-dt / motion::WHEEL_TAU).exp()))
            .clamp(-motion::WHEEL_MAX_STEP, motion::WHEEL_MAX_STEP);
        if (rest - step).abs() < px(0.5) {
            Self::set_page_top(handle, top + rest);
            return false;
        }
        Self::set_page_top(handle, top + step);
        *wheel = Some(w);
        true
    }

    fn step_settings_wheel(&mut self, window: &mut Window) {
        let outer = Self::step_page_wheel(&self.settings_scroll, &mut self.settings_wheel);
        let inner = if self.settings_form.tab == super::settings_view::MODEL_TAB {
            Self::step_page_wheel(&self.settings_form.model_scroll, &mut self.model_wheel)
        } else {
            self.model_wheel = None;
            false
        };
        if outer || inner {
            window.request_animation_frame();
        }
    }

    /// 内容区：设置页、首页（问候 + 用量概览）或当前会话的对话列表。
    fn render_content(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        // 这一帧重新登记工具输出的滚动区（列表只画视口附近的行，登记也跟着来）
        self.tool_scroll_regions.borrow_mut().clear();
        if self.settings_open {
            // 对话这会儿没在画：滚轮余量作废，别等关了设置再补上几秒前的那几格
            self.sessions[self.active].wheel = None;
            self.step_settings_wheel(window);
            return v_flex().size_full().child(self.render_settings(window, cx)).into_any_element();
        }
        let active = self.active;
        if let Some(message) = self.sessions[active].blocks_error.clone() {
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_size(TEXT_SMALL)
                .text_color(rgb(INK_SOFT))
                .child(message)
                .into_any_element();
        }
        if !self.sessions[active].history_loaded {
            let message = self.sessions[active]
                .history_error
                .clone()
                .unwrap_or_else(|| "正在读取这段对话…".into());
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_size(TEXT_SMALL)
                .text_color(rgb(INK_SOFT))
                .child(message)
                .into_any_element();
        }
        // 点开了子任务：详情页占内容区；那个子任务已经不在了（回退过）就回到对话
        if let Some(task) = self.sessions[active].viewing_task.clone() {
            if let Some(page) = self.render_task_page(&task, window, cx) {
                // 对话这会儿没在画，滚轮余量作废
                self.sessions[active].wheel = None;
                return page;
            }
            self.sessions[active].viewing_task = None;
        }
        // 审查这一轮的改动：同样占内容区；会话换了、发了新消息、又跑起来了就回到对话
        if self.review_open() {
            self.sessions[active].wheel = None;
            return self.render_review_page(cx);
        }
        self.review = None;
        // 还没发过消息的会话显示首页（与官方空会话一致，输入框在底部）
        if !self.sessions[active].has_user_message() {
            // 放大字号后放不下时整页往下长、可以滚（min_h 而不是 h：定高时居中的内容会从上下两头溢出，
            // 问候语顶到顶栏下面）；上下渐隐同对话区
            let paper = rgb(PAPER);
            let clear = Rgba { a: 0., ..paper };
            return div()
                .relative()
                .size_full()
                .child(
                    div()
                        .id("home-scroll")
                        .size_full()
                        .overflow_y_scroll()
                        .track_scroll(&self.home_scroll)
                        .child(
                            div()
                                .min_h_full()
                                .py_4()
                                .v_flex()
                                // 定宽，滚动时不用每帧把整页来回排好几遍（见 `MeasuredWidth`）
                                .when_some(self.home_w.get(), |d, w| d.w(w))
                                .child(
                                    self.reading_column(div().flex_1().v_flex().justify_center())
                                        .child(self.render_overview(cx)),
                                ),
                        ),
                )
                .child(self.home_w.probe())
                .child({
                    // 上下渐隐跟手，按距边距离连续收放（贴顶 / 贴底时对应那条收成 0）
                    let rem = window.rem_size();
                    let (top, bottom) = scroll_handle_fades(&self.home_scroll, rem);
                    div()
                        .absolute()
                        .inset_0()
                        .child(edge_fade(top, paper, clear).top_0())
                        .child(edge_fade(bottom, clear, paper).bottom_0())
                })
                .into_any_element();
        }

        self.step_jump_to_bottom(window);
        // 虚拟列表：只布局 / 绘制视口附近的行；最后一行是运行状态行（见 Session::sync_list）
        let view = self.settings.view_mode;
        // 未测行的估高按当前列宽 / 字号算：窗口变窄、换字号、切宽阅读时整表按新度量重估
        let list_w = self.sessions[active].list.viewport_bounds().size.width;
        let target = if self.settings.wide_reading { READING_WIDE_W } else { READING_W };
        let column_w = if list_w > px(0.) { list_w.min(target) } else { target };
        self.sessions[active].set_list_metrics(ListMetrics {
            column_w,
            rem: window.rem_size(),
            font_rev: crate::theme::font_revision(),
            measured: list_w > px(0.),
        });
        self.sessions[active].sync_list(view);
        // 滚轮在列表状态（行数、定位）都更新完之后才推进这一帧，免得和跳转、补行抢
        self.step_wheel(window);
        // 展开 / 收起卡片：把被点的那一行钉回屏幕原位（按上一帧布局量到的偏差补偿）
        if self.sessions[active].step_pin(Instant::now()) {
            window.request_animation_frame();
        }
        // 跟随判定放到最后：这一帧的位移都落位了，再按几何 + 方向决定吸附 / 脱离
        let moving = self.jump.is_some();
        self.sessions[active].sync_follow(Instant::now(), moving);
        // 这一帧还在滚（平滑余量 / 回底动画 / 正拖着滚动条）：悬停变化不再单独通知内容区
        if self.jump.is_some()
            || self.sessions[active].wheel.is_some()
            || self.sessions[active].list.is_scrollbar_dragging()
        {
            self.scrolling_until = Some(Instant::now() + std::time::Duration::from_millis(120));
        }
        let state = self.sessions[active].list.clone();
        let scrollbar_state = state.clone();
        let scroll = list(
            state,
            cx.processor(move |this, ix: usize, window, cx| {
                let s = &this.sessions[this.active];
                let row = this.reading_column(div());
                let content = match s.rows.get(ix).copied() {
                    // 拆开的大回复的一块（见 `session::Row`）：块内没有分组和展开裁切，
                    // 只有第一块带顶部留白，最后一块挂操作行 / 改动卡片
                    Some(Row::Part { src, part }) => {
                        let body = this.render_agent_part(src, part, window, cx);
                        // 最后一块留出和普通回合末尾一样的下边距
                        let tail = s.part_count(src).is_some_and(|count| part + 1 == count);
                        let body = if part == 0 {
                            this.with_flash(div().relative(), src, None, window, cx).child(body)
                        } else {
                            div().relative().child(body)
                        };
                        let body = if tail { body.pb(super::chat::STEP_GAP) } else { body };
                        row.pt(px(if ix == 0 { 16. } else { 0. })).child(body)
                    }
                    Some(Row::Whole(source)) => {
                        // 过程组里的一步：展开 / 收起走弹簧，收起没走完之前照样画、按进度裁切
                        // （每帧都要问一次进度，状态才留得住）
                        let reveal = match s.layout.get(source) {
                            Some(layout) if layout.member => {
                                // 只有用户手动点过的那一组播弹簧；自动收起（新行涌入、正文到达）直接落位，
                                // 否则每出一行列表高度都一涨一缩
                                let animate = layout
                                    .group_start
                                    .is_some_and(|start| s.animated_group == Some(start));
                                motion::reveal_progress_when(
                                    ("step", source),
                                    layout.body,
                                    animate,
                                    window,
                                    cx,
                                )
                            }
                            _ => 1.,
                        };
                        match (s.transcript.get(source), s.layout.get(source)) {
                            // 收起的组内步骤、当前视图不显示的行：零高度（分组结果在测量指纹里，变了就重测）。
                            // 没有正文回复的回合末尾（改动卡 / 用时）不算收起：它挂在 row 这一层，得画出来
                            (Some(_), Some(layout))
                                if !layout.body
                                    && layout.head.is_none()
                                    && (!layout.member || reveal <= 0.)
                                    && !s.is_turn_fallback_tail(source, view) =>
                            {
                                div()
                            }
                            (Some(item), Some(&layout)) => {
                                let body =
                                    this.render_row(source, item, layout, reveal, window, cx);
                                // 刚从搜索结果 / 改动列表跳过来的那条：闪一下（见 search.rs）。
                                // diff 卡片自己有底色盖不住，改成描边；描边不框进卡片下面的行距
                                let card = matches!(item, ChatItem::Tool(c) if !c.diffs.is_empty())
                                    .then(|| ChatApp::row_gap(layout));
                                let body = this.with_flash(
                                    div().relative().child(body),
                                    source,
                                    card,
                                    window,
                                    cx,
                                );
                                row.pt(px(if ix == 0 { 16. } else { 0. })).child(body)
                            }
                            _ => row,
                        }
                    }
                    None if ix == s.rows.len() => row.pb_12().children(this.render_turn_status()),
                    _ => row,
                };
                div().w_full().child(content).into_any_element()
            }),
        )
        .flex_1()
        .min_h_0();

        // 上下边缘渐隐：内容滚到顶栏 / 输入区下面时淡出，不是一刀切。高度按滚到哪一档给
        // （贴顶 / 贴底时对应那条收成 0），不超过首行上边距和状态行下边距，滚到头时不遮内容；
        // 覆盖层不挂事件，不挡点击和滚轮
        let paper = rgb(PAPER);
        let clear = Rgba { a: 0., ..paper };
        let rem = window.rem_size();
        let (fade_top, fade_bottom) = self.sessions[active].scroll_fades(rem);
        // 滚轮平滑：capture 阶段截住滚轮累计相对位移（见 `ChatApp::wheel_by`），列表自己在 bubble 阶段
        // 一格硬跳的处理就不跑了。只有横向（Shift+滚轮）时放行。
        // 命中区不挡悬停；弹层、对话框盖在上面时 `should_handle_scroll` 为 false，不抢它们的滚轮
        let app = cx.entity().downgrade();
        let line = rems(1.75).to_pixels(window.rem_size());
        let wheel = canvas(
            |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
            move |_, hitbox, window, _| {
                window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
                    if !phase.capture() || !hitbox.should_handle_scroll(window) {
                        return;
                    }
                    // gpui 的 delta.y 正 = 往开头
                    let dy = event.delta.pixel_delta(line).y;
                    if dy == px(0.) {
                        return;
                    }
                    let precise = event.delta.precise()
                        || std::env::var_os("CLAUDESKTOP_WHEEL").is_some_and(|v| v == "direct");
                    // 指针在展开的工具输出上、那个框还能往这个方向滚：capture 阶段就把它滚掉，
                    // 外层对话列表不再接这一格（放给 bubble 的话列表也会跟着硬跳一次）
                    let pos = event.position;
                    let handled = app
                        .update(cx, |this, cx| {
                            // 内层滚到底之后，余量交给外层；没交还就是内层吃掉（见 `scroll_tool_output`）
                            if let Some(rest) = this.scroll_tool_output(pos, dy, cx) {
                                this.wheel_by(-rest, precise, cx);
                            }
                            true
                        })
                        .unwrap_or(false);
                    if handled {
                        cx.stop_propagation();
                    }
                });
            },
        )
        .absolute()
        .inset_0();
        // 按会话划开元素状态：展开进度、悬停淡入这些按行号记的状态不串到别的会话
        div()
            .id(("transcript", self.sessions[active].token))
            .relative()
            .size_full()
            .v_flex()
            .child(scroll)
            .child(wheel)
            // 滚动 / 悬停浮现的滚动条：叠在内容之上（不挤占阅读列宽度），
            // 拖动时列表内部冻结总高，拖到底自动恢复跟随（见 `ListState::set_offset_from_scrollbar`）
            .child(Scrollbar::vertical(&scrollbar_state).mode(ScrollbarMode::Scrolling))
            .child(edge_fade(fade_top, paper, clear).top_0())
            .child(edge_fade(fade_bottom, clear, paper).bottom_0())
            .children(self.render_jump_to_bottom(window, cx))
            .into_any_element()
    }
}

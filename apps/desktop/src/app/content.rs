//! 内容区：输入框上方那一块（对话列表 / 首页概览 / 设置页 / 子任务详情），是独立的缓存 view（[`ContentView`]）。
//!
//! 打字时输入框通知它自己的 view，祖先 `ChatApp` 跟着重画；内容区不是输入框的祖先，缓存照旧复用，
//! 所以打一个字只重画输入区、顶栏这些，不重画对话（长 markdown、代码块一帧好几毫秒）。
//! 失效规则很粗：`ChatApp` 每次 `cx.notify()` 都通知内容区重画（`ChatApp::new` 里的 `observe_self`），
//! 不用像侧栏那样维护指纹。反过来，只影响输入区的刷新要用 `ChatApp::refresh_composer`，
//! 别用 `cx.notify()`，否则又连对话一起重画。
//! 内容区里的动画（展开、悬停淡入、平滑滚动）通知的是当前 view，也就是内容区自己；
//! 常驻的循环动画（转圈、呼吸点、扫光）例外，由叠在上面的 `motion::LoopOverlay` 画（见 motion.rs 文件头）。

use super::{ChatApp, motion};
use crate::model::ChatItem;
use crate::theme::rgb;
use crate::theme::*;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

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
            .child(self.content_view.clone().cached(StyleRefinement::default().size_full()))
            .child(self.loop_overlay.clone())
    }

    /// 内容区：设置页、首页（问候 + 用量概览）或当前会话的对话列表。
    fn render_content(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if self.settings_open {
            return v_flex().size_full().child(self.render_settings(window, cx)).into_any_element();
        }
        let active = self.active;
        // 点开了子任务：详情页占内容区；那个子任务已经不在了（回退过）就回到对话
        if let Some(task) = self.sessions[active].viewing_task.clone() {
            if let Some(page) = self.render_task_page(&task, window, cx) {
                return page;
            }
            self.sessions[active].viewing_task = None;
        }
        // 审查这一轮的改动：同样占内容区；会话换了、发了新消息、又跑起来了就回到对话
        if self.review_open() {
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
                                .child(self.render_overview(cx)),
                        ),
                )
                .child(self.home_w.probe())
                .child(edge_fade(px(16.), paper, clear).top_0())
                .child(edge_fade(px(24.), clear, paper).bottom_0())
                .into_any_element();
        }

        self.step_jump_to_bottom(window);
        self.step_wheel(window);
        // 虚拟列表：只布局 / 绘制视口附近的行；最后一行是运行状态行（见 Session::sync_list）
        let view = self.settings.view_mode;
        self.sessions[active].sync_list(view);
        let state = self.sessions[active].list.clone();
        let scroll = list(
            state,
            cx.processor(|this, ix: usize, window, cx| {
                let s = &this.sessions[this.active];
                let n = s.transcript.len();
                let row = div().max_w(READING_W).mx_auto().w_full().px_6();
                // 过程组里的一步：展开 / 收起走弹簧，收起没走完之前照样画、按进度裁切
                // （每帧都要问一次进度，状态才留得住）
                let reveal = match s.layout.get(ix) {
                    Some(layout) if layout.member => {
                        motion::reveal_progress(("step", ix), layout.body, window, cx)
                    }
                    _ => 1.,
                };
                let content = match (s.transcript.get(ix), s.layout.get(ix)) {
                    // 收起的组内步骤、当前视图不显示的行：零高度（分组结果在测量指纹里，变了就重测）
                    (Some(_), Some(layout))
                        if !layout.body
                            && layout.head.is_none()
                            && (!layout.member || reveal <= 0.) =>
                    {
                        div()
                    }
                    (Some(item), Some(&layout)) => {
                        let body = this.render_row(ix, item, layout, reveal, window, cx);
                        // 刚从搜索结果 / 改动列表跳过来的那条：闪一下（见 search.rs）。
                        // diff 卡片自己有底色盖不住，改成描边；描边不框进卡片下面的行距
                        let card = matches!(item, ChatItem::Tool(c) if !c.diffs.is_empty())
                            .then(|| ChatApp::row_gap(layout));
                        let body =
                            this.with_flash(div().relative().child(body), ix, card, window, cx);
                        row.pt(px(if ix == 0 { 16. } else { 0. })).child(body)
                    }
                    (None, _) if ix == n => row.pb_6().children(this.render_turn_status()),
                    _ => row,
                };
                div().w_full().child(content).into_any_element()
            }),
        )
        .flex_1()
        .min_h_0();

        // 上下边缘渐隐：内容滚到顶栏 / 输入区下面时淡出，不是一刀切。高度不超过首行上边距和
        // 状态行下边距，滚到头时不遮内容；覆盖层不挂事件，不挡点击和滚轮
        let paper = rgb(PAPER);
        let clear = Rgba { a: 0., ..paper };
        // 滚轮平滑：capture 阶段截住滚轮记成目标（见 `ChatApp::wheel_by`），列表自己在 bubble 阶段
        // 一格硬跳 60px 的处理就不跑了。只有横向（Shift+滚轮）时放行。
        // 命中区不挡悬停；弹层、对话框盖在上面时 `should_handle_scroll` 为 false，不抢它们的滚轮
        let app = cx.entity().downgrade();
        let line = rems(1.25).to_pixels(window.rem_size());
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
                    let precise = event.delta.precise();
                    app.update(cx, |this, cx| this.wheel_by(-dy, precise, cx)).ok();
                    cx.stop_propagation();
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
            .child(edge_fade(px(16.), paper, clear).top_0())
            .child(edge_fade(px(24.), clear, paper).bottom_0())
            .children(self.render_jump_to_bottom(window, cx))
            .into_any_element()
    }
}

/// 滚动区上下边缘的渐隐条（从 `from` 渐变到 `to`），不挂事件，不挡点击和滚轮。
fn edge_fade(h: Pixels, from: Rgba, to: Rgba) -> Div {
    div().absolute().left_0().right_0().h(h).bg(linear_gradient(
        180.,
        linear_color_stop(from, 0.),
        linear_color_stop(to, 1.),
    ))
}

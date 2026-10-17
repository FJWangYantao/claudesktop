//! 侧栏：浮起的圆角面板，含导航、按工作目录分组的会话列表、设置与内核状态。
//!
//! 侧栏是独立的缓存 view（[`SidebarView`]）：对话区的转圈、流式输出每帧都在重画 `ChatApp`，
//! 侧栏几十行会话每行约 0.1ms，一起重画就是好几毫秒。缓存后只在侧栏用到的状态变了时重画
//! （`ChatApp::render` 每帧比一次指纹，见 [`ChatApp::sync_sidebar`]）。
//! 新加侧栏要显示的状态时，记得把它并进 `sync_sidebar` 的指纹，否则侧栏不会跟着变。
//!
//! 运行中那行的转圈不画在侧栏里（它每拍都要重画，会把整个侧栏一起带上——缓存 view 重画时
//! 里面嵌套的缓存 view 也不复用），而是行里只留个占位、记下位置，由 `ChatApp` 叠在上面画
//! （[`ChatApp::render_sidebar_spinners`]）。

use super::chrome::{CHROME_H, drag_area, traffic_light_gap};
use super::motion;
use super::widgets::Surface;
use super::{ChatApp, project_label};
use crate::settings::KernelChoice;
use crate::theme::rgb;
use crate::theme::*;
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::base::motion::{Transition, transition};
use gpui_kit::component::button::Button;
use gpui_kit::component::input::Input;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::cell::RefCell;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

/// 侧栏的缓存 view：内容全由 `ChatApp::render_sidebar` 画，这里只是把它隔成一块单独重画的区域。
pub(crate) struct SidebarView {
    app: WeakEntity<ChatApp>,
}

impl SidebarView {
    pub(super) fn new(app: WeakEntity<ChatApp>) -> Self {
        Self { app }
    }
}

impl Render for SidebarView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        match self.app.upgrade() {
            Some(app) => {
                app.update(cx, |app, cx| app.render_sidebar(window, cx)).into_any_element()
            }
            None => div().into_any_element(),
        }
    }
}

/// 侧栏里一个运行中会话的转圈该画在哪：占位格的位置和它所在的裁切范围（窗口坐标）。
#[derive(Clone, Copy, PartialEq)]
pub(super) struct SpinnerSpot {
    token: u64,
    bounds: Bounds<Pixels>,
    clip: Bounds<Pixels>,
}

/// 侧栏这次和上次重画时记下的转圈位置。侧栏复用缓存时不重画、也不重记，位置照旧有效。
#[derive(Default)]
pub(super) struct SpinnerSpots {
    now: Vec<SpinnerSpot>,
    before: Vec<SpinnerSpot>,
}

pub(super) type SharedSpots = Rc<RefCell<SpinnerSpots>>;

impl ChatApp {
    /// 侧栏的位置：宽度随开合在 180ms 里滑动（收到 0 后不画），里面是缓存的侧栏 view。
    pub(super) fn render_sidebar_slot(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let (duration, easing) = {
            let motion = cx.theme().motion_tokens();
            (motion.duration_normal, motion.easing_move.clone())
        };
        let target = if self.sidebar_open { SIDEBAR_W } else { px(0.) };
        let width = transition(
            "sidebar-width",
            target,
            Transition::new(duration).easing(easing),
            window,
            cx,
        );
        if width <= px(0.) {
            // 完全收起时侧栏不画，记下的转圈位置作废
            self.sidebar_spinners.borrow_mut().now.clear();
        }
        // 侧栏按全宽画、整块从左边滑进滑出（外层裁切），开合过程中它自己的尺寸不变
        (width > px(0.)).then(|| {
            div().w(width).h_full().flex_shrink_0().overflow_hidden().child(
                div().relative().left(width - SIDEBAR_W).h_full().child(
                    self.sidebar_view
                        .clone()
                        .cached(StyleRefinement::default().w(SIDEBAR_W).h_full()),
                ),
            )
        })
    }

    /// 比对侧栏的指纹，变了才通知它重画（每帧调一次，在画侧栏之前）。
    /// 指纹要盖住 `render_sidebar` / `render_session_row` 读到的全部状态。
    pub(super) fn sync_sidebar(&mut self, cx: &mut Context<Self>) {
        let mut h = DefaultHasher::new();
        // 外观、字号、字体变了全部重画
        (crate::theme::is_dark(), f32::from(root_font_size()).to_bits(), sans()).hash(&mut h);
        for (name, members) in self.session_groups() {
            name.hash(&mut h);
            for i in members {
                (i, self.sessions[i].token).hash(&mut h);
            }
        }
        (
            self.settings.sidebar_by_date,
            self.sessions_by_activity,
            self.settings.sidebar_kernel_status,
            self.settings_open,
            self.sidebar_open,
            self.renaming,
            self.confirm_delete,
            self.sidebar_hover,
            self.active,
            &self.cwd,
        )
            .hash(&mut h);
        for k in &self.kernels {
            (k.started, k.ready, k.error, &k.status).hash(&mut h);
        }
        for s in &self.sessions {
            let age = s.meta.as_ref().map(|m| age_label(m.updated_ms));
            (&s.title, s.busy, s.busy && s.waiting_permission(), age).hash(&mut h);
        }
        let key = h.finish();
        if key != self.sidebar_key {
            self.sidebar_key = key;
            self.sidebar_view.update(cx, |_, cx| cx.notify());
        }
    }

    /// 侧栏里运行中会话的转圈：叠在侧栏上、画在各行占位格记下的位置（见文件头）。
    /// 侧栏这一帧重画、行挪了位置时，占位格会要下一帧，这里跟着挪过去。
    pub(super) fn render_sidebar_spinners(&self) -> Vec<Div> {
        let spots = self.sidebar_spinners.borrow();
        spots
            .now
            .iter()
            .filter(|spot| {
                self.sessions
                    .iter()
                    .any(|s| s.token == spot.token && s.busy && !s.waiting_permission())
            })
            .map(|spot| {
                let (b, clip) = (spot.bounds, spot.clip);
                div()
                    .absolute()
                    .left(clip.origin.x)
                    .top(clip.origin.y)
                    .w(clip.size.width)
                    .h(clip.size.height)
                    .overflow_hidden()
                    .child(
                        div()
                            .absolute()
                            .left(b.origin.x - clip.origin.x)
                            .top(b.origin.y - clip.origin.y)
                            .w(b.size.width)
                            .h(b.size.height)
                            .flex()
                            .justify_center()
                            .items_center()
                            .child(motion::spinner()),
                    )
            })
            .collect()
    }

    /// 鼠标进出侧栏的一行会话（行右侧操作按钮的淡入淡出跟着它）。
    fn set_sidebar_hover(&mut self, token: u64, hovered: bool, cx: &mut Context<Self>) {
        let next = if hovered {
            Some(token)
        } else if self.sidebar_hover == Some(token) {
            None
        } else {
            self.sidebar_hover
        };
        if next != self.sidebar_hover {
            self.sidebar_hover = next;
            cx.notify();
        }
    }

    pub(super) fn render_sidebar(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        // 这一遍重新记转圈的位置（各行的占位格在 prepaint 时记）
        {
            let mut spots = self.sidebar_spinners.borrow_mut();
            spots.before = std::mem::take(&mut spots.now);
        }
        let mut body = v_flex().gap_0().pb_2();

        // 会话：按工作目录分组，当前项目在前，其余按最近活动排序（或按日期分组）。
        // 第一组的组名行右侧带分组方式 / 排序切换 / 在资源管理器中打开（悬停才显出来）
        for (g, (name, members)) in self.session_groups().into_iter().enumerate() {
            let header = div()
                .id(("group-header", g))
                .group("group-header")
                .h_flex()
                .items_center()
                .gap_1()
                .px_4()
                .pt(px(if g == 0 { 10. } else { 16. }))
                .pb_1()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_size(TEXT_META)
                        .text_color(rgb(INK_SOFT))
                        .child(name),
                );
            let header = if g == 0 {
                let sort_on = self.sessions_by_activity;
                let by_date = self.settings.sidebar_by_date;
                header
                    .child(
                        Self::sidebar_icon_button(
                            "group-by-date",
                            IconAll::CalendarDays,
                            if by_date { "按项目分组" } else { "按日期分组" },
                            by_date,
                            cx,
                        )
                        .when(!by_date, |d| {
                            d.opacity(0.).group_hover("group-header", |d| d.opacity(1.))
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.save_ui_pref(move |s| s.sidebar_by_date = !by_date, cx);
                            cx.notify();
                        })),
                    )
                    // 按日期分组时组内总是按最近活动排，排序切换没有意义
                    .when(!by_date, |d| {
                        d.child(
                            Self::sidebar_icon_button(
                                "sort-sessions",
                                IconAll::ArrowDownUp,
                                if sort_on {
                                    "按打开顺序排列"
                                } else {
                                    "按最近活动排列"
                                },
                                sort_on,
                                cx,
                            )
                            .when(!sort_on, |d| {
                                d.opacity(0.).group_hover("group-header", |d| d.opacity(1.))
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.sessions_by_activity = !this.sessions_by_activity;
                                cx.notify();
                            })),
                        )
                    })
                    .child(
                        Self::sidebar_icon_button(
                            "open-workspace",
                            IconAll::FolderOpen,
                            "在资源管理器中打开",
                            false,
                            cx,
                        )
                        .opacity(0.)
                        .group_hover("group-header", |d| d.opacity(1.))
                        .on_click({
                            let cwd = self.cwd.clone();
                            move |_, _, cx: &mut App| cx.reveal_path(&cwd)
                        }),
                    )
            } else {
                header
            };
            body = body.child(header);
            for i in members {
                body = body.child(self.render_session_row(i, window, cx));
            }
        }

        let nav_item = |id: &'static str, icon: IconAll, label: &'static str, active: bool| {
            div()
                .id(id)
                .h_flex()
                .items_center()
                .gap_2p5()
                .mx_2()
                .px_2()
                .py(px(5.))
                .rounded_md()
                .cursor_pointer()
                .when(active, |d| d.bg(rgb(SELECTED)))
                .hover(|d| d.bg(rgb(SELECTED)))
                .child(Icon::new(icon).size_4().text_color(rgb(INK_SOFT)))
                .child(div().text_size(TEXT_BODY).text_color(rgb(INK)).child(label))
        };

        let panel = v_flex()
            .size_full()
            .rounded_xl()
            .bg(rgb(SAND))
            .overflow_hidden()
            .child(
                // 头部：侧栏开关，其余是拖拽区（与会话区顶栏同高）
                div()
                    .h_flex()
                    .h(CHROME_H - px(8.))
                    .flex_shrink_0()
                    .items_center()
                    .pl_2()
                    .children(traffic_light_gap())
                    .child(self.sidebar_toggle("sidebar-toggle", cx))
                    .child(drag_area()),
            )
            .child(
                nav_item("new-chat", IconAll::Plus, "新会话", false)
                    .on_click(cx.listener(|this, _, _, cx| this.add_session(cx))),
            )
            .child(nav_item("plugins", IconAll::Blocks, "插件", false).on_click(cx.listener(
                |this, _, _, cx| {
                    this.toast("插件系统尚未开放；当前通过输入区的内核下拉切换 agent 能力", cx);
                },
            )))
            .child(div().id("sidebar-scroll").flex_1().min_h_0().overflow_y_scroll().child(body))
            .child(
                // 底部：设置 + 内核状态
                div()
                    .v_flex()
                    .gap_1()
                    .pb_3()
                    .child(
                        nav_item("open-settings", IconAll::Settings, "设置", self.settings_open)
                            .on_click(cx.listener(|this, _, window, cx| {
                                if this.settings_open {
                                    this.settings_open = false;
                                    cx.notify();
                                } else {
                                    this.open_settings(window, cx);
                                }
                            })),
                    )
                    .child(div().v_flex().gap_1().px_4().pt_1().children(
                        // 可在设置页关掉（「侧栏显示内核状态」）
                        if self.settings.sidebar_kernel_status {
                            self.kernel_status_rows()
                        } else {
                            Default::default()
                        },
                    )),
            );

        // 侧栏是浮在窗口底色上的圆角面板，四周留一圈窗口底色
        div().w(SIDEBAR_W).flex_shrink_0().h_full().pl_2().py_2().child(panel)
    }

    /// 侧栏分组标题上的小图标按钮（带提示）；`on` = 开关处于打开状态（图标高亮）。
    fn sidebar_icon_button(
        id: &'static str,
        icon: IconAll,
        tip: &'static str,
        on: bool,
        cx: &App,
    ) -> Button {
        Self::quiet_button(id, icon, Surface::Sand, cx)
            .xsmall()
            .tooltip(tip)
            .when(on, |b| b.icon(Icon::new(icon).text_color(rgb(CORAL))))
    }

    /// 每个已启动内核一行：状态点 + 「自研内核 · 已连接」
    fn kernel_status_rows(&self) -> Vec<Div> {
        KernelChoice::ALL
            .into_iter()
            .filter(|k| self.kernels[k.index()].started)
            .map(|kind| {
                let slot = &self.kernels[kind.index()];
                let dot = if slot.error {
                    RED
                } else if slot.ready {
                    GREEN
                } else {
                    CORAL
                };
                div()
                    .h_flex()
                    .gap_1p5()
                    .items_center()
                    .child(div().size(px(6.)).flex_shrink_0().rounded_full().bg(rgb(dot)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(TEXT_META)
                            .text_color(rgb(INK_SOFT))
                            .child(format!("{} · {}", kind.label(), slot.status)),
                    )
            })
            .collect()
    }

    /// 侧栏分组：(组名, 会话下标)。当前工作目录一组在最前；
    /// 组内默认按打开顺序，按活动排序时新会话在前、其余按最近更新。
    pub(super) fn session_groups(&self) -> Vec<(SharedString, Vec<usize>)> {
        // 无元信息的会话（刚开、还没落盘）视为最新
        let activity = |i: usize| self.sessions[i].meta.as_ref().map_or(u64::MAX, |m| m.updated_ms);
        if self.settings.sidebar_by_date {
            let now =
                SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
            let offset = crate::usage::local_offset_secs();
            let mut buckets: [Vec<usize>; 4] = Default::default();
            for i in 0..self.sessions.len() {
                buckets[crate::usage::day_bucket(activity(i).min(now), now, offset)].push(i);
            }
            return buckets
                .into_iter()
                .zip(crate::usage::DAY_BUCKETS)
                .filter(|(members, _)| !members.is_empty())
                .map(|(mut members, name)| {
                    members.sort_by_key(|i| std::cmp::Reverse(activity(*i)));
                    (name.into(), members)
                })
                .collect();
        }
        let mut groups: Vec<(&Path, Vec<usize>)> = vec![(self.cwd.as_path(), Vec::new())];
        for (i, s) in self.sessions.iter().enumerate() {
            match groups.iter_mut().find(|(cwd, _)| *cwd == s.cwd.as_path()) {
                Some((_, members)) => members.push(i),
                None => groups.push((s.cwd.as_path(), vec![i])),
            }
        }
        groups
            .into_iter()
            .enumerate()
            .map(|(g, (cwd, mut members))| {
                if self.sessions_by_activity {
                    members.sort_by_key(|i| std::cmp::Reverse(activity(*i)));
                }
                let name = if g == 0 { self.project_name.clone() } else { project_label(cwd) };
                (name, members)
            })
            .collect()
    }

    fn render_session_row(
        &self,
        i: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let s = &self.sessions[i];
        let active = self.active == i;
        let row = div()
            .id(("session", i))
            .relative()
            .h_flex()
            .items_center()
            .gap_2()
            .mx_2()
            .px_2()
            .py(px(5.))
            .rounded_md()
            .bg(if active { rgb(SELECTED) } else { rgba(CLEAR) });

        // 重命名中：标题换成输入框
        if self.renaming == Some(s.token) {
            return row.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(CORAL))
                    .bg(rgb(CARD))
                    .text_size(TEXT_BODY)
                    .child(
                        Input::new(&self.rename_input).appearance(false).bordered(false).small(),
                    ),
            );
        }
        // 删除确认
        if self.confirm_delete == Some(s.token) {
            let small_button = |id: &'static str, label: &'static str, danger: bool| {
                div()
                    .id((id, i))
                    .flex_shrink_0()
                    .px_2()
                    .py_0p5()
                    .rounded_md()
                    .cursor_pointer()
                    .text_size(TEXT_SMALL)
                    .text_color(rgb(if danger { WHITE } else { INK_SOFT }))
                    .bg(rgb(if danger { DANGER } else { CARD }))
                    .child(label)
            };
            return row
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_size(TEXT_SMALL)
                        .text_color(rgb(RED))
                        .child(format!("删除「{}」？", s.title)),
                )
                .child(
                    small_button("delete-yes", "删除", true)
                        .on_click(cx.listener(move |this, _, _, cx| this.delete_session(i, cx))),
                )
                .child(small_button("delete-no", "取消", false).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.confirm_delete = None;
                        cx.notify();
                    },
                )));
        }

        // 状态标记：等授权红点、运行中转圈，其余空心圈（与官方一样，每行都有个标记位）
        let marker = if s.busy && s.waiting_permission() {
            div().size(px(6.)).rounded_full().bg(rgb(RED)).into_any_element()
        } else if s.busy {
            // 转圈由 ChatApp 叠在这里画（见文件头）：这里只记位置，挪了位置就要下一帧让它跟上
            let (spots, token) = (self.sidebar_spinners.clone(), s.token);
            canvas(
                move |bounds, window, _| {
                    let spot = SpinnerSpot { token, bounds, clip: window.content_mask().bounds };
                    let mut spots = spots.borrow_mut();
                    if spots.before.get(spots.now.len()) != Some(&spot) {
                        window.request_animation_frame();
                    }
                    spots.now.push(spot);
                },
                |_, _, _, _| {},
            )
            .size(px(12.))
            .into_any_element()
        } else {
            div()
                .size(px(9.))
                .rounded_full()
                .border_1()
                .border_color(rgb(if active { INK_SOFT } else { INK_FAINT }))
                .into_any_element()
        };
        let age = s.meta.as_ref().map(|m| age_label(m.updated_ms));
        let token = s.token;
        let actions = motion::hover_opacity(
            ("row-actions", token as usize),
            self.sidebar_hover == Some(token),
            window,
            cx,
        );
        let icon_button = |id: &'static str, icon: IconAll| {
            div()
                .id((id, i))
                .p(px(3.))
                .rounded_md()
                .cursor_pointer()
                .hover(|d| d.bg(rgb(HOVER_STRONG)))
                .child(Icon::new(icon).size_3p5().text_color(rgb(INK_SOFT)))
        };
        row.cursor_pointer()
            .when(!active, |d| d.hover(|d| d.bg(rgb(SELECTED))))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                this.set_sidebar_hover(token, *hovered, cx)
            }))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.activate_session(i, cx);
            }))
            .child(
                div()
                    .size(px(14.))
                    .flex_shrink_0()
                    .flex()
                    .justify_center()
                    .items_center()
                    .child(marker),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(TEXT_BODY)
                    .text_color(rgb(INK))
                    .when(active, |d| d.font_weight(FontWeight::MEDIUM))
                    .child(s.title.clone()),
            )
            .when_some(age, |row, age| {
                row.child(
                    div().flex_shrink_0().text_size(TEXT_META).text_color(rgb(INK_SOFT)).child(age),
                )
            })
            // 悬停时盖在右侧标签上的操作按钮
            .child(
                div()
                    .absolute()
                    .right(px(4.))
                    .top_0()
                    .bottom_0()
                    .h_flex()
                    .items_center()
                    .gap_0p5()
                    .pl_2()
                    .rounded_md()
                    .bg(rgb(SELECTED))
                    .opacity(actions)
                    .child(icon_button("session-rename", IconAll::SquarePen).on_click(cx.listener(
                        move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.start_rename(i, window, cx);
                        },
                    )))
                    .child(icon_button("session-delete", IconAll::X).on_click(cx.listener(
                        move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.renaming = None;
                            this.confirm_delete = Some(this.sessions[i].token);
                            cx.notify();
                        },
                    ))),
            )
    }

    pub(crate) fn start_rename(&mut self, idx: usize, window: &mut Window, cx: &mut Context<Self>) {
        let s = &self.sessions[idx];
        self.renaming = Some(s.token);
        self.rename_focused = false;
        self.confirm_delete = None;
        let title = s.title.to_string();
        self.rename_input.update(cx, |state, cx| {
            state.set_value(title, window, cx);
            state.focus(window, cx);
            state.select_all(window, cx);
        });
        cx.notify();
    }

    /// 提交重命名：空标题不改；已落盘的会话同步写元信息。
    pub(super) fn commit_rename(&mut self, cx: &mut Context<Self>) {
        let Some(token) = self.renaming.take() else {
            return;
        };
        let text = self.rename_input.read(cx).value().trim().to_string();
        if let Some(s) = self.sessions.iter_mut().find(|s| s.token == token)
            && !text.is_empty()
            && text != s.title.as_ref()
        {
            s.title = text.clone().into();
            s.renamed = true;
            if let (Some(store), Some(meta)) = (&self.store, &mut s.meta) {
                meta.title = text;
                let _ = store.save_meta(meta);
            }
        }
        cx.notify();
    }

    /// 删除会话：停掉进行中的一轮，删日志和元信息，用量统计里也去掉。
    /// 删到一个不剩时补一个新会话。
    /// 调试钩子：让第 `idx` 个会话进入删除确认。
    pub(crate) fn ask_delete(&mut self, idx: usize, cx: &mut Context<Self>) {
        self.confirm_delete = self.sessions.get(idx).map(|s| s.token);
        cx.notify();
    }

    pub(crate) fn delete_session(&mut self, idx: usize, cx: &mut Context<Self>) {
        self.confirm_delete = None;
        if idx >= self.sessions.len() {
            return;
        }
        let kernel = self.session_kernel(idx);
        let s = &mut self.sessions[idx];
        s.cancel_pending_permissions(kernel.as_ref());
        s.cancel_token += 1;
        if let (true, Some(kernel), Some(id)) = (s.busy, &kernel, s.acp_id.clone()) {
            let _ = kernel.cancel(&id);
        }
        let removed = self.sessions.remove(idx);
        let id = removed.meta.as_ref().map(|m| m.id.clone());
        // 先关掉日志文件再删
        drop(removed);
        if let Some(id) = id {
            self.usage.retain(|t| t.session != id);
            if let Some(store) = &self.store
                && let Err(err) = store.delete(&id)
            {
                self.toast(Notification::error(format!("删除会话文件失败：{err}")), cx);
            }
        }
        if self.sessions.is_empty() {
            let cwd = self.cwd.clone();
            self.open_session(self.settings.kernel(), cwd);
            self.active = 0;
        } else if idx < self.active || self.active >= self.sessions.len() {
            self.active = self.active.saturating_sub(1);
        }
        cx.notify();
    }
}

/// 「刚刚 / 5 分钟 / 3 小时 / 2 天」
fn age_label(updated_ms: u64) -> SharedString {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
    let mins = now.saturating_sub(updated_ms) / 60_000;
    match mins {
        0 => "刚刚".into(),
        m if m < 60 => format!("{m} 分钟").into(),
        m if m < 60 * 24 => format!("{} 小时", m / 60).into(),
        m => format!("{} 天", m / (60 * 24)).into(),
    }
}

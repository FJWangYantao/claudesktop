//! 后台任务的界面：输入区顶盖的停靠条、顶栏的状态胶囊与它的弹层。
//!
//! 两条硬约束（见 `docs/dev/gpui-ui.md`）：
//! ① 转圈放在自己的小 view（[`BgSpinner`]）里：每帧只重画那 12px，不驱动 `ChatApp`
//!    和正文区重画；停靠条不在内容区里，不能用 `motion::spinner()`（它靠 `LoopOverlay` 画）。
//! ② 只有真的影响这一块时才 `refresh_composer`（只重画输入区），运行中的任务有无翻转才
//!    `sync_sidebar`；不要在这里无脑 `cx.notify()`。
//!
//! 数据来自内核的整表快照（`KernelEvent::BackgroundTasks`）。任务的秒数用本地锚点走
//! （内核只在有输出时才刷新快照）；任务结束后先停靠 4 秒，对话里另有永久回执。

use super::ChatApp;
use super::menu::{MenuChip, menu_panel};
use crate::theme::rgb;
use crate::theme::*;
use claudesktop_core::BackgroundTaskView;
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::popover::Popover;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::time::Instant;

/// 普通任务结束后在停靠条上留多久（对话里另有回执，这里只是让「刚结束」看得见）
const LINGER_SECS: u64 = 4;
/// 展开的尾日志最高多少（超出内部滚动，不顶出窗口）
const LOG_MAX_H: Pixels = px(120.);

/// 后台任务的界面状态：只在内存里，不进会话持久化。
pub(crate) struct BgTasksUi {
    /// 任务的本地锚点：`duration_secs` 只在有新输出时才刷新，跳秒靠本地钟
    pub(super) started: std::collections::HashMap<String, Instant>,
    /// 停靠条是否展开（默认收起成一行）
    pub(super) dock_open: bool,
    /// 停靠条里正在看尾日志的那一条（单选，省高度）
    pub(super) log_open: Option<String>,
    /// 刚结束、还在停靠条上留着的任务
    pub(super) linger: Vec<BgLinger>,
    /// 展开的尾日志的滚动位置（同一时间只有一条在画）
    pub(super) log_scroll: ScrollHandle,
}

impl Default for BgTasksUi {
    fn default() -> Self {
        Self {
            started: Default::default(),
            dock_open: false,
            log_open: None,
            linger: Default::default(),
            log_scroll: ScrollHandle::new(),
        }
    }
}

/// 刚结束、还在停靠条上停一会儿的任务。
pub(super) struct BgLinger {
    pub task: BackgroundTaskView,
    pub until: Instant,
}

/// 停靠条里那个 12px 的转圈：自己一个 view，每帧只重画自己。
pub(crate) struct BgSpinner;

impl Render for BgSpinner {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        super::motion::spinner_in_place()
    }
}

/// 顶栏胶囊的弹层内容：独立 entity，`ChatApp` 变了才跟着重画。
pub(super) struct BgTasksContent {
    app: Entity<ChatApp>,
    _app_observed: Subscription,
}

impl BgTasksContent {
    fn new(app: Entity<ChatApp>, cx: &mut Context<Self>) -> Self {
        let _app_observed = cx.observe(&app, |_, _, cx| cx.notify());
        Self { app, _app_observed }
    }
}

impl Render for BgTasksContent {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let handle = self.app.clone();
        let app = self.app.read(cx);
        let tasks = app.session_tasks();
        let log_open = app.bg.log_open.clone();
        let scroll = app.bg.log_scroll.clone();
        let rows: Vec<Div> = tasks
            .iter()
            .enumerate()
            .map(|(i, task)| {
                let open = log_open.as_deref() == Some(task.id.as_str());
                task_row(&handle, task, elapsed_secs(app, task), i, open, &scroll)
            })
            .collect();
        div().v_flex().gap_1().children(rows)
    }
}

impl ChatApp {
    /// 当前会话拉起的后台任务（快照顺序：新的在前）。
    pub(crate) fn session_tasks(&self) -> Vec<BackgroundTaskView> {
        let Some(session) = self.sessions.get(self.active) else { return Vec::new() };
        let id = session.acp_id.clone();
        let key = session.kernel.key();
        self.kernels
            .iter()
            .filter(|(slot_key, _)| *slot_key == key)
            .flat_map(|(_, slot)| slot.tasks.iter())
            .filter(|task| id.as_deref() == Some(task.session.as_str()))
            .cloned()
            .collect()
    }

    /// 侧栏用：这个会话（含子会话）还有没有在跑的后台命令。
    pub(crate) fn session_has_running_bg(&self, idx: usize) -> bool {
        let Some(session) = self.sessions.get(idx) else { return false };
        let id = session.acp_id.as_deref();
        let key = session.kernel.key();
        self.kernels.iter().filter(|(slot_key, _)| *slot_key == key).any(|(_, slot)| {
            slot.tasks
                .iter()
                .any(|task| task.running && id.is_some_and(|id| task.session.starts_with(id)))
        })
    }

    /// 收到新快照后更新本地状态；返回侧栏是否需要重画（有会话的运行中状态翻转）。
    pub(super) fn sync_background_ui(
        &mut self,
        old: &[BackgroundTaskView],
        new: &[BackgroundTaskView],
    ) -> bool {
        let now = Instant::now();
        for task in new.iter().filter(|task| task.running) {
            self.bg
                .started
                .entry(task.id.clone())
                .or_insert_with(|| now - std::time::Duration::from_secs(task.duration_secs));
        }
        // 快照里已经没有的任务不再记锚点
        self.bg.started.retain(|id, _| new.iter().any(|task| &task.id == id));
        // 从「在跑」变成「跑完了、也没续办」的：在停靠条上留一会儿
        for task in new {
            if task.running || task.ready_to_resume {
                continue;
            }
            let Some(was) = old.iter().find(|t| t.id == task.id) else { continue };
            if !was.running {
                continue;
            }
            self.bg.linger.retain(|l| l.task.id != task.id);
            self.bg.linger.push(BgLinger {
                task: task.clone(),
                until: now + std::time::Duration::from_secs(LINGER_SECS),
            });
        }
        if self.bg.linger.iter().all(|l| l.task.running) {
            self.bg.linger.clear();
        }
        // 侧栏只看「有没有在跑」：翻转了才重画
        running_by_session(old) != running_by_session(new)
    }

    /// 每秒一拍：跳秒 / 驻留到期。返回 true = 输入区要重画。
    pub(crate) fn tick_background(&mut self) -> bool {
        let now = Instant::now();
        let had = !self.bg.linger.is_empty();
        self.bg.linger.retain(|l| l.until > now);
        let expired = had && self.bg.linger.is_empty();
        expired || self.session_tasks().iter().any(|task| task.running)
    }

    /// 回执一到（对话里已经留痕），停靠条上的驻留就撤掉。
    pub(crate) fn drop_bg_linger(&mut self, id: &str) -> bool {
        let before = self.bg.linger.len();
        self.bg.linger.retain(|l| l.task.id != id);
        before != self.bg.linger.len()
    }

    /// 停靠条上的任务：运行中的在前，驻留的完整任务在后。
    fn dock_tasks(&self) -> Vec<BackgroundTaskView> {
        let mut tasks: Vec<BackgroundTaskView> =
            self.session_tasks().into_iter().filter(|task| task.running).collect();
        tasks.extend(
            self.bg.linger.iter().filter(|l| l.until > Instant::now()).map(|l| l.task.clone()),
        );
        tasks
    }

    /// 输入区顶盖的后台任务停靠条（和 `render_context_bar` 同一个槽位）。
    pub(super) fn render_bg_dock(&self, cx: &mut Context<Self>) -> Option<Div> {
        let tasks = self.dock_tasks();
        let first = tasks.first()?.clone();
        let running = tasks.iter().filter(|task| task.running).count();
        let open = self.bg.dock_open;
        let handle = cx.entity();
        let elapsed = elapsed_secs(self, &first);
        let followup = first.followup.clone();
        // 收起态那一句：单任务 / 只剩驻留都给命令，多任务给条数 + 第一条命令
        let headline: SharedString = if running > 1 {
            format!("{running} 个后台任务 · {}", first_line(&first.command)).into()
        } else {
            first_line(&first.command).into()
        };
        let (icon, color) = if running > 0 {
            (IconAll::SquareTerminal, CORAL)
        } else if first.exit_code == Some(0) {
            (IconAll::Check, GREEN)
        } else {
            (IconAll::X, RED)
        };

        let head = div()
            .h_flex()
            .items_center()
            .gap_1p5()
            .min_w_0()
            .px_2p5()
            .py_1()
            .when(running > 0, |d| d.child(self.bg_spinner.clone()))
            .when(running == 0, |d| d.child(Icon::new(icon).size_3().text_color(rgb(color))))
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .font_family(MONO)
                    .text_size(TEXT_SMALL)
                    .text_color(rgb(INK))
                    .child(headline),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(TEXT_META)
                    .text_color(rgb(INK_FAINT))
                    .font_features(tabular_nums())
                    .child(secs_label(elapsed)),
            )
            .when_some(followup, |d, text| {
                d.child(followup_chip(("bg-followup-head", 0usize), &text))
            })
            .child(div().flex_1())
            .child(
                small_chip(
                    ("bg-dock-toggle", self.session().token as usize),
                    if open { "收起" } else { "展开" },
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.bg.dock_open = !this.bg.dock_open;
                    if !this.bg.dock_open {
                        this.bg.log_open = None;
                    }
                    this.refresh_composer(cx);
                })),
            )
            .when(running == 1, |d| {
                let id = first.id.clone();
                d.child(stop_action(("bg-dock-stop", 0usize), &id, &handle))
            });

        let log_open = self.bg.log_open.clone();
        let scroll = self.bg.log_scroll.clone();
        let body = div().v_flex().gap_1().px_2p5().pb_2().children(tasks.iter().enumerate().map(
            |(i, task)| {
                let is_open = log_open.as_deref() == Some(task.id.as_str());
                task_row(&handle, task, elapsed_secs(self, task), i, is_open, &scroll)
            },
        ));

        Some(
            div()
                .v_flex()
                .w_full()
                .mx(COMPOSER_RADIUS)
                // 由后画的输入卡片盖住这 1px，上沿只留中间一条接缝（同 `render_context_bar`）
                .mb(px(-1.))
                .rounded_tl(COMPOSER_INNER_RADIUS)
                .rounded_tr(COMPOSER_INNER_RADIUS)
                .bg(rgb(SIDEBAR))
                .border_t_1()
                .border_l_1()
                .border_r_1()
                .border_color(rgb(LINE))
                .child(head)
                .when(open, |d| d.child(body)),
        )
    }

    /// 顶栏的状态胶囊：个数 + 点开的任务清单（和停靠条同时出现）。
    pub(super) fn render_bg_tasks_pill(&self, cx: &mut Context<Self>) -> Option<Popover> {
        let running = self.session_tasks().iter().filter(|task| task.running).count();
        if running == 0 {
            return None;
        }
        let app = cx.entity();
        // 胶囊：圆角小 chip，打开时换成沙色底（和输入框工具栏的 chip 同一套）
        let trigger = MenuChip::new(IconAll::SquareTerminal, format!("{running} 个后台"))
            .id("bg-tasks")
            .pill();
        Some(
            menu_panel(Popover::new("bg-tasks-menu"), &self.menu_focus)
                .anchor(Anchor::TopRight)
                .w(rems(22.))
                .p_2()
                .trigger(trigger)
                .content(move |_, window, cx| {
                    let content = window.use_keyed_state("bg-tasks-content", cx, |_, cx| {
                        cx.new(|cx| BgTasksContent::new(app.clone(), cx))
                    });
                    content.read(cx).clone()
                }),
        )
    }
}

/// 一条任务的行（停靠条和顶栏弹层共用）：命令 + 秒数 + 续办 / 停止 + 尾日志。
fn task_row(
    app: &Entity<ChatApp>,
    task: &BackgroundTaskView,
    elapsed: u64,
    index: usize,
    log_open: bool,
    scroll: &ScrollHandle,
) -> Div {
    let id = task.id.clone();
    let mut row =
        div().v_flex().gap_1().rounded_md().px_1().py_1().hover(|d| d.bg(rgb(HOVER))).child(
            div()
                .h_flex()
                .items_center()
                .gap_1p5()
                .min_w_0()
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .font_family(MONO)
                        .text_size(TEXT_SMALL)
                        .text_color(rgb(INK))
                        .child(super::chat::first_line(&task.command)),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text_size(TEXT_META)
                        .text_color(rgb(INK_FAINT))
                        .font_features(tabular_nums())
                        .child(secs_label(elapsed)),
                )
                .child(div().flex_1())
                .when_some(task.followup.clone(), |d, text| {
                    d.child(followup_chip(("bg-followup", index), &text))
                })
                .child(log_action(app, &task.id, index, log_open))
                .when(task.running, |d| d.child(stop_action(("bg-stop", index), &id, app)))
                .when(!task.running && task.followup.is_some(), |d| {
                    let id = task.id.clone();
                    let app = app.clone();
                    d.child(small_chip(("bg-cancel", index), "取消续办").on_click(
                        move |_, _, cx| {
                            let id = id.clone();
                            app.update(cx, |app, cx| app.cancel_background_followup(&id, cx));
                        },
                    ))
                }),
        );
    if !task.running {
        row = row.child(div().text_size(TEXT_META).text_color(rgb(INK_FAINT)).child(
            match (task.exit_code, task.ready_to_resume) {
                (Some(0), _) => "已结束 · 退出码 0".to_string(),
                (Some(code), _) => format!("已结束 · 退出码 {code}"),
                (None, true) => "已结束 · 自动续办中".to_string(),
                (None, false) => "已结束".to_string(),
            },
        ));
    }
    if log_open {
        row = row.child(log_panel(app, task, scroll));
    }
    row
}

fn log_panel(
    app: &Entity<ChatApp>,
    task: &BackgroundTaskView,
    scroll: &ScrollHandle,
) -> Stateful<Div> {
    let text: SharedString = if task.output_tail.trim().is_empty() {
        "（还没有输出）".into()
    } else {
        task.output_tail.clone().into()
    };
    let _ = app;
    div()
        .id(("bg-log-panel", 0usize))
        .w_full()
        .max_h(LOG_MAX_H)
        .overflow_y_scroll()
        .track_scroll(scroll)
        .rounded_md()
        .bg(rgb(CODE_BG))
        .p_2()
        .font_family(MONO)
        .text_size(TEXT_META)
        .text_color(rgb(INK_SOFT))
        .child(text)
}

/// 秒数的中文文案（「12 秒」/「1 分 5 秒」），和对话里的回执一致。
fn secs_label(secs: u64) -> String {
    let secs = secs.max(1);
    match (secs / 60, secs % 60) {
        (0, s) => format!("{s} 秒"),
        (m, 0) => format!("{m} 分钟"),
        (m, s) => format!("{m} 分 {s} 秒"),
    }
}

/// 任务的秒数：运行中按本地锚点走，不跑的一律用内核给的定值。
fn elapsed_secs(app: &ChatApp, task: &BackgroundTaskView) -> u64 {
    match app.bg.started.get(&task.id) {
        Some(at) if task.running => at.elapsed().as_secs(),
        _ => task.duration_secs,
    }
}

/// 会话 → 有没有在跑的后台任务（侧栏重画的判据）。
fn running_by_session(tasks: &[BackgroundTaskView]) -> std::collections::BTreeSet<&str> {
    tasks.iter().filter(|task| task.running).map(|task| task.session.as_str()).collect()
}

fn first_line(text: &str) -> String {
    super::chat::first_line(text).to_string()
}

/// 一片沙色小胶囊（「展开 / 收起」「取消续办」这类轻动作）。
fn small_chip(id: impl Into<ElementId>, label: &str) -> Stateful<Div> {
    div()
        .id(id)
        .flex_shrink_0()
        .px_1p5()
        .py(px(2.))
        .rounded_md()
        .cursor_pointer()
        .text_size(TEXT_META)
        .text_color(rgb(INK_SOFT))
        .hover(|d| d.bg(rgb(HOVER_STRONG)))
        .child(label.to_string())
}

/// 「自动续办」微标：说明这条任务跑完会把模型叫回来。
fn followup_chip(id: impl Into<ElementId>, instruction: &str) -> Stateful<Div> {
    let tip = instruction.to_string();
    div()
        .id(id)
        .flex_shrink_0()
        .px_1p5()
        .py(px(2.))
        .rounded_md()
        .bg(rgb(ACCENT_SOFT))
        .text_size(TEXT_META)
        .text_color(rgb(CORAL))
        .tooltip(move |window, cx| super::chat::text_tip(&tip, window, cx))
        .child("自动续办")
}

/// 停止按钮（透明底、悬停出底色，不描边）。
fn stop_action(id: impl Into<ElementId>, target: &str, app: &Entity<ChatApp>) -> Stateful<Div> {
    let target = target.to_string();
    let app = app.clone();
    div()
        .id(id)
        .flex_shrink_0()
        .p(px(2.))
        .rounded_md()
        .cursor_pointer()
        .hover(|d| d.bg(rgb(HOVER_STRONG)))
        .tooltip(|window, cx| Tooltip::new("停止这条后台命令").build(window, cx))
        .child(Icon::new(IconAll::X).size_3().text_color(rgb(RED)))
        .on_click(move |_, _, cx| {
            let target = target.clone();
            app.update(cx, |app, cx| app.stop_background(&target, cx));
        })
}

/// 展开 / 收起尾日志。
fn log_action(app: &Entity<ChatApp>, target: &str, index: usize, open: bool) -> Stateful<Div> {
    let target = target.to_string();
    let app = app.clone();
    div()
        .id(("bg-log-toggle", index))
        .flex_shrink_0()
        .p(px(2.))
        .rounded_md()
        .cursor_pointer()
        .hover(|d| d.bg(rgb(HOVER_STRONG)))
        .tooltip(move |window, cx| {
            Tooltip::new(if open { "收起输出" } else { "查看输出" }).build(window, cx)
        })
        .child(
            Icon::new(if open { IconAll::ChevronDown } else { IconAll::ChevronRight })
                .size_3()
                .text_color(rgb(INK_SOFT)),
        )
        .on_click(move |_, _, cx| {
            let target = target.clone();
            app.update(cx, |app, cx| {
                app.bg.log_open = if app.bg.log_open.as_deref() == Some(target.as_str()) {
                    None
                } else {
                    Some(target.clone())
                };
                app.refresh_composer(cx);
            });
        })
}

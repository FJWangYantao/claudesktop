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
/// 停靠条的行 / 弹层的行用不同前缀：两处同帧出现时元素 id 不能撞
const DOCK_SCOPE: &str = "bgd";
const POP_SCOPE: &str = "bgp";

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

impl BgTasksUi {
    /// 初始状态：正常运行是收起；截图钩子（`CLAUDESKTOP_BG_PREVIEW`）下按模式预先展开。
    pub(super) fn initial() -> Self {
        #[allow(unused_mut)]
        let mut ui = Self::default();
        #[cfg(feature = "headless-screenshot")]
        if let Some(mode) = preview() {
            ui.dock_open = mode != "dock";
            ui.log_open = (mode == "dock-open").then(|| "bg-3".to_string());
        }
        ui
    }
}

/// 截图钩子：`CLAUDESKTOP_BG_PREVIEW = dock | dock-open | menu` 时用几条假任务渲染停靠条
/// 和顶栏弹层（只在 `--features headless-screenshot` 的构建里存在）。
#[cfg(feature = "headless-screenshot")]
pub(super) fn preview() -> Option<&'static str> {
    match std::env::var("CLAUDESKTOP_BG_PREVIEW").ok().as_deref() {
        Some("dock") => Some("dock"),
        Some("dock-open") => Some("dock-open"),
        Some("menu") => Some("menu"),
        _ => None,
    }
}

/// 截图钩子用的假任务：一条挂了续办、一条纯后台、一条刚结束。
#[cfg(feature = "headless-screenshot")]
fn preview_tasks() -> Vec<BackgroundTaskView> {
    let build_tail = "   Compiling claudesktop-core v0.19.1\n   Compiling claudesktop-builtin v0.19.1\n   Compiling claudesktop v0.19.1\n    Finished `dev` profile in 41.62s\n\ntest result: ok. 312 passed; 0 failed; 0 ignored\nrunning 225 tests\n   Doc-tests claudesktop-core";
    vec![
        BackgroundTaskView {
            id: "bg-3".into(),
            session: "preview".into(),
            command: "cargo test --workspace".into(),
            running: true,
            ready_to_resume: false,
            exit_code: None,
            duration_secs: 96,
            output_tail: build_tail.into(),
            followup: Some("跑完把失败的用例整理出来".into()),
        },
        BackgroundTaskView {
            id: "bg-2".into(),
            session: "preview".into(),
            command: "npm run dev".into(),
            running: true,
            ready_to_resume: false,
            exit_code: None,
            duration_secs: 245,
            output_tail: "VITE v5.4.2  ready in 412 ms\n\n  ➜  Local:   http://localhost:5173/\n  ➜  press h + enter to show help".into(),
            followup: None,
        },
    ]
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

/// 顶栏胶囊的弹层内容：独立 entity，`ChatApp` 或输入框变了才跟着重画。
pub(super) struct BgTasksContent {
    app: Entity<ChatApp>,
    /// 弹层自己的尾日志滚动位置（和停靠条那个不是同一个元素，别共用句柄）
    log_scroll: ScrollHandle,
    _app_observed: Subscription,
    /// 秒数和尾日志更新走 `refresh_composer`（只通知输入框），弹层也得跟着刷
    _input_observed: Subscription,
}

impl BgTasksContent {
    fn new(app: Entity<ChatApp>, cx: &mut Context<Self>) -> Self {
        let input = app.read(cx).input.clone();
        let _app_observed = cx.observe(&app, |_, _, cx| cx.notify());
        let _input_observed = cx.observe(&input, |_, _, cx| cx.notify());
        Self { app, log_scroll: ScrollHandle::new(), _app_observed, _input_observed }
    }
}

impl Render for BgTasksContent {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let handle = self.app.clone();
        let scroll = self.log_scroll.clone();
        let app = self.app.read(cx);
        let tasks = app.session_tasks();
        let log_open = app.bg.log_open.clone();
        let rows: Vec<Div> = tasks
            .iter()
            .enumerate()
            .map(|(i, task)| {
                let open = log_open.as_deref() == Some(task.id.as_str());
                task_row(&handle, task, elapsed_secs(app, task), i, open, &scroll, POP_SCOPE)
            })
            .collect();
        div().v_flex().gap_1().children(rows)
    }
}

impl ChatApp {
    /// 当前会话拉起的后台任务（快照顺序：新的在前）。
    pub(crate) fn session_tasks(&self) -> Vec<BackgroundTaskView> {
        // 截图钩子：给停靠条 / 弹层塞几条假任务，别的路径还是读内核快照
        #[cfg(feature = "headless-screenshot")]
        {
            if preview().is_some() {
                return preview_tasks();
            }
        }
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
        let Some(id) = session.acp_id.as_deref() else { return false };
        let key = session.kernel.key();
        self.kernels.iter().filter(|(slot_key, _)| *slot_key == key).any(|(_, slot)| {
            slot.tasks.iter().any(|task| {
                // 子会话用 `父/子` 的写法认（和 `bgtasks::has_running` 一致），
                // 不能只 `starts_with`：`…-10` 会被当成 `…-1` 的子会话
                task.running
                    && (task.session == id
                        || task.session.strip_prefix(id).is_some_and(|rest| rest.starts_with('/')))
            })
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
        // 侧栏只看「有没有在跑」：翻转了才重画
        running_by_session(old) != running_by_session(new)
    }

    /// 每秒一拍：跳秒 / 驻留到期。返回 true = 输入区要重画。
    pub(crate) fn tick_background(&mut self) -> bool {
        let now = Instant::now();
        let before = self.bg.linger.len();
        self.bg.linger.retain(|l| l.until > now);
        // 只要真收掉了就得重画（几条驻留先后到期，不能等全空）
        let expired = self.bg.linger.len() != before;
        expired || self.session_tasks().iter().any(|task| task.running)
    }

    /// 回执一到（对话里已经留痕），停靠条上的驻留就撤掉。
    pub(crate) fn drop_bg_linger(&mut self, id: &str) -> bool {
        let before = self.bg.linger.len();
        self.bg.linger.retain(|l| l.task.id != id);
        before != self.bg.linger.len()
    }

    /// 停靠条上的任务：当前会话运行中的在前，本会话刚结束正在驻留的在后
    /// （别的会话留下的驻留不能串到这一条上来）。
    fn dock_tasks(&self) -> Vec<BackgroundTaskView> {
        let session = self.sessions.get(self.active).and_then(|s| s.acp_id.clone());
        let mut tasks: Vec<BackgroundTaskView> =
            self.session_tasks().into_iter().filter(|task| task.running).collect();
        let now = Instant::now();
        tasks.extend(
            self.bg
                .linger
                .iter()
                .filter(|l| l.until > now && session.as_deref() == Some(l.task.session.as_str()))
                .map(|l| l.task.clone()),
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
        let toggle = row_id(DOCK_SCOPE, "toggle", self.session().token as usize);

        // 展开态：标题行只说有几条在跑，具体命令和按钮都在下面的任务行里
        // （收起态那一行整块换掉，不重复画同一条任务的元素 id）
        let head = if open {
            let title: SharedString = if running > 0 {
                format!("后台任务 · {running} 运行中").into()
            } else {
                "后台任务 · 已结束".into()
            };
            div()
                .h_flex()
                .items_center()
                .gap_1p5()
                .min_w_0()
                .px_2p5()
                .py_1()
                .child(div().text_size(TEXT_META).text_color(rgb(INK_SOFT)).child(title))
                .child(div().flex_1())
                .child(
                    small_chip(toggle, "收起")
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_bg_dock(cx))),
                )
        } else {
            let elapsed = elapsed_secs(self, &first);
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
            div()
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
                        .text_color(rgb(INK_SOFT))
                        .font_features(tabular_nums())
                        .child(secs_label(elapsed)),
                )
                .when_some(first.followup.clone(), |d, text| {
                    d.child(followup_chip(row_id(DOCK_SCOPE, "head-followup", 0), &text))
                })
                .child(div().flex_1())
                .child(
                    small_chip(toggle, "展开")
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_bg_dock(cx))),
                )
                .when(running == 1, |d| {
                    let id = first.id.clone();
                    d.child(stop_action(row_id(DOCK_SCOPE, "head-stop", 0), &id, &handle))
                })
        };

        let log_open = self.bg.log_open.clone();
        let scroll = self.bg.log_scroll.clone();
        let body = div().v_flex().gap_1().px_2p5().pb_2().children(tasks.iter().enumerate().map(
            |(i, task)| {
                let is_open = log_open.as_deref() == Some(task.id.as_str());
                task_row(&handle, task, elapsed_secs(self, task), i, is_open, &scroll, DOCK_SCOPE)
            },
        ));

        Some(
            div()
                .v_flex()
                // 和 `render_context_bar` 一样只写左右外边距：再叠 `w_full()` 会向右多凸一圈
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

    /// 收起 / 展开停靠条（收起时顺手清掉正在看的尾日志）。
    fn toggle_bg_dock(&mut self, cx: &mut Context<Self>) {
        self.bg.dock_open = !self.bg.dock_open;
        if !self.bg.dock_open {
            self.bg.log_open = None;
        }
        self.refresh_composer(cx);
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
                .default_open(preview_menu_open())
                .content(move |_, window, cx| {
                    let content = window.use_keyed_state("bg-tasks-content", cx, |_, cx| {
                        cx.new(|cx| BgTasksContent::new(app.clone(), cx))
                    });
                    content.read(cx).clone()
                }),
        )
    }
}

/// 截图钩子：这个构建要不要默认展开顶栏弹层（正常运行恒为 false）。
fn preview_menu_open() -> bool {
    #[cfg(feature = "headless-screenshot")]
    {
        preview() == Some("menu")
    }
    #[cfg(not(feature = "headless-screenshot"))]
    {
        false
    }
}

/// 行内元素 id：停靠条和顶栏弹层同帧出现时不能撞（`scope` 区分两处）。
fn row_id(scope: &str, kind: &str, index: usize) -> ElementId {
    ElementId::Name(format!("bg-{scope}-{kind}-{index}").into())
}

/// 一条任务的行（停靠条和顶栏弹层共用）：命令 + 秒数 + 续办 / 停止 + 尾日志。
fn task_row(
    app: &Entity<ChatApp>,
    task: &BackgroundTaskView,
    elapsed: u64,
    index: usize,
    log_open: bool,
    scroll: &ScrollHandle,
    scope: &'static str,
) -> Div {
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
                        .text_color(rgb(INK_SOFT))
                        .font_features(tabular_nums())
                        .child(secs_label(elapsed)),
                )
                .child(div().flex_1())
                .when_some(task.followup.clone(), |d, text| {
                    d.child(followup_chip(row_id(scope, "followup", index), &text))
                })
                .child(log_action(app, &task.id, row_id(scope, "log", index), log_open))
                // 跑着的时候更要能撤续办（进程照跑）；「停止」只给在跑的
                .when(task.followup.is_some(), |d| {
                    let id = task.id.clone();
                    let app = app.clone();
                    d.child(small_chip(row_id(scope, "cancel", index), "取消续办").on_click(
                        move |_, _, cx| {
                            let id = id.clone();
                            app.update(cx, |app, cx| app.cancel_background_followup(&id, cx));
                        },
                    ))
                })
                .when(task.running, |d| {
                    let id = task.id.clone();
                    d.child(stop_action(row_id(scope, "stop", index), &id, app))
                }),
        );
    if !task.running {
        let (label, color) = match (task.exit_code, task.ready_to_resume) {
            (Some(0), _) => ("已结束 · 退出码 0".to_string(), INK_SOFT),
            (Some(code), _) => (format!("已结束 · 退出码 {code}"), RED),
            (None, true) => ("已结束 · 自动续办中".to_string(), INK_SOFT),
            (None, false) => ("已结束".to_string(), INK_SOFT),
        };
        row = row.child(div().text_size(TEXT_META).text_color(rgb(color)).child(label));
    }
    if log_open {
        row = row.child(log_panel(task, row_id(scope, "log-panel", index), scroll));
    }
    row
}

fn log_panel(task: &BackgroundTaskView, id: ElementId, scroll: &ScrollHandle) -> Stateful<Div> {
    let text: SharedString = if task.output_tail.trim().is_empty() {
        "（还没有输出）".into()
    } else {
        task.output_tail.clone().into()
    };
    div()
        .id(id)
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
fn log_action(app: &Entity<ChatApp>, target: &str, id: ElementId, open: bool) -> Stateful<Div> {
    let target = target.to_string();
    let app = app.clone();
    div()
        .id(id)
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

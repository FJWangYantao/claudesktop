//! 侧栏：贴满窗口左缘的中性灰面板，含导航、对话历史、设置与内核状态；宽度可拖拽调整。
//! 对话历史默认分两区（「设置 · 通用 · 侧栏 · 会话管理」选**按项目**）：上面是**项目区**
//! （按工作目录分组，当前项目在前，登记过的空项目也常驻），下面是**任务区**
//! （[`TASKS_KEY`]：建立时没为它选过文件夹的对话，见 [`Session::task`]），最后是「已归档」组。
//! 选**按时间**（以及聊天空间）时不再拆任务区，项目和任务的会话一起进
//! 今天 / 昨天 / 最近 7 天 / 更早，末尾仍是「已归档」。
//! 会话列表可按状态筛选（[`SidebarFilter`]，第一组组名旁的筛选按钮），筛选时不分组。
//! 会话多了不占地方：点组名折叠整组（记进设置），每组默认只列最近活动的几个
//! （[`visible_members`]），当前 / 运行中 / 待查看的会话折叠了也照常露出来。
//! 用不着又不想删的会话可以归档：收进列表末尾的「已归档」组（默认收起，元信息里记 `archived`）。
//!
//! 侧栏是独立的缓存 view（[`SidebarView`]）：对话区的转圈、流式输出每帧都在重画 `ChatApp`，
//! 侧栏几十行会话每行约 0.1ms，一起重画就是好几毫秒。缓存后只在侧栏用到的状态变了时重画
//! （`ChatApp::render` 每帧比一次指纹，见 [`ChatApp::sync_sidebar`]）。
//! 新加侧栏要显示的状态时，记得把它并进 `sync_sidebar` 的指纹，否则侧栏不会跟着变。
//!
//! 运行中那行的闪烁圆点不画在侧栏里（它每拍都要重画，会把整个侧栏一起带上——缓存 view 重画时
//! 里面嵌套的缓存 view 也不复用），而是行里只留个占位、记下窗口坐标，由叠在上面的小 view
//! [`SidebarSpinners`] 在 `canvas` 里直接按窗口坐标画（避免受顶栏 `CHROME_H` 偏移）。
//! 它不画在 `ChatApp` 里：节拍器通知 `ChatApp` 会连内容区一起重画（见 content.rs）。

use super::chrome::{CHROME_H, drag_area};
use super::menu::{MenuList, menu_item, menu_panel};
use super::motion;
use super::widgets::Surface;
use super::{ChatApp, project_label};
use crate::session::scroll_handle_edge;
use crate::settings::{KernelChoice, SidebarGrouping, Space, same_project_path};
use crate::theme::rgb;
use crate::theme::*;
use claudesktop_core::SessionKind;
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::base::motion::{Transition, transition};
use gpui_kit::component::button::Button;
use gpui_kit::component::input::Input;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::popover::Popover;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::cell::RefCell;
use std::collections::HashSet;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

/// 侧栏的一组会话（同一工作目录，或按日期分组时的同一天）。
pub(crate) struct SessionGroup {
    /// 记折叠 / 展开用：`dir:<工作目录>` 或 `date:<日期组名>`
    pub key: String,
    pub name: SharedString,
    /// 项目组对应的目录；任务 / 日期 / 已归档组为 None
    pub dir: Option<PathBuf>,
    pub members: Vec<usize>,
}

/// 「已归档」组的 [`SessionGroup::key`]
pub(crate) const ARCHIVED_KEY: &str = "archived";

/// 「任务」组的 [`SessionGroup::key`]（对话历史的下半区：建立时没选过文件夹的对话）
pub(crate) const TASKS_KEY: &str = "tasks";

/// 两个折叠键是不是同一组：`dir:` 按项目路径归一比较（Windows 下大小写、斜杠来源不一），
/// 日期 / 任务 / 已归档这类键精确匹配。
fn same_group_key(a: &str, b: &str) -> bool {
    match (a.strip_prefix("dir:"), b.strip_prefix("dir:")) {
        (Some(x), Some(y)) => same_project_path(Path::new(x), Path::new(y)),
        _ => a == b,
    }
}

/// 一组默认列出几个会话，其余收进「显示其余 N 个」
const GROUP_PREVIEW: usize = 5;

/// 一组在侧栏里列出哪些会话、藏起来几个，列出的保持组内原来的顺序。
/// 折叠时只列要紧的（`keep`：当前、运行中、待查看）；没点「显示全部」时列最近活动的
/// [`GROUP_PREVIEW`] 个加上要紧的。只多出一两个时不值得收，全列。
fn visible_members(
    members: &[usize],
    folded: bool,
    show_all: bool,
    keep: impl Fn(usize) -> bool,
    activity: impl Fn(usize) -> u64,
) -> (Vec<usize>, usize) {
    if !folded && (show_all || members.len() <= GROUP_PREVIEW + 2) {
        return (members.to_vec(), 0);
    }
    let recent: HashSet<usize> = if folded {
        HashSet::new()
    } else {
        let mut by_activity = members.to_vec();
        by_activity.sort_by_key(|&i| std::cmp::Reverse(activity(i)));
        by_activity.into_iter().take(GROUP_PREVIEW).collect()
    };
    let shown: Vec<usize> =
        members.iter().copied().filter(|&i| recent.contains(&i) || keep(i)).collect();
    let hidden = members.len() - shown.len();
    (shown, hidden)
}

/// 侧栏只列哪些会话。不写进设置：筛选是一时的（看一眼谁在跑、谁有结果），重启回到全部。
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash, Debug)]
pub(crate) enum SidebarFilter {
    #[default]
    All,
    /// 正在跑或等授权
    Running,
    /// 等授权，或这一轮在不在眼前时结束了、还没切过去看（`Session::unread`）
    Unread,
}

impl SidebarFilter {
    const ALL: [Self; 3] = [Self::All, Self::Running, Self::Unread];

    fn label(self) -> &'static str {
        match self {
            Self::All => "全部会话",
            Self::Running => "进行中",
            Self::Unread => "待查看",
        }
    }

    fn desc(self) -> &'static str {
        match self {
            Self::All => "显示全部会话",
            Self::Running => "正在运行或等你授权",
            Self::Unread => "等你授权或有新结果",
        }
    }

    fn icon(self) -> IconAll {
        match self {
            Self::All => IconAll::MessageSquare,
            Self::Running => IconAll::Loader,
            Self::Unread => IconAll::BellDot,
        }
    }

    /// 筛完一个不剩时列表里的提示
    fn empty_hint(self) -> &'static str {
        match self {
            Self::All => "",
            Self::Running => "没有正在运行的会话",
            Self::Unread => "没有待查看的会话",
        }
    }
}

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

/// 侧栏运行中指示器的覆盖层：铺满内容区容器、不挂事件，按 [`SpinnerSpots`] 记下的窗口坐标画闪烁圆点。
pub(crate) struct SidebarSpinners {
    app: WeakEntity<ChatApp>,
}

impl SidebarSpinners {
    pub(super) fn new(app: WeakEntity<ChatApp>) -> Self {
        Self { app }
    }
}

impl Render for SidebarSpinners {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let app = self.app.clone();
        canvas(
            move |_, window, cx| {
                let spots = app
                    .upgrade()
                    .map(|app| app.read(cx).sidebar_spinner_spots())
                    .unwrap_or_default();
                if !spots.is_empty() {
                    motion::request_loop_tick(window);
                }
                spots
            },
            |_, spots, window, _| {
                if spots.is_empty() {
                    return;
                }
                let phase = motion::pulse_dot_phase();
                let color = rgb(CORAL);
                for (bounds, clip) in spots {
                    window.with_content_mask(Some(ContentMask { bounds: clip }), |window| {
                        motion::paint_pulse_dot(bounds, phase, color, window);
                    });
                }
            },
        )
        .absolute()
        .size_full()
    }
}

/// 侧栏里一个运行中会话的闪烁圆点该画在哪：占位格的位置和它所在的裁切范围（窗口坐标）。
#[derive(Clone, Copy, PartialEq)]
pub(super) struct SpinnerSpot {
    token: u64,
    bounds: Bounds<Pixels>,
    clip: Bounds<Pixels>,
    /// 记录该点时侧栏的横向滑移量，用于开合动画期间平滑跟手不重算
    recorded_dx: Pixels,
}

/// 侧栏记下的闪烁圆点位置与侧栏开合滑移状态。侧栏复用缓存时不重画、也不重记，位置照旧有效。
#[derive(Default)]
pub(super) struct SpinnerSpots {
    now: Vec<SpinnerSpot>,
    slide_dx: Pixels,
    visible_w: Pixels,
}

pub(super) type SharedSpots = Rc<RefCell<SpinnerSpots>>;

/// 把记录到的占位格位置按当前侧栏滑移量平移，并裁切到当前可见范围；滑出视野或滚动裁掉时返回 `None`。
fn clipped_spot_bounds(
    spot: SpinnerSpot,
    slide_dx: Pixels,
    visible_w: Pixels,
) -> Option<(Bounds<Pixels>, Bounds<Pixels>)> {
    let dx = slide_dx - spot.recorded_dx;
    let mut bounds = spot.bounds;
    bounds.origin.x += dx;
    let left = (spot.clip.origin.x + dx).max(px(0.));
    let right = (spot.clip.right() + dx).min(visible_w);
    let top = spot.clip.top();
    let bottom = spot.clip.bottom();
    if right <= left || bottom <= top {
        return None;
    }
    if bounds.right() <= left
        || bounds.left() >= right
        || bounds.bottom() <= top
        || bounds.top() >= bottom
    {
        return None;
    }
    Some((bounds, Bounds::from_corners(point(left, top), point(right, bottom))))
}

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
        let full_w = self.sidebar_width().to_pixels(window.rem_size());
        let target = if self.sidebar_open { full_w } else { px(0.) };
        let width = transition(
            "sidebar-width",
            target,
            Transition::new(duration).easing(easing),
            window,
            cx,
        );
        let slide_dx = width - full_w;
        {
            let mut spots = self.sidebar_spinners.borrow_mut();
            spots.slide_dx = slide_dx;
            spots.visible_w = width;
            if width <= px(0.) {
                // 完全收起时侧栏不画，记下的闪烁圆点位置作废
                spots.now.clear();
            }
        }
        // 侧栏按全宽画、整块从左边滑进滑出（外层裁切），开合过程中它自己的尺寸不变
        (width > px(0.)).then(|| {
            div().w(width).h_full().flex_shrink_0().overflow_hidden().child(
                div().relative().left(slide_dx).h_full().child(
                    self.sidebar_view
                        .clone()
                        .cached(StyleRefinement::default().w(self.sidebar_width()).h_full()),
                ),
            )
        })
    }

    /// 比对侧栏的指纹，变了才通知它重画（每帧调一次，在画侧栏之前）。
    /// 指纹要盖住 `render_sidebar` / `render_session_row` 读到的全部状态。
    pub(super) fn sync_sidebar(&mut self, cx: &mut Context<Self>) {
        // 正看着的会话不算待查看（切过来、关掉设置页时清掉）
        if !self.settings_open {
            let active = self.active;
            self.sessions[active].unread = false;
        }
        let mut h = DefaultHasher::new();
        // 外观、字号、字体、配色、当前内核（档案名显示在顶栏 / 侧栏）变了全部重画
        (
            crate::theme::is_dark(),
            f32::from(root_font_size()).to_bits(),
            crate::theme::palette_index(),
            sans(),
            self.sidebar_width_rem().to_bits(),
            self.settings.kernel().key(),
        )
            .hash(&mut h);
        // 启用的 agent（内核状态行、会话标签按档案显示名画）：停用 / 改名也要重画
        for agent in &self.settings.acp.agents {
            (&agent.id, agent.display_name()).hash(&mut h);
        }
        for group in self.session_groups() {
            (&group.key, &group.name).hash(&mut h);
            for i in group.members {
                (i, self.sessions[i].token).hash(&mut h);
            }
        }
        let mut expanded: Vec<&String> = self.sidebar_expanded.iter().collect();
        expanded.sort();
        (&self.settings.sidebar_folded, expanded).hash(&mut h);
        (
            self.settings.sidebar_grouping(),
            self.sessions_by_activity,
            self.settings.sidebar_kernel_status,
            self.settings_open,
            self.settings_form.tab,
            self.sidebar_open,
            self.renaming,
            self.confirm_delete,
            self.sidebar_hover,
            self.sidebar_filter,
            self.active,
            &self.cwd,
        )
            .hash(&mut h);
        // 列表底部渐隐跟手：档位变了侧栏要重画（缓存 view 不重画就不会跟着变）。
        // 单独 hash：上面那个元组已经 12 项了，`Hash` 只给到 12 元组
        scroll_handle_edge(&self.sidebar_scroll).hash(&mut h);
        // 登记过的项目（空项目也占一组）和正在确认移除的项目
        (&self.settings.sidebar_projects, &self.confirm_remove_project).hash(&mut h);
        // 空间切换（分段高亮、小点）和会话类型
        self.settings.space.hash(&mut h);
        for s in &self.sessions {
            (s.kind, s.is_chat() && s.busy, s.is_chat() && s.unread).hash(&mut h);
        }
        for (_, k) in &self.kernels {
            (k.started, k.ready, k.error, &k.status).hash(&mut h);
        }
        for s in &self.sessions {
            let age = s.meta.as_ref().map(|m| age_label(m.updated_ms));
            let archived = s.meta.as_ref().is_some_and(|m| m.archived);
            (
                &s.title,
                s.busy,
                s.busy && s.waiting_user(),
                s.unread,
                age,
                archived,
                s.task,
                s.worktree.as_ref().map(|w| (&w.branch, &w.repo)),
            )
                .hash(&mut h);
        }
        // 后台命令在跑：行首会换成终端微标，跑没了也要跟着换回来
        for i in 0..self.sessions.len() {
            self.session_has_running_bg(i).hash(&mut h);
        }
        let key = h.finish();
        if key != self.sidebar_key {
            self.sidebar_key = key;
            self.sidebar_view.update(cx, |_, cx| cx.notify());
        }
    }

    /// 侧栏里运行中会话的闪烁圆点位置与裁切范围（窗口坐标）。
    /// `SidebarSpinners` 的 `canvas` 在 `SidebarView` prepaint 之后取这里算好的位置直接绘制。
    pub(super) fn sidebar_spinner_spots(&self) -> Vec<(Bounds<Pixels>, Bounds<Pixels>)> {
        let spots = self.sidebar_spinners.borrow();
        spots
            .now
            .iter()
            .filter(|spot| {
                self.sessions.iter().any(|s| s.token == spot.token && s.busy && !s.waiting_user())
            })
            .filter_map(|spot| clipped_spot_bounds(*spot, spots.slide_dx, spots.visible_w))
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
        // 这一遍重新记闪烁圆点的位置（各行的占位格在 prepaint 时记）
        self.sidebar_spinners.borrow_mut().now.clear();
        let mut body = v_flex().gap_0().pb_2();

        // 会话：按工作目录分组，当前项目在前，其余按最近活动排序（或按日期分组）；
        // 筛选时按同样的顺序排成一组。第一组的组名行右侧带筛选 / 排序切换
        // （悬停才显出来，开着的开关常显）
        let filter = self.sidebar_filter;
        // 代码空间「按项目」：导航里有「添加项目」，项目组头有移除；其余分法没有
        let by_project = self.sidebar_by_project();
        let groups = if filter == SidebarFilter::All {
            self.session_groups()
        } else {
            let members: Vec<usize> = self
                .session_groups()
                .into_iter()
                .flat_map(|g| g.members)
                .filter(|&i| self.sidebar_keeps(filter, i))
                .collect();
            let name = format!("{} · {}", filter.label(), members.len()).into();
            vec![SessionGroup { key: String::new(), name, dir: None, members }]
        };
        let activity = |i: usize| self.sessions[i].meta.as_ref().map_or(u64::MAX, |m| m.updated_ms);
        let keep = |i: usize| {
            let s = &self.sessions[i];
            i == self.active || s.busy || s.unread
        };
        for (g, SessionGroup { key, name, dir, members }) in groups.into_iter().enumerate() {
            let group_name = name.clone();
            let group_dir = dir;
            // 筛选时不折叠、不收（筛出来的本来就少）
            let foldable = filter == SidebarFilter::All;
            // 已归档组默认收起、点开就全列，开合只记在内存里（`sidebar_expanded`）
            let archive = key == ARCHIVED_KEY;
            // 任务区：对话历史的下半区。上面排着项目，这里前加一道分隔线
            let tasks = key == TASKS_KEY;
            let folded = foldable
                && if archive {
                    !self.sidebar_expanded.contains(&key)
                } else {
                    self.group_folded(&key)
                };
            let show_all = archive || self.sidebar_expanded.contains(&key);
            let total = members.len();
            let more_key = key.clone();
            let (members, hidden) = if foldable {
                visible_members(&members, folded, show_all, keep, activity)
            } else {
                (members, 0)
            };
            // 组名：点了折叠 / 展开整组；折叠时后面带上会话数（单独一块，组名截短时也留着），
            // 箭头常显（展开时悬停才显出来）。任务区比项目名多一个小图标，一眼分得开
            let label = div()
                .min_w_0()
                .h_flex()
                .gap_1()
                .whitespace_nowrap()
                .text_size(TEXT_META)
                .text_color(rgb(INK_SOFT))
                .when(tasks, |d| {
                    d.child(
                        Icon::new(IconAll::ListTodo)
                            .size_3()
                            .flex_shrink_0()
                            .text_color(rgb(INK_SOFT)),
                    )
                })
                .child(div().min_w_0().overflow_hidden().text_ellipsis().child(name))
                .when(folded, |d| {
                    d.child(
                        div()
                            .flex_shrink_0()
                            .font_features(tabular_nums())
                            .child(format!("· {total}")),
                    )
                });
            let title = div().id(("group-title", g)).relative().flex_1().min_w_0().h_flex();
            let title = if foldable {
                let chevron =
                    Icon::new(if folded { IconAll::ChevronRight } else { IconAll::ChevronDown })
                        .size_3()
                        .text_color(rgb(INK_SOFT));
                // 箭头放在组名左边的留白里（绝对定位），不占组名的宽度：第一组右侧还排着几个按钮
                title
                    .role(Role::Button)
                    .aria_expanded(!folded)
                    .cursor_pointer()
                    .child(
                        div()
                            .absolute()
                            .left(rems(-0.875))
                            .top_0()
                            .bottom_0()
                            .flex()
                            .items_center()
                            .when(!folded, |d| {
                                d.opacity(0.).group_hover("group-header", |d| d.opacity(1.))
                            })
                            .child(chevron),
                    )
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let key = key.clone();
                        if key == ARCHIVED_KEY {
                            if !this.sidebar_expanded.remove(&key) {
                                this.sidebar_expanded.insert(key);
                            }
                            cx.notify();
                            return;
                        }
                        this.save_ui_pref(
                            move |s| {
                                // 已经按另一种写法记着这一组的折叠：认得出是同一条，别叠一条新的
                                let had = s.sidebar_folded.iter().any(|k| same_group_key(k, &key));
                                s.sidebar_folded.retain(|k| !same_group_key(k, &key));
                                if !had {
                                    s.sidebar_folded.push(key.clone());
                                }
                            },
                            cx,
                        );
                        cx.notify();
                    }))
            } else {
                title.child(label)
            };
            let header = div()
                .id(("group-header", g))
                .group("group-header")
                .h_flex()
                .items_center()
                .gap_1()
                // 外边距 + 内边距（而不是整段内边距）：任务区的分隔线和会话行一样两头缩进，不顶到面板边上
                .mx_2()
                .px_2()
                .pt(px(if g == 0 { 10. } else { 16. }))
                .pb_1()
                // 任务区前一道分隔线：上面是项目，下面是任务
                .when(tasks && g > 0, |d| {
                    d.mt(px(6.)).pt(px(12.)).border_t_1().border_color(rgb(LINE))
                })
                .child(title);
            // 折叠了的第一组只留筛选按钮：排序 / 分组方式 / 打开目录用不上，
            // 悬停才显的按钮不显时也占宽度，留着会把组名和会话数挤成省略号
            let header = if g == 0 && (filter != SidebarFilter::All || folded) {
                header.child(self.sidebar_filter_menu(cx))
            } else if g == 0 {
                let sort_on = self.sessions_by_activity;
                header.child(self.sidebar_filter_menu(cx)).when(by_project, |d| {
                    // 按时间分组时桶内总是按最近活动排，排序切换没有意义
                    d.child(
                        Self::sidebar_icon_button(
                            "sort-sessions",
                            IconAll::ArrowDownUp,
                            if sort_on { "按打开顺序排列" } else { "按最近活动排列" },
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
            } else {
                header
            };
            // 每个项目组直接新建到自己的目录；任务组新建一个任务对话。
            // 「已归档」和其他分组不提供新建入口
            let header = header
                .when_some(group_dir.clone(), |d, path| {
                    let reveal = path.clone();
                    let remove = path.clone();
                    d.child(
                        Self::quiet_button(("group-new", g), IconAll::Plus, Surface::Sand, cx)
                            .xsmall()
                            .tooltip(format!("在「{}」中新建对话", group_name))
                            .opacity(0.)
                            .group_hover("group-header", |d| d.opacity(1.))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.add_session_in_project(path.clone(), cx);
                            })),
                    )
                    .when(!folded, |d| {
                        d.child(
                            Self::sidebar_icon_button(
                                ("group-open-workspace", g),
                                IconAll::FolderOpen,
                                "在资源管理器中打开",
                                false,
                                cx,
                            )
                            .opacity(0.)
                            .group_hover("group-header", |d| d.opacity(1.))
                            .on_click(move |_, _, cx: &mut App| cx.reveal_path(&reveal)),
                        )
                    })
                    .child(
                        Self::sidebar_icon_button(
                            ("group-remove", g),
                            IconAll::X,
                            "从侧栏移除项目",
                            false,
                            cx,
                        )
                        .opacity(0.)
                        .group_hover("group-header", |d| d.opacity(1.))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.request_remove_project(remove.clone(), cx)
                        })),
                    )
                })
                .when(tasks, |d| {
                    d.child(
                        Self::quiet_button(("group-new-task", g), IconAll::Plus, Surface::Sand, cx)
                            .xsmall()
                            .tooltip("新建任务对话")
                            .opacity(0.)
                            .group_hover("group-header", |d| d.opacity(1.))
                            .on_click(cx.listener(|this, _, _, cx| this.add_task_session(cx))),
                    )
                });
            // 移除项目：组头原地换成确认条（组里的真会话会被归档，不是删掉），
            // 会话行照常列在下面——用户在确认时还能看见这些东西会被怎么处理
            let confirming = group_dir.clone().filter(|dir| {
                self.confirm_remove_project.as_deref().is_some_and(|p| same_project_path(p, dir))
            });
            body = body.child(match confirming {
                Some(dir) => {
                    self.render_remove_project_row(g, &dir, &group_name, cx).into_any_element()
                }
                None => header.into_any_element(),
            });
            // 筛完一个不剩时的提示。「全部」下没有提示（折叠的组、空的当前项目组），
            // 别画空行：空串的行照样占着内边距和行高
            if members.is_empty() && !filter.empty_hint().is_empty() {
                body = body.child(
                    div()
                        .px_4()
                        .py_2()
                        .text_size(TEXT_SMALL)
                        .text_color(rgb(INK_SOFT))
                        .child(filter.empty_hint()),
                );
            }
            for i in members {
                body = body.child(self.render_session_row(i, window, cx));
            }
            // 「显示其余 N 个」/「收起」：只在内存里记，重启回到只列最近几个
            let more = if hidden > 0 && !folded {
                Some(format!("显示其余 {hidden} 个"))
            } else if foldable && !archive && !folded && show_all && total > GROUP_PREVIEW + 2 {
                Some("收起".to_string())
            } else {
                None
            };
            if let Some(text) = more {
                let key = more_key.clone();
                body = body.child(
                    div()
                        .id(("group-more", g))
                        .h_flex()
                        .items_center()
                        .gap_2()
                        .mx_2()
                        .px_2()
                        .py(px(5.))
                        .rounded_md()
                        .cursor_pointer()
                        .hover(|d| d.bg(rgb(SELECTED)))
                        // 和会话行的状态标记同宽，文字对齐会话标题
                        .child(div().size(px(14.)).flex_shrink_0())
                        .child(div().text_size(TEXT_SMALL).text_color(rgb(INK_SOFT)).child(text))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if !this.sidebar_expanded.remove(&key) {
                                this.sidebar_expanded.insert(key.clone());
                            }
                            cx.notify();
                        })),
                );
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

        // 设置页开在「插件」「用量」分类时，高亮的是侧栏的那一项而不是「设置」
        let plugins_open =
            self.settings_open && self.settings_form.tab == super::settings_view::PLUGINS_TAB;
        let usage_open =
            self.settings_open && self.settings_form.tab == super::usage_view::USAGE_TAB;
        // 品牌：极简几何标记 + 名称，摆在拖拽区里
        let brand = div().pl_2().child(
            div()
                .text_size(TEXT_BODY)
                .font_weight(WEIGHT_STRONG)
                .text_color(rgb(INK))
                .child("Kimi Code"),
        );
        // 贴边的面板：沙色底 + 右侧一条分隔线，占满窗口左缘，靠面差把视觉重心让给主区
        let panel = v_flex()
            .size_full()
            .bg(rgb(SIDEBAR))
            .border_r_1()
            .border_color(rgb(LINE))
            .overflow_hidden()
            .child(
                // 头部：侧栏开关 + 品牌，其余是拖拽区（顶栏在它上面一格，窗口按钮不在这里）
                div()
                    .h_flex()
                    .h(CHROME_H.to_pixels(window.rem_size()))
                    .flex_shrink_0()
                    .items_center()
                    .pl_2()
                    .child(self.sidebar_toggle("sidebar-toggle", cx))
                    .child(drag_area().h_flex().items_center().child(brand)),
            )
            // 空间切换：聊天 | 代码（品牌行下面一格，见「设计说明-聊天与Code分离.md」5.1）。
            // 另一个空间有会话在跑 / 等授权时，那一格带个小点，别切走了就忘了它
            .child(self.render_space_switch(cx))
            .child(
                nav_item("new-chat", IconAll::Plus, "新会话", false)
                    .on_click(cx.listener(|this, _, _, cx| this.add_session(cx))),
            )
            // 添加项目：把目录登记进侧栏（空项目也常驻）。按时间分组的空间里没有项目组，
            // 这个入口跟着隐藏
            .when(by_project, |d| {
                d.child(nav_item("add-project", IconAll::FolderPlus, "添加项目", false).on_click(
                    cx.listener(|this, _, window, cx| this.pick_and_add_project(window, cx)),
                ))
            })
            .child(nav_item("plugins", IconAll::Blocks, "插件", plugins_open).on_click(
                cx.listener(|this, _, window, cx| {
                    this.open_settings_at(super::settings_view::PLUGINS_TAB, window, cx)
                }),
            ))
            .child(
                nav_item("usage", IconAll::ChartColumn, "用量", usage_open)
                    .on_click(cx.listener(|this, _, window, cx| this.open_usage(window, cx))),
            )
            .child(div().mx_3().my_1().border_t_1().border_color(rgb(LINE)))
            .child(
                // 列表滚到底部「设置」上面时渐隐，不是一刀切（同对话区的下边缘）；
                // 列表底下留出渐隐的高度，滚到头时最后一行不被盖住。滚到底了渐隐收成 0
                // （档位并进 `sync_sidebar` 的指纹，否则缓存 view 不会跟着变）
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .v_flex()
                    .child(
                        div()
                            .id("sidebar-scroll")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .track_scroll(&self.sidebar_scroll)
                            .child(div().pb_4().child(body)),
                    )
                    .child({
                        let h = scroll_handle_edge(&self.sidebar_scroll).heights().1;
                        let rem = window.rem_size();
                        div().absolute().left_0().right_0().bottom_0().h(h.to_pixels(rem)).bg(
                            linear_gradient(
                                180.,
                                linear_color_stop(Rgba { a: 0., ..rgb(SIDEBAR) }, 0.),
                                linear_color_stop(rgb(SIDEBAR), 1.),
                            )
                            .color_space(ColorSpace::Oklab),
                        )
                    }),
            )
            .child(
                // 底部：设置 + 内核状态
                div()
                    .v_flex()
                    .gap_1()
                    .pb_3()
                    .child(
                        nav_item(
                            "open-settings",
                            IconAll::Settings,
                            "设置",
                            self.settings_open && !plugins_open && !usage_open,
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            // 用量页也是设置页画的：在用量页上点「设置」是切过去，不是关掉
                            if this.settings_open
                                && this.settings_form.tab != super::usage_view::USAGE_TAB
                            {
                                this.close_settings(cx);
                            } else if this.settings_open {
                                // 设置表单还是进用量页之前那份（可能有没保存的改动），只换分类
                                this.settings_form.tab = this.usage_page.settings_tab;
                                cx.notify();
                            } else {
                                this.open_settings(window, cx);
                            }
                        })),
                    )
                    .child(div().v_flex().gap_1().px_4().pt_1().children(
                        // 默认不显示，可在设置页打开（「侧栏显示内核状态」）
                        if self.settings.sidebar_kernel_status {
                            self.kernel_status_rows()
                        } else {
                            Default::default()
                        },
                    )),
            );

        // 侧栏占满窗口左缘：全高、无外边距、不浮起
        div().w(self.sidebar_width()).flex_shrink_0().h_full().child(panel)
    }

    /// 空间切换（品牌行下面那格）：聊天 | 代码。
    /// 另一个空间里有会话在跑 / 等授权时，那一格带个小点（等授权红、其余赤陶），
    /// 别切走了就忘了它在跑。
    fn render_space_switch(&self, cx: &mut Context<Self>) -> Div {
        let current = self.space();
        // 每个空间的「有动静」：跑着的算一个（等授权的优先标红），待查看的算一个
        let dot = |space: Space| {
            let (busy, waiting, unread) = self
                .sessions
                .iter()
                .filter(|s| Space::of(s.kind) == space)
                .fold((false, false, false), |(b, w, u), s| {
                    (b || s.busy, w || (s.busy && s.waiting_user()), u || s.unread)
                });
            if waiting {
                Some(RED)
            } else if busy || unread {
                Some(CORAL)
            } else {
                None
            }
        };
        let mut segs = div().h_flex().flex_1().min_w_0().gap_0p5().p_0p5();
        for (n, space) in [Space::Chat, Space::Code].into_iter().enumerate() {
            let active = space == current;
            let marker = dot(space);
            segs = segs.child(
                div()
                    .id(("space", n))
                    .flex_1()
                    .h_flex()
                    .justify_center()
                    .items_center()
                    .gap_1()
                    .py_0p5()
                    .rounded_md()
                    .cursor_pointer()
                    .text_size(TEXT_META)
                    .bg(if active { rgb(CARD) } else { rgba(CLEAR) })
                    .text_color(rgb(if active { INK } else { INK_SOFT }))
                    .when(active, |d| {
                        d.font_weight(FontWeight::MEDIUM)
                            .shadow(raised_shadow())
                            .border_1()
                            .border_color(rgb(LINE))
                    })
                    .when(!active, |d| d.hover(|d| d.bg(rgb(SELECTED))))
                    .child(space.label())
                    .when_some(marker, |d, color| {
                        d.child(div().size(px(5.)).flex_shrink_0().rounded_full().bg(rgb(color)))
                    })
                    .on_click(cx.listener(move |this, _, _, cx| this.switch_space(space, cx))),
            );
        }
        div().px_2().pb_1().child(
            div()
                .h_flex()
                .items_center()
                .rounded_md()
                .bg(rgb(PAPER))
                .border_1()
                .border_color(rgb(LINE))
                .p_0p5()
                .child(segs),
        )
    }

    /// 侧栏分组标题上的小图标按钮（带提示）；`on` = 开关处于打开状态（图标高亮）。
    fn sidebar_icon_button(
        id: impl Into<ElementId>,
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

    /// 第 `i` 个会话在筛选 `filter` 下留不留。
    fn sidebar_keeps(&self, filter: SidebarFilter, i: usize) -> bool {
        let s = &self.sessions[i];
        match filter {
            SidebarFilter::All => true,
            SidebarFilter::Running => s.busy,
            SidebarFilter::Unread => s.unread || (s.busy && s.waiting_user()),
        }
    }

    #[cfg(feature = "headless-screenshot")]
    pub(crate) fn debug_sidebar_projects(&mut self, verify_flow: bool, cx: &mut Context<Self>) {
        let idx = self
            .sessions
            .iter()
            .position(|s| s.restored && s.kind == SessionKind::Code && s.has_user_message())
            .expect("没有可预览的项目会话");
        if !verify_flow {
            // 顶盖只在还没发过消息的 Code 会话里出现
            match self
                .sessions
                .iter()
                .position(|s| s.kind == SessionKind::Code && !s.has_user_message())
            {
                Some(idx) => self.activate_session(idx, cx),
                None => self.add_task_session(cx),
            }
            return;
        }
        self.activate_session(idx, cx);
        let original = self.session().token;
        let original_dir = self.session().cwd.clone();
        let target = self
            .recent_workspaces()
            .into_iter()
            .find(|dir| !same_project_path(dir, &original_dir))
            .expect("需要另一个项目目录验证切换");
        let chat_dirs: Vec<_> =
            self.sessions.iter().filter(|s| s.is_chat()).map(|s| s.cwd.clone()).collect();
        assert!(
            self.recent_workspaces()
                .iter()
                .all(|dir| !chat_dirs.iter().any(|c| same_project_path(dir, c)))
        );
        self.switch_workspace(target.clone(), cx);
        assert_ne!(self.session().token, original);
        assert_eq!(self.session().cwd, target);
        assert!(!self.session().task);
        assert_eq!(self.settings.workspace.as_ref(), Some(&target));
        let old = self.sessions.iter().find(|s| s.token == original).unwrap();
        assert_eq!(old.cwd, original_dir);
        assert!(old.has_user_message());
        self.as_task(cx);
        assert!(self.session().task);
        assert_eq!(self.session().cwd, self.acp_base.cwd);
        let blank = self.session().token;
        let total = self.sessions.len();
        self.switch_workspace(target.clone(), cx);
        assert_eq!(self.session().cwd, target);
        assert!(!self.session().task);
        assert_eq!(self.sessions.len(), total);
        assert_ne!(self.session().token, blank);
        // 侧栏项目组 / 任务组的「+」：同目录也按 task 分开复用，不串台
        let project_token = self.session().token;
        self.add_session_in_project(target.clone(), cx);
        assert_eq!(self.session().token, project_token);
        assert!(!self.session().task);
        self.add_task_session(cx);
        let task_token = self.session().token;
        assert!(self.session().task);
        self.add_task_session(cx);
        assert_eq!(self.session().token, task_token);
        self.add_session_in_project(target.clone(), cx);
        assert!(!self.session().task);
        eprintln!(
            "sidebar projects verified: existing conversation, blank conversation, task, recent projects"
        );
    }

    /// 侧栏「按项目 / 按时间」和项目增删的自检钩子（`CLAUDESKTOP_OPEN` 里调）。
    /// `mode` 为空只换分组方式；`flow` 走完整套流程并断言（添加项目 → 空项目常驻 → 移除不动别的会话）；
    /// `pending` 摆一个「移除项目」确认条出来看排版。
    ///
    /// 全程只动内存：截图 / 自检不该改写用户盘上的 `settings.json`（换分组、登记项目、切 workspace
    /// 平时都走 `save_ui_pref`），所以先把设置文件路径摘掉，跑完再放回去。
    #[cfg(feature = "headless-screenshot")]
    pub(crate) fn debug_sidebar_grouping(
        &mut self,
        time: bool,
        mode: &str,
        cx: &mut Context<Self>,
    ) {
        let settings_path = self.settings_path.take();
        self.debug_sidebar_grouping_in_memory(time, mode, cx);
        self.settings_path = settings_path;
    }

    /// [`Self::debug_sidebar_grouping`] 的本体（设置文件路径已经被摘掉）。
    #[cfg(feature = "headless-screenshot")]
    fn debug_sidebar_grouping_in_memory(&mut self, time: bool, mode: &str, cx: &mut Context<Self>) {
        if self.space() != Space::Code {
            self.switch_space(Space::Code, cx);
        }
        let grouping = if time { SidebarGrouping::Time } else { SidebarGrouping::Project };
        self.settings.sidebar_grouping = Some(grouping);
        cx.notify();
        if mode == "pending" {
            if let Some(dir) = self.session_groups().into_iter().find_map(|g| g.dir) {
                self.confirm_remove_project = Some(dir);
                cx.notify();
            }
            return;
        }
        if mode != "flow" {
            return;
        }
        // 拿当前目录下的一个现成子目录当项目，不往盘上造东西
        let dir = std::fs::read_dir(&self.cwd)
            .ok()
            .and_then(|mut it| {
                it.find_map(|e| {
                    let path = e.ok()?.path();
                    path.is_dir().then_some(path)
                })
            })
            .expect("当前目录下没有可当项目的子目录");
        let before = self.sessions.len();
        self.add_project(dir.clone(), cx);
        let added = self.session().token;
        assert!(self.settings.sidebar_projects.iter().any(|p| same_project_path(p, &dir)));
        assert!(
            self.session_groups()
                .iter()
                .any(|g| { g.dir.as_deref().is_some_and(|d| same_project_path(d, &dir)) })
        );
        // 项目里只有一条没发过消息的空会话：移除时不用二次确认，别的东西一件不少
        self.request_remove_project(dir.clone(), cx);
        assert!(self.confirm_remove_project.is_none());
        assert!(!self.settings.sidebar_projects.iter().any(|p| same_project_path(p, &dir)));
        assert!(self.session_index(added).is_none());
        assert_eq!(self.sessions.len(), before);
        assert!(
            !self
                .session_groups()
                .iter()
                .any(|g| { g.dir.as_deref().is_some_and(|d| same_project_path(d, &dir)) })
        );
        eprintln!("sidebar grouping verified: add project, empty project group, remove project");
    }

    /// 第一组组名旁的筛选按钮：点开选全部 / 进行中 / 待查看。筛选中时图标高亮、常显。
    fn sidebar_filter_menu(&self, cx: &mut Context<Self>) -> Popover {
        let on = self.sidebar_filter != SidebarFilter::All;
        let trigger =
            Self::sidebar_icon_button("sidebar-filter", IconAll::ListFilter, "筛选会话", on, cx)
                .when(!on, |d| d.opacity(0.).group_hover("group-header", |d| d.opacity(1.)));
        let handle = cx.entity();
        let focus = self.menu_focus.clone();
        menu_panel(Popover::new("sidebar-filter-menu"), &self.menu_focus)
            .anchor(Anchor::TopRight)
            .w(rems(16.25))
            .trigger(trigger)
            .content(move |_, window, cx| {
                let popover = cx.entity();
                let current = handle.read(cx).sidebar_filter;
                let mut menu = MenuList::new("filter-menu-list", &handle, &popover);
                for (i, f) in SidebarFilter::ALL.into_iter().enumerate() {
                    menu = menu.pick(
                        menu_item(("filter-item", i), f.icon(), f.label(), f.desc(), f == current),
                        move |this, cx| {
                            this.sidebar_filter = f;
                            cx.notify();
                        },
                    );
                }
                menu.finish(&focus, window, cx)
            })
    }

    /// 每个已启动内核一行：状态点 + 「自研内核 · 已连接」
    fn kernel_status_rows(&self) -> Vec<Div> {
        self.kernels
            .iter()
            .filter(|(_, slot)| slot.started)
            .map(|(key, slot)| {
                let kind = KernelChoice::from_key(key, &self.settings.acp);
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
                            .child(format!("{} · {}", kind.label(&self.settings.acp), slot.status)),
                    )
            })
            .collect()
    }

    /// 侧栏分组：(组名, 会话下标)，只列**当前空间**的会话。
    /// 代码空间默认「按项目」：上半区是项目，下半区是任务（[`TASKS_KEY`]），最后是已归档；
    /// 设成「按时间」或聊天空间（没有工作目录）时一律按日期分组，任务和项目混在一起。
    /// 组内默认按打开顺序，按活动排序时新会话在前、其余按最近更新。
    pub(super) fn session_groups(&self) -> Vec<SessionGroup> {
        let space = self.space().kind();
        let archived = |i: usize| self.sessions[i].meta.as_ref().is_some_and(|m| m.archived);
        let is_task = |i: usize| self.sessions[i].task;
        let by_project = self.sidebar_by_project();
        // 按项目时任务独立成组（下面 `assemble_groups` 补），其余分法把任务混进日期桶
        let groups =
            self.active_session_groups(|i| !(archived(i) || by_project && is_task(i)), by_project);
        assemble_groups(
            groups,
            self.sessions.len(),
            |i| self.session_activity(i),
            is_task,
            archived,
            |i| self.sessions[i].kind == space,
            by_project,
        )
    }

    /// 侧栏此刻是不是「按项目」组织（聊天空间没有工作目录，永远是按时间）。
    pub(super) fn sidebar_by_project(&self) -> bool {
        self.space().kind() == SessionKind::Code
            && self.settings.sidebar_grouping() == SidebarGrouping::Project
    }

    /// 这一组在侧栏里是不是折叠着（`dir:` 键按项目路径归一比较）。
    fn group_folded(&self, key: &str) -> bool {
        self.settings.sidebar_folded.iter().any(|k| same_group_key(k, key))
    }

    /// 某个会话最近活动的时刻（没落盘的算最新）。
    fn session_activity(&self, i: usize) -> u64 {
        self.sessions[i].meta.as_ref().map_or(u64::MAX, |m| m.updated_ms)
    }

    /// 没归档的会话（`listed`）按工作目录或日期分组（只列当前空间的）。
    /// `by_project` 为假时按日期分（代码空间的「按时间」和聊天空间都这样）。
    fn active_session_groups(
        &self,
        listed: impl Fn(usize) -> bool,
        by_project: bool,
    ) -> Vec<SessionGroup> {
        // 无元信息的会话（刚开、还没落盘）视为最新
        let activity = |i: usize| self.session_activity(i);
        let space = self.space().kind();
        let listed =
            |i: usize| listed(i) && !self.sessions[i].ephemeral && self.sessions[i].kind == space;
        if !by_project {
            let now =
                SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
            let offset = crate::usage::local_offset_secs();
            let mut buckets: [Vec<usize>; 4] = Default::default();
            for i in (0..self.sessions.len()).filter(|&i| listed(i)) {
                buckets[crate::usage::day_bucket(activity(i).min(now), now, offset)].push(i);
            }
            return buckets
                .into_iter()
                .zip(crate::usage::DAY_BUCKETS)
                .filter(|(members, _)| !members.is_empty())
                .map(|(mut members, name)| {
                    members.sort_by_key(|i| std::cmp::Reverse(activity(*i)));
                    SessionGroup {
                        key: format!("date:{name}"),
                        name: name.into(),
                        dir: None,
                        members,
                    }
                })
                .collect();
        }
        // 当前目录是项目时它一组总在最前（空着也列出）；不是项目（对话都在任务区）就不占位
        let mut groups: Vec<(PathBuf, Vec<usize>)> = if self.is_project_dir(&self.cwd) {
            vec![(self.cwd.clone(), Vec::new())]
        } else {
            Vec::new()
        };
        // 显式登记过的项目：一条会话都没有也常驻在侧栏（点组头「+」就能开工）。
        // 目录暂时不在（外接盘 / 网络盘没挂上）就不占位，插回来自己会出现
        for dir in &self.settings.sidebar_projects {
            if !dir.is_dir() || groups.iter().any(|(d, _)| same_project_path(d, dir)) {
                continue;
            }
            groups.push((dir.clone(), Vec::new()));
        }
        for (i, s) in self.sessions.iter().enumerate().filter(|&(i, _)| listed(i)) {
            let dir = s.group_dir();
            match groups.iter_mut().find(|(d, _)| same_project_path(d, dir)) {
                Some((_, members)) => members.push(i),
                None => groups.push((dir.to_path_buf(), vec![i])),
            }
        }
        groups
            .into_iter()
            .map(|(cwd, mut members)| {
                if self.sessions_by_activity {
                    members.sort_by_key(|i| std::cmp::Reverse(activity(*i)));
                }
                let name = if same_project_path(&cwd, &self.cwd) {
                    self.project_name.clone()
                } else {
                    project_label(&cwd)
                };
                SessionGroup {
                    key: format!("dir:{}", cwd.display()),
                    name,
                    dir: Some(cwd),
                    members,
                }
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
        // 设置页开着时选中的是「设置」，当前会话不再同时高亮
        let active = self.active == i && !self.settings_open;
        let row = div()
            .id(("session", i))
            .role(Role::ListItem)
            .aria_selected(active)
            .aria_label(s.title.clone())
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

        // 状态标记的优先级：等授权红点 > 运行中闪烁圆点 > 待查看赤陶点 > 后台命令终端微标
        let bg_running = !s.busy && self.session_has_running_bg(i);
        let marker = if s.busy && s.waiting_user() {
            div().size(px(6.)).rounded_full().bg(rgb(RED)).into_any_element()
        } else if s.busy {
            // 闪烁圆点由 SidebarSpinners 叠在这里画（见文件头）：这里只记窗口坐标，居中对齐 14px 槽位
            let (spots, token) = (self.sidebar_spinners.clone(), s.token);
            canvas(
                move |bounds, window, _| {
                    let mut spots = spots.borrow_mut();
                    let recorded_dx = spots.slide_dx;
                    spots.now.push(SpinnerSpot {
                        token,
                        bounds,
                        clip: window.content_mask().bounds,
                        recorded_dx,
                    });
                },
                |_, _, _, _| {},
            )
            .size_full()
            .into_any_element()
        } else if s.unread {
            // 有还没看的新结果：赤陶实心点
            div().size(px(7.)).rounded_full().bg(rgb(CORAL)).into_any_element()
        } else if bg_running {
            // 后台命令还在跑（回合已经结束）：安静的终端小图标
            Icon::new(IconAll::SquareTerminal).size_3().text_color(rgb(CORAL)).into_any_element()
        } else {
            div().into_any_element()
        };
        let age = s.meta.as_ref().map(|m| age_label(m.updated_ms));
        let token = s.token;
        let actions = motion::hover_opacity(
            ("row-actions", token as usize),
            self.sidebar_hover == Some(token),
            window,
            cx,
        );
        let icon_button = |id: &'static str, icon: IconAll, tip: &'static str| {
            div()
                .id((id, i))
                .p(px(3.))
                .rounded_md()
                .cursor_pointer()
                .hover(|d| d.bg(rgb(HOVER_STRONG)))
                .tooltip(move |window, cx| Tooltip::new(tip).build(window, cx))
                .child(Icon::new(icon).size_3p5().text_color(rgb(INK_SOFT)))
        };
        // 还没落盘的会话（没发过消息）没东西可留，只给删除
        let archived = s.meta.as_ref().map(|m| m.archived);
        row.cursor_pointer()
            .when(!active, |d| d.hover(|d| d.bg(rgb(SELECTED))))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                this.set_sidebar_hover(token, *hovered, cx)
            }))
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                // Ctrl+点击：在右边另开一块对话 Pane（同一个会话已经开着就只是聚焦它）
                if event.modifiers().control {
                    this.open_chat_in_pane(i, window, cx);
                } else {
                    this.activate_session(i, cx);
                }
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
                    .when(active, |d| d.font_weight(WEIGHT_OPTION))
                    .child(s.title.clone()),
            )
            // 跑在自己 worktree 里的会话：行尾一个小分支图标（悬停显示分支名）
            .when_some(s.worktree.as_ref().map(|w| w.branch.clone()), |row, branch| {
                let tip = format!("git worktree：{branch}");
                row.child(
                    div()
                        .id(("session-worktree", i))
                        .flex_shrink_0()
                        .text_color(rgb(INK_SOFT))
                        .tooltip(move |window, cx| super::chat::text_tip(&tip, window, cx))
                        .child(Icon::new(IconAll::GitBranch).size_3p5()),
                )
            })
            .when_some(age, |row, age| {
                row.child(
                    div().flex_shrink_0().text_size(TEXT_META).text_color(rgb(INK_SOFT)).child(age),
                )
            })
            // 悬停时盖在右侧标签上的操作按钮；左边先渐隐一段，盖住标题时不是从字中间一刀切
            .child(
                div()
                    .absolute()
                    .right(px(4.))
                    .top_0()
                    .bottom_0()
                    .h_flex()
                    .opacity(actions)
                    .child(div().w_6().h_full().bg(linear_gradient(
                        90.,
                        linear_color_stop(Rgba { a: 0., ..rgb(SELECTED) }, 0.),
                        linear_color_stop(rgb(SELECTED), 1.),
                    )))
                    .child(
                        div()
                            .h_flex()
                            .h_full()
                            .items_center()
                            .gap_0p5()
                            .rounded_r_md()
                            .bg(rgb(SELECTED))
                            .child(
                                icon_button("session-rename", IconAll::SquarePen, "重命名")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.start_rename(i, window, cx);
                                    })),
                            )
                            .when_some(archived, |d, archived| {
                                let (icon, tip) = if archived {
                                    (IconAll::ArchiveRestore, "取消归档")
                                } else {
                                    (IconAll::Archive, "归档")
                                };
                                d.child(icon_button("session-archive", icon, tip).on_click(
                                    cx.listener(move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.set_archived(i, !archived, cx);
                                    }),
                                ))
                            })
                            .child(icon_button("session-delete", IconAll::X, "删除").on_click(
                                cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.renaming = None;
                                    // 侧栏同一时间只留一条编辑 / 确认态
                                    this.confirm_remove_project = None;
                                    this.confirm_delete = Some(this.sessions[i].token);
                                    cx.notify();
                                }),
                            )),
                    ),
            )
    }

    pub(crate) fn start_rename(&mut self, idx: usize, window: &mut Window, cx: &mut Context<Self>) {
        let s = &self.sessions[idx];
        self.renaming = Some(s.token);
        self.rename_focused = false;
        // 侧栏同一时间只留一条编辑 / 确认态
        self.confirm_delete = None;
        self.confirm_remove_project = None;
        let title = s.title.to_string();
        self.rename_input.update(cx, |state, cx| {
            state.set_value(title, window, cx);
            state.focus(window, cx);
            state.select_all(window, cx);
        });
        cx.notify();
    }

    /// 归档 / 取消归档第 `idx` 个会话（写进元信息）。归档的是正看着的会话时换走：
    /// 有还没发过消息的新会话就切过去，没有就开一个，免得它留在眼前像没归档成功。
    pub(crate) fn set_archived(&mut self, idx: usize, archived: bool, cx: &mut Context<Self>) {
        // 归档一个建过 worktree 但什么都没改的会话：顺手清掉那个空目录
        if archived {
            self.cleanup_worktree(idx);
        }
        let Some(meta) = self.sessions.get_mut(idx).and_then(|s| s.meta.as_mut()) else {
            return;
        };
        meta.archived = archived;
        if let Some(store) = &self.store
            && let Err(err) = store.save_meta(meta)
        {
            self.toast(Notification::error(format!("保存会话失败：{err}")), cx);
        }
        if archived && idx == self.active {
            // 换到同空间里还没发过消息的新会话，没有就在这个空间新开一个
            let space = self.space();
            let fresh = self
                .sessions
                .iter()
                .position(|s| !s.has_user_message() && Space::of(s.kind) == space);
            match fresh {
                Some(fresh) => self.activate_session(fresh, cx),
                None => self.open_in_space(space, cx),
            }
        }
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

    /// 项目组头的移除确认条：组里的真会话会被归档到「已归档」，不会丢数据。
    /// 窄侧栏放不下一行「移除 X 并归档 N 个会话？」，所以标题单独一行，
    /// 后果说明和按钮排第二行（没有真会话时一行就够）。
    fn render_remove_project_row(
        &self,
        g: usize,
        dir: &Path,
        name: &SharedString,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let real = self
            .project_unarchived_sessions(dir)
            .into_iter()
            .filter(|&i| self.is_real_session(i))
            .count();
        let target = dir.to_path_buf();
        let small_button = |id: &'static str, label: &'static str, danger: bool| {
            div()
                .id((id, g))
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
        let actions = div()
            .h_flex()
            .flex_shrink_0()
            .items_center()
            .gap_2()
            .child(small_button("group-remove-yes", "移除", true).on_click(cx.listener(
                move |this, _, _, cx| {
                    this.remove_project(&target, cx);
                },
            )))
            .child(small_button("group-remove-no", "取消", false).on_click(cx.listener(
                |this, _, _, cx| {
                    this.confirm_remove_project = None;
                    cx.notify();
                },
            )));
        let title = div()
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .text_ellipsis()
            .whitespace_nowrap()
            .text_size(TEXT_SMALL)
            .text_color(rgb(RED))
            .child(format!("移除「{name}」？"));
        // 内边距跟组头一致（第一组 10、其余 16），确认时整块不上下跳
        let row = div()
            .id(("group-remove-confirm", g))
            .v_flex()
            .gap_1()
            .mx_2()
            .px_2()
            .pt(px(if g == 0 { 10. } else { 16. }))
            .pb_1()
            .child(div().h_flex().items_center().gap_2().child(title));
        if real == 0 {
            return row.child(div().h_flex().justify_end().child(actions));
        }
        row.child(
            div()
                .h_flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_size(TEXT_META)
                        .text_color(rgb(INK_SOFT))
                        .child(format!("归档 {real} 个会话")),
                )
                .child(actions),
        )
    }

    /// 删除会话：停掉进行中的一轮，删日志和元信息，用量统计里也去掉。
    /// 删到一个不剩时补一个新会话。
    /// 调试钩子：让第 `idx` 个会话进入删除确认。
    pub(crate) fn ask_delete(&mut self, idx: usize, cx: &mut Context<Self>) {
        // 侧栏同一时间只留一条编辑 / 确认态
        self.renaming = None;
        self.confirm_remove_project = None;
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
        if let (Some(kernel), Some(id)) = (&kernel, s.acp_id.clone()) {
            if s.busy {
                let _ = kernel.cancel(&id);
            }
            // 停掉它起的后台命令、释放内核里的会话状态
            kernel.close_session(&id);
        }
        let removed = self.sessions.remove(idx);
        let id = removed.meta.as_ref().map(|m| m.id.clone());
        // 会话的 worktree：不丢东西的前提下能删多少删多少，留下的说一声（见 worktree.rs）
        self.cleanup_worktree_of(&removed, cx);
        // 先关掉日志文件再删
        drop(removed);
        if let Some(id) = id {
            self.usage_index_generation += 1;
            self.usage.retain(|t| t.session != id);
            if let Some(store) = &self.store
                && let Err(err) = store.delete(&id)
            {
                self.toast(Notification::error(format!("删除会话文件失败：{err}")), cx);
            }
        }
        if self.sessions.is_empty() {
            let space = self.space();
            self.open_in_space(space, cx);
        } else if idx < self.active || self.active >= self.sessions.len() {
            self.active = self.active.saturating_sub(1);
        }
        cx.notify();
    }
}

/// 侧栏分组的收尾：在项目分组后面补「任务」和「已归档」两组。
/// `projects` 是 `active_session_groups` 排好的项目分组，其余参数从会话列表里读；
/// `in_space` 决定一个会话在不在当前空间；`split_tasks` 为假（按时间 / 聊天空间）时
/// 不单独拆出任务区——那些会话已经在日期桶里了。
fn assemble_groups(
    mut projects: Vec<SessionGroup>,
    total: usize,
    activity: impl Fn(usize) -> u64,
    is_task: impl Fn(usize) -> bool,
    archived: impl Fn(usize) -> bool,
    in_space: impl Fn(usize) -> bool,
    split_tasks: bool,
) -> Vec<SessionGroup> {
    // 任务区：建立时没为它选过文件夹的对话，按最近活动排（最近在上）。
    // 聊天会话恒不是任务（没有工作目录），所以只在代码空间里有这一组
    let mut tasks: Vec<usize> = if split_tasks {
        (0..total).filter(|&i| in_space(i) && !archived(i) && is_task(i)).collect()
    } else {
        Vec::new()
    };
    if !tasks.is_empty() {
        tasks.sort_by_key(|&i| std::cmp::Reverse(activity(i)));
        projects.push(SessionGroup {
            key: TASKS_KEY.into(),
            name: "任务".into(),
            dir: None,
            members: tasks,
        });
    }
    // 已归档：最近归档 / 用过的在前（也是当前空间的）
    let mut old: Vec<usize> = (0..total).filter(|&i| in_space(i) && archived(i)).collect();
    if !old.is_empty() {
        old.sort_by_key(|&i| std::cmp::Reverse(activity(i)));
        projects.push(SessionGroup {
            key: ARCHIVED_KEY.into(),
            name: "已归档".into(),
            dir: None,
            members: old,
        });
    }
    projects
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

#[cfg(test)]
mod tests {
    use super::*;

    // 会话 i 的最近活动时间就是 i（越大越新）
    fn shown(n: usize, folded: bool, show_all: bool, keep: &[usize]) -> (Vec<usize>, usize) {
        let members: Vec<usize> = (0..n).collect();
        visible_members(&members, folded, show_all, |i| keep.contains(&i), |i| i as u64)
    }

    #[core::prelude::v1::test]
    fn groups_list_recent_sessions_and_keep_the_important_ones() {
        // 只多出一两个：全列
        assert_eq!(shown(7, false, false, &[]), ((0..7).collect(), 0));
        // 多了：最近的 5 个按原顺序，加上要紧的
        assert_eq!(shown(10, false, false, &[]), (vec![5, 6, 7, 8, 9], 5));
        assert_eq!(shown(10, false, false, &[1]), (vec![1, 5, 6, 7, 8, 9], 4));
        assert_eq!(shown(10, false, true, &[]), ((0..10).collect(), 0));
        // 折叠：只剩要紧的，「显示全部」也不管用
        assert_eq!(shown(3, true, false, &[]), (vec![], 3));
        assert_eq!(shown(10, true, true, &[2, 8]), (vec![2, 8], 8));
    }

    #[core::prelude::v1::test]
    fn worktree_sessions_join_the_main_repo_group() {
        // 主仓库根和一个 worktree 目录：分组按主仓库算，worktree 会话归到它那一组
        let repo = std::path::PathBuf::from("/proj/app");
        let mut s = crate::session::Session::new(
            1,
            crate::settings::KernelChoice::Demo,
            std::path::PathBuf::from("/data/worktrees/app-1234abcd/1020-1432-k7f"),
        );
        s.worktree = Some(claudesktop_core::WorktreeMeta {
            repo: repo.clone(),
            branch: "claudesktop/1020-1432-k7f".into(),
            base: "main".into(),
        });
        assert_eq!(s.group_dir(), repo.as_path());
        let mut plain = crate::session::Session::new(
            2,
            crate::settings::KernelChoice::Demo,
            std::path::PathBuf::from("/proj/app"),
        );
        assert_eq!(plain.group_dir(), plain.cwd.as_path());
        // 元信息里的 worktree 从日志恢复回来
        let meta = claudesktop_core::SessionMeta {
            id: "s".into(),
            title: "t".into(),
            cwd: s.cwd.clone(),
            agent: "builtin:x".into(),
            kernel_session_id: None,
            kernel_sessions: Vec::new(),
            created_ms: 0,
            updated_ms: 0,
            archived: false,
            task: false,
            worktree: s.worktree.clone(),
            kind: claudesktop_core::SessionKind::Code,
            web: None,
            mode: None,
            queued: Vec::new(),
        };
        let restored =
            crate::session::Session::from_records(3, meta.clone(), Vec::new(), &Default::default());
        assert_eq!(restored.worktree, s.worktree);
        assert_eq!(restored.group_dir(), repo.as_path());
        // 老元信息（没有这一项）读成 None
        let mut old = meta;
        old.worktree = None;
        assert!(
            crate::session::Session::from_records(4, old, Vec::new(), &Default::default())
                .worktree
                .is_none()
        );
        plain.task = false;
        let _ = plain;
    }

    #[core::prelude::v1::test]
    fn tasks_and_archive_follow_the_projects() {
        // 0 / 1 在项目里，2 / 3 是任务，4 归档了；活动时间越大越新
        let groups = assemble_groups(
            vec![SessionGroup {
                key: "dir:a".into(),
                name: "a".into(),
                dir: Some("/a".into()),
                members: vec![0, 1],
            }],
            5,
            |i| i as u64,
            |i| i == 2 || i == 3,
            |i| i == 4,
            |_| true,
            true,
        );
        let order: Vec<&str> = groups.iter().map(|g| g.key.as_str()).collect();
        assert_eq!(order, ["dir:a", TASKS_KEY, ARCHIVED_KEY]);
        // 任务区按最近活动排，最近的在上面
        assert_eq!(groups[1].members, [3, 2]);
        assert_eq!(groups[1].name.as_ref(), "任务");
        // 只有两边都空时才不出现这两组
        let only_tasks = assemble_groups(Vec::new(), 2, |_| 0, |_| true, |_| false, |_| true, true);
        assert_eq!(only_tasks.len(), 1);
        assert_eq!(only_tasks[0].members, [0, 1]);
    }

    /// 按时间分组：任务不单独成组（已经混在日期桶里），末尾只补「已归档」。
    #[core::prelude::v1::test]
    fn by_time_keeps_tasks_in_the_date_buckets() {
        let groups = assemble_groups(
            vec![SessionGroup {
                key: "date:今天".into(),
                name: "今天".into(),
                dir: None,
                members: vec![0, 1, 2],
            }],
            4,
            |i| i as u64,
            |i| i == 2,
            |i| i == 3,
            |_| true,
            false,
        );
        let order: Vec<&str> = groups.iter().map(|g| g.key.as_str()).collect();
        assert_eq!(order, ["date:今天", ARCHIVED_KEY]);
        assert_eq!(groups[0].members, [0, 1, 2]);
    }

    #[core::prelude::v1::test]
    fn clipped_spot_bounds_keeps_stationary_spot_and_tracks_slide() {
        let spot = SpinnerSpot {
            token: 1,
            bounds: Bounds::from_corners(point(px(20.), px(160.)), point(px(34.), px(174.))),
            clip: Bounds::from_corners(point(px(0.), px(100.)), point(px(256.), px(700.))),
            recorded_dx: px(0.),
        };
        // 静止时保留原窗口坐标
        assert_eq!(clipped_spot_bounds(spot, px(0.), px(256.)), Some((spot.bounds, spot.clip)));
        // 侧栏向左轻微滑出 10px：圆点跟着左移 10px，裁切框右界收进到 246
        let slid = clipped_spot_bounds(spot, px(-10.), px(246.)).expect("完全在裁切框内应绘制");
        assert_eq!(slid.0.origin.x, px(10.));
        assert_eq!(
            slid.1,
            Bounds::from_corners(point(px(0.), px(100.)), point(px(246.), px(700.)))
        );
        // 滑到贴近左缘被裁掉一半（20 - 25 = -5，右缘 9 > 0）时仍绘制并裁切
        let half = clipped_spot_bounds(spot, px(-25.), px(231.)).expect("半露出左缘时仍应绘制");
        assert_eq!(half.0.origin.x, px(-5.));
        assert_eq!(
            half.1,
            Bounds::from_corners(point(px(0.), px(100.)), point(px(231.), px(700.)))
        );
        // 向左滑出左缘（right = 34 - 100 = -66 <= 0）后不绘制
        assert_eq!(clipped_spot_bounds(spot, px(-100.), px(156.)), None);
        // 完全滑出侧栏可见宽度后不绘制
        assert_eq!(clipped_spot_bounds(spot, px(-256.), px(0.)), None);
        // 滚动到列表裁切框之外后不绘制
        let scrolled_out = SpinnerSpot {
            bounds: Bounds::from_corners(point(px(20.), px(80.)), point(px(34.), px(94.))),
            ..spot
        };
        assert_eq!(clipped_spot_bounds(scrolled_out, px(0.), px(256.)), None);
    }
}

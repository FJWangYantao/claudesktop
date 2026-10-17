//! 侧栏：浮起的圆角面板，含导航、按工作目录分组的会话列表、设置与内核状态。
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
//! 运行中那行的转圈不画在侧栏里（它每拍都要重画，会把整个侧栏一起带上——缓存 view 重画时
//! 里面嵌套的缓存 view 也不复用），而是行里只留个占位、记下位置，由叠在上面的小 view
//! [`SidebarSpinners`] 画。它不画在 `ChatApp` 里：节拍器通知 `ChatApp` 会连内容区一起重画（见 content.rs）。

use super::chrome::{CHROME_H, drag_area, traffic_light_gap};
use super::menu::{MenuList, menu_item, menu_panel};
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
use gpui_kit::component::popover::Popover;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::cell::RefCell;
use std::collections::HashSet;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

/// 侧栏的一组会话（同一工作目录，或按日期分组时的同一天）。
pub(crate) struct SessionGroup {
    /// 记折叠 / 展开用：`dir:<工作目录>` 或 `date:<日期组名>`
    pub key: String,
    pub name: SharedString,
    pub members: Vec<usize>,
}

/// 「已归档」组的 [`SessionGroup::key`]
pub(crate) const ARCHIVED_KEY: &str = "archived";

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
            Self::All => "按项目或日期分组",
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

/// 侧栏转圈的覆盖层：铺满窗口、不挂事件，按 [`SpinnerSpots`] 记下的位置画转圈。
pub(crate) struct SidebarSpinners {
    app: WeakEntity<ChatApp>,
}

impl SidebarSpinners {
    pub(super) fn new(app: WeakEntity<ChatApp>) -> Self {
        Self { app }
    }
}

impl Render for SidebarSpinners {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let spinners = self.app.upgrade().map(|app| app.read(cx).render_sidebar_spinners());
        div().absolute().top_0().left_0().size_full().children(spinners.into_iter().flatten())
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
        // 正看着的会话不算待查看（切过来、关掉设置页时清掉）
        if !self.settings_open {
            let active = self.active;
            self.sessions[active].unread = false;
        }
        let mut h = DefaultHasher::new();
        // 外观、字号、字体变了全部重画
        (crate::theme::is_dark(), f32::from(root_font_size()).to_bits(), sans()).hash(&mut h);
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
            self.settings.sidebar_by_date,
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
        for k in &self.kernels {
            (k.started, k.ready, k.error, &k.status).hash(&mut h);
        }
        for s in &self.sessions {
            let age = s.meta.as_ref().map(|m| age_label(m.updated_ms));
            let archived = s.meta.as_ref().is_some_and(|m| m.archived);
            (&s.title, s.busy, s.busy && s.waiting_permission(), s.unread, age, archived)
                .hash(&mut h);
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

        // 会话：按工作目录分组，当前项目在前，其余按最近活动排序（或按日期分组）；
        // 筛选时按同样的顺序排成一组。第一组的组名行右侧带筛选 / 分组方式 / 排序切换 /
        // 在资源管理器中打开（悬停才显出来，开着的开关常显）
        let filter = self.sidebar_filter;
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
            vec![SessionGroup { key: String::new(), name, members }]
        };
        let activity = |i: usize| self.sessions[i].meta.as_ref().map_or(u64::MAX, |m| m.updated_ms);
        let keep = |i: usize| {
            let s = &self.sessions[i];
            i == self.active || s.busy || s.unread
        };
        for (g, SessionGroup { key, name, members }) in groups.into_iter().enumerate() {
            // 筛选时不折叠、不收（筛出来的本来就少）
            let foldable = filter == SidebarFilter::All;
            // 已归档组默认收起、点开就全列，开合只记在内存里（`sidebar_expanded`）
            let archive = key == ARCHIVED_KEY;
            let folded = foldable
                && if archive {
                    !self.sidebar_expanded.contains(&key)
                } else {
                    self.settings.sidebar_folded.contains(&key)
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
            // 箭头常显（展开时悬停才显出来）
            let label = div()
                .min_w_0()
                .h_flex()
                .gap_1()
                .whitespace_nowrap()
                .text_size(TEXT_META)
                .text_color(rgb(INK_SOFT))
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
                            move |s| match s.sidebar_folded.iter().position(|k| *k == key) {
                                Some(at) => {
                                    s.sidebar_folded.remove(at);
                                }
                                None => s.sidebar_folded.push(key.clone()),
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
                .px_4()
                .pt(px(if g == 0 { 10. } else { 16. }))
                .pb_1()
                .child(title);
            // 折叠了的第一组只留筛选按钮：排序 / 分组方式 / 打开目录用不上，
            // 悬停才显的按钮不显时也占宽度，留着会把组名和会话数挤成省略号
            let header = if g == 0 && (filter != SidebarFilter::All || folded) {
                header.child(self.sidebar_filter_menu(cx))
            } else if g == 0 {
                let sort_on = self.sessions_by_activity;
                let by_date = self.settings.sidebar_by_date;
                header
                    .child(self.sidebar_filter_menu(cx))
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

        // 设置页开在「插件」分类时，高亮的是侧栏的「插件」而不是「设置」
        let plugins_open =
            self.settings_open && self.settings_form.tab == super::settings_view::PLUGINS_TAB;
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
            .child(nav_item("plugins", IconAll::Blocks, "插件", plugins_open).on_click(
                cx.listener(|this, _, window, cx| {
                    this.open_settings_at(super::settings_view::PLUGINS_TAB, window, cx)
                }),
            ))
            .child(
                // 列表滚到底部「设置」上面时渐隐，不是一刀切（同对话区的下边缘）；
                // 列表底下留出渐隐的高度，滚到头时最后一行不被盖住
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
                            .child(div().pb_4().child(body)),
                    )
                    .child(div().absolute().left_0().right_0().bottom_0().h(px(16.)).bg(
                        linear_gradient(
                            180.,
                            linear_color_stop(Rgba { a: 0., ..rgb(SAND) }, 0.),
                            linear_color_stop(rgb(SAND), 1.),
                        ),
                    )),
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
                            self.settings_open && !plugins_open,
                        )
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

    /// 第 `i` 个会话在筛选 `filter` 下留不留。
    fn sidebar_keeps(&self, filter: SidebarFilter, i: usize) -> bool {
        let s = &self.sessions[i];
        match filter {
            SidebarFilter::All => true,
            SidebarFilter::Running => s.busy,
            SidebarFilter::Unread => s.unread || (s.busy && s.waiting_permission()),
        }
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
            .content(move |_, _, cx| {
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
                menu.finish(&focus)
            })
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
    pub(super) fn session_groups(&self) -> Vec<SessionGroup> {
        let archived = |i: usize| self.sessions[i].meta.as_ref().is_some_and(|m| m.archived);
        let mut groups = self.active_session_groups(|i| !archived(i));
        let members: Vec<usize> = (0..self.sessions.len()).filter(|&i| archived(i)).collect();
        if !members.is_empty() {
            let mut members = members;
            // 最近归档 / 用过的在前
            members.sort_by_key(|&i| {
                std::cmp::Reverse(self.sessions[i].meta.as_ref().map_or(0, |m| m.updated_ms))
            });
            groups
                .push(SessionGroup { key: ARCHIVED_KEY.into(), name: "已归档".into(), members });
        }
        groups
    }

    /// 没归档的会话（`listed`）按工作目录或日期分组。
    fn active_session_groups(&self, listed: impl Fn(usize) -> bool) -> Vec<SessionGroup> {
        // 无元信息的会话（刚开、还没落盘）视为最新
        let activity = |i: usize| self.sessions[i].meta.as_ref().map_or(u64::MAX, |m| m.updated_ms);
        if self.settings.sidebar_by_date {
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
                    SessionGroup { key: format!("date:{name}"), name: name.into(), members }
                })
                .collect();
        }
        let mut groups: Vec<(&Path, Vec<usize>)> = vec![(self.cwd.as_path(), Vec::new())];
        for (i, s) in self.sessions.iter().enumerate().filter(|&(i, _)| listed(i)) {
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
                SessionGroup { key: format!("dir:{}", cwd.display()), name, members }
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

        // 状态标记：等授权红点、运行中转圈、待查看赤陶实心点，其余空心圈（与官方一样，每行都有个标记位）
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
        } else if s.unread {
            // 有还没看的新结果：赤陶实心点
            div().size(px(7.)).rounded_full().bg(rgb(CORAL)).into_any_element()
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
        self.confirm_delete = None;
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
            match self.sessions.iter().position(|s| !s.has_user_message()) {
                Some(fresh) => self.activate_session(fresh, cx),
                None => self.add_session(cx),
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
}

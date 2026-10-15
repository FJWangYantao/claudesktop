//! 侧栏：浮起的圆角面板，含导航、按工作目录分组的会话列表、设置与内核状态。

use super::chrome::{drag_area, traffic_light_gap, CHROME_H};
use super::ChatApp;
use crate::settings::KernelChoice;
use crate::theme::*;
use crate::theme::rgb;
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::input::Input;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

impl ChatApp {
    pub(super) fn render_sidebar(&self, cx: &mut Context<Self>) -> Div {
        let mut body = v_flex().gap_0().pb_2();

        // 会话：按工作目录分组，当前项目在前，其余按最近活动排序。
        // 第一组的组名行右侧带排序切换 / 在资源管理器中打开（悬停才显出来）
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
                header
                    .child(
                        Self::sidebar_icon_button(
                            "sort-sessions",
                            IconAll::ArrowDownUp,
                            if sort_on { "按打开顺序排列" } else { "按最近活动排列" },
                            sort_on,
                        )
                        .when(!sort_on, |d| {
                            d.opacity(0.).group_hover("group-header", |d| d.opacity(1.))
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.sessions_by_activity = !this.sessions_by_activity;
                            cx.notify();
                        })),
                    )
                    .child(
                        Self::sidebar_icon_button(
                            "open-workspace",
                            IconAll::FolderOpen,
                            "在资源管理器中打开",
                            false,
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
                body = body.child(self.render_session_row(i, cx));
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
            .child(
                nav_item("plugins", IconAll::Blocks, "插件", false).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.sessions[this.active]
                            .note("插件系统尚未开放；当前通过输入区的内核下拉切换 agent 能力");
                        cx.notify();
                    },
                )),
            )
            .child(
                div()
                    .id("sidebar-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(body),
            )
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
                    .child(
                        div()
                            .v_flex()
                            .gap_1()
                            .px_4()
                            .pt_1()
                            .children(
                                // 可在设置页关掉（「侧栏显示内核状态」）
                                self.settings
                                    .sidebar_kernel_status
                                    .then(|| self.kernel_status_rows())
                                    .unwrap_or_default(),
                            ),
                    ),
            );

        // 侧栏是浮在窗口底色上的圆角面板，四周留一圈窗口底色
        div()
            .w(SIDEBAR_W)
            .flex_shrink_0()
            .h_full()
            .pl_2()
            .py_2()
            .child(panel)
    }

    /// 侧栏里的小图标按钮（带提示）；`on` = 开关处于打开状态（图标高亮）。
    fn sidebar_icon_button(
        id: &'static str,
        icon: IconAll,
        tip: &'static str,
        on: bool,
    ) -> Stateful<Div> {
        div()
            .id(id)
            .p(px(3.))
            .rounded_md()
            .cursor_pointer()
            .hover(|d| d.bg(rgb(SELECTED)))
            .tooltip(move |window, cx| Tooltip::new(tip).build(window, cx))
            .child(
                Icon::new(icon)
                    .size_3p5()
                    .text_color(rgb(if on { CORAL } else { INK_SOFT })),
            )
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
                    // 无元信息的会话（刚开、还没落盘）视为最新
                    let activity = |i: usize| {
                        self.sessions[i]
                            .meta
                            .as_ref()
                            .map_or(u64::MAX, |m| m.updated_ms)
                    };
                    members.sort_by(|a, b| activity(*b).cmp(&activity(*a)));
                }
                let name = if g == 0 {
                    self.project_name.clone()
                } else {
                    cwd.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| cwd.display().to_string())
                        .into()
                };
                (name, members)
            })
            .collect()
    }

    fn render_session_row(&self, i: usize, cx: &mut Context<Self>) -> Stateful<Div> {
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
                    .child(Input::new(&self.rename_input).appearance(false).bordered(false).small()),
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
            Spinner::new().xsmall().color(rgb(CORAL).into()).into_any_element()
        } else {
            div()
                .size(px(9.))
                .rounded_full()
                .border_1()
                .border_color(rgb(if active { INK_SOFT } else { INK_FAINT }))
                .into_any_element()
        };
        let age = s.meta.as_ref().map(|m| age_label(m.updated_ms));
        let group: SharedString = format!("session-row-{i}").into();
        let icon_button = |id: &'static str, icon: IconAll| {
            div()
                .id((id, i))
                .p(px(3.))
                .rounded_md()
                .cursor_pointer()
                .hover(|d| d.bg(rgb(HOVER_STRONG)))
                .child(Icon::new(icon).size_3p5().text_color(rgb(INK_SOFT)))
        };
        row.group(group.clone())
            .cursor_pointer()
            .when(!active, |d| d.hover(|d| d.bg(rgb(SELECTED))))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.active = i;
                this.settings_open = false;
                cx.notify();
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
                    div()
                        .flex_shrink_0()
                        .text_size(TEXT_META)
                        .text_color(rgb(INK_SOFT))
                        .child(age),
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
                    .opacity(0.)
                    .group_hover(group, |d| d.opacity(1.))
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
        if let Some(s) = self.sessions.iter_mut().find(|s| s.token == token) {
            if !text.is_empty() && text != s.title.as_ref() {
                s.title = text.clone().into();
                s.renamed = true;
                if let (Some(store), Some(meta)) = (&self.store, &mut s.meta) {
                    meta.title = text;
                    let _ = store.save_meta(meta);
                }
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
            if let Some(store) = &self.store {
                if let Err(err) = store.delete(&id) {
                    log_note(&mut self.sessions, format!("删除会话文件失败：{err}"));
                }
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

/// 删除失败之类的提示：放进第一个会话（总是存在）。
fn log_note(sessions: &mut [crate::session::Session], text: String) {
    if let Some(s) = sessions.first_mut() {
        s.note(text);
    }
}

/// 「刚刚 / 5 分钟 / 3 小时 / 2 天」
fn age_label(updated_ms: u64) -> SharedString {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    let mins = now.saturating_sub(updated_ms) / 60_000;
    match mins {
        0 => "刚刚".into(),
        m if m < 60 => format!("{m} 分钟").into(),
        m if m < 60 * 24 => format!("{} 小时", m / 60).into(),
        m => format!("{} 天", m / (60 * 24)).into(),
    }
}

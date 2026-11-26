//! 底部输入区（多行输入，下方一行权限模式 / 附件 / 模型 / 发送）与导出、附件等动作。

use super::chrome::CHROME_H;
use super::menu::{MenuChip, MenuList, menu_item, menu_popover};
use super::widgets::Surface;
use super::{ChatApp, project_label, short_path};
use crate::model::PendingImage;
use crate::settings::KernelChoice;
use crate::theme::rgb;
use crate::theme::*;
use claudesktop_builtin::ThinkingLevel;
use claudesktop_core::Kernel;
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::base::PopoverState;
use gpui_kit::component::button::Button;
use gpui_kit::component::input::Textarea;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::sync::Arc;

impl ChatApp {
    /// 「分享」：当前会话导出为 Markdown，系统保存对话框选位置。
    pub(crate) fn share_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let idx = self.active;
        if !self.sessions[idx].history_loaded {
            self.toast(Notification::warning("这段对话还在读取，稍等再导出"), cx);
            return;
        }
        let markdown = self.sessions[idx].to_markdown(&self.settings.acp);
        let suggested = format!("{}.md", self.sessions[idx].title);
        let dir = self.cwd.clone();
        let rx = cx.prompt_for_new_path(&dir, Some(&suggested));
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(path))) = rx.await {
                let _ = this.update_in(cx, |app, _, cx| {
                    let note = match std::fs::write(&path, &markdown) {
                        Ok(()) => Notification::success(format!("会话已导出：{}", path.display())),
                        Err(err) => Notification::error(format!("导出失败：{err}")),
                    };
                    app.toast(note, cx);
                });
            }
        })
        .detach();
    }

    // ---------- 输入区 ----------
    /// 侧栏开关：展开时在侧栏头部，收起时在会话区顶栏最左（同一时刻只出现一个）。
    pub(super) fn sidebar_toggle(&self, id: &'static str, cx: &mut Context<Self>) -> Button {
        // 沙色侧栏和纸色顶栏上都会出现，取深的一档（SELECTED）两边都看得见
        Self::quiet_button(id, IconAll::PanelLeft, Surface::Sand, cx)
            .tooltip(if self.sidebar_open { "收起侧栏" } else { "展开侧栏" })
            .on_click(cx.listener(|this, _, _, cx| {
                this.sidebar_open = !this.sidebar_open;
                cx.notify();
            }))
    }

    /// 权限模式菜单里选了一项：会话立即生效，已建立的内核会话同步 set_mode；
    /// 还没建立的会话等 Started 时再交给内核
    pub(crate) fn choose_mode(&mut self, i: usize, cx: &mut Context<Self>) {
        let idx = self.active;
        let Some(mode) = self.sessions[idx].modes.get(i).map(|m| m.id.clone()) else {
            return;
        };
        let kernel = self.session_kernel(idx);
        let connected = match (kernel.as_ref(), self.sessions[idx].acp_id.as_deref()) {
            (Some(kernel), Some(id)) => {
                if let Err(err) = kernel.set_mode(id, &mode) {
                    self.sessions[idx].note(format!("切换权限模式失败：{err}"));
                    cx.notify();
                    return;
                }
                true
            }
            _ => false,
        };
        let s = &mut self.sessions[idx];
        s.choose_mode_index(i);
        if !connected {
            s.pending_mode = Some(mode);
        }
        self.persist_session_mode(idx, cx);
        cx.notify();
    }

    pub(super) fn persist_session_mode(&mut self, idx: usize, cx: &mut Context<Self>) {
        if let (Some(store), Some(meta)) = (&self.store, &self.sessions[idx].meta)
            && let Err(err) = store.save_meta(meta)
        {
            self.toast(Notification::error(format!("权限模式未能保存：{err}")), cx);
        }
    }

    #[cfg(feature = "headless-screenshot")]
    pub(crate) fn debug_history_mode_menu(&mut self, cx: &mut Context<Self>) {
        let idx = self
            .sessions
            .iter()
            .position(|s| {
                s.restored && s.kernel == crate::settings::KernelChoice::Builtin && !s.is_chat()
            })
            .expect("没有可预览的自研内核历史会话");
        self.activate_session(idx, cx);
        let session = self.session();
        assert!(!session.ready);
        assert_eq!(
            session.modes.iter().any(|m| m.id == crate::model::MODE_AUTO_ID),
            self.settings.auto_mode_default(),
        );
        eprintln!("restored mode menu verified before reconnecting");
    }

    /// 模型菜单底栏里选了一档：只作用于**这个会话**，不动设置文件里的档案。
    /// 已建立的内核会话下一轮请求起生效；还没建立的等 `Started` 再补（`flush_pending_thinking`）。
    pub(crate) fn choose_thinking(&mut self, level: Option<ThinkingLevel>, cx: &mut Context<Self>) {
        let kernel = self.session_kernel(self.active);
        let s = &mut self.sessions[self.active];
        s.thinking = level;
        match (kernel, s.acp_id.clone()) {
            (Some(kernel), Some(id)) => {
                let wire = level.map(thinking_wire);
                if let Err(err) = kernel.set_thinking(&id, wire.as_deref()) {
                    s.note(format!("切换思考强度失败：{err}"));
                }
            }
            _ => s.pending_thinking = Some(level),
        }
        cx.notify();
    }

    /// ACP 会话在模型菜单底栏里选了一档思考强度：原生 `thought_level` 或思考型 `modes`
    /// 都由内核自己挑通道，这里只管把选中的值交给它并乐观更新界面。
    pub(crate) fn choose_acp_thinking(&mut self, value: String, cx: &mut Context<Self>) {
        let kernel = self.session_kernel(self.active);
        let s = &mut self.sessions[self.active];
        let Some(index) = s.config_options.iter().position(claudesktop_core::is_selectable_thought)
        else {
            return;
        };
        let previous = s.config_options[index].current_value.clone();
        s.config_options[index].current_value = value.clone();
        match (kernel, s.acp_id.clone()) {
            (Some(kernel), Some(id)) => {
                if let Err(err) = kernel.set_thinking(&id, Some(&value)) {
                    s.config_options[index].current_value = previous;
                    s.note(format!("切换思考强度失败：{err}"));
                }
            }
            // 空闲回收 / 内核刚换：进程下次发消息才起来，先记着，`Started` 时补交
            _ => s.pending_acp_thinking = Some(value),
        }
        cx.notify();
    }

    /// `Started` 到达：把会话建立前选的档位交给内核（和 `pending_model` / `pending_mode` 一个套路）。
    pub(super) fn flush_pending_thinking(&mut self, idx: usize, kernel: Option<&dyn Kernel>) {
        let (Some(kernel), Some(wanted)) = (kernel, self.sessions[idx].pending_thinking.take())
        else {
            return;
        };
        let Some(id) = self.sessions[idx].acp_id.clone() else { return };
        let wire = wanted.map(thinking_wire);
        if let Err(err) = kernel.set_thinking(&id, wire.as_deref()) {
            self.sessions[idx].note(format!("切换思考强度失败：{err}"));
        }
    }

    /// 聊天会话的「联网」开关（输入框工具栏的 chip）：会话级，下一轮开头生效。
    /// 已建立的内核会话立刻交给内核；还没建立的等 `Started` 再补（`flush_pending_web`）。
    pub(crate) fn toggle_web(&mut self, on: bool, cx: &mut Context<Self>) {
        let kernel = self.session_kernel(self.active);
        let s = &mut self.sessions[self.active];
        if !s.is_chat() {
            return;
        }
        s.web = on;
        // 记进元信息：重开应用后还是这样（还没落过盘的等建档时一起写）
        if let (Some(store), Some(meta)) = (self.store.as_ref(), s.meta.as_mut()) {
            meta.web = Some(on);
            let _ = store.save_meta(meta);
        }
        match (kernel, s.acp_id.clone()) {
            (Some(kernel), Some(id)) if kernel.supports_chat() => {
                if let Err(err) = kernel.set_web(&id, on) {
                    s.note(format!("切换联网失败：{err}"));
                }
            }
            _ => s.pending_web = Some(on),
        }
        cx.notify();
    }

    /// `Started` 到达：把会话建立前选的联网开关交给内核。
    pub(super) fn flush_pending_web(&mut self, idx: usize, kernel: Option<&dyn Kernel>) {
        let (Some(kernel), Some(wanted)) = (kernel, self.sessions[idx].pending_web.take()) else {
            return;
        };
        if !kernel.supports_chat() {
            return;
        }
        let Some(id) = self.sessions[idx].acp_id.clone() else { return };
        if let Err(err) = kernel.set_web(&id, wanted) {
            self.sessions[idx].note(format!("切换联网失败：{err}"));
        }
    }

    /// 聊天会话的「联网」开关（`Globe`）：会话级，下一轮开头生效。
    /// 关着时联网工具根本不在工具定义里（见 `tools::chat_specs`）。
    fn render_web_chip(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let on = self.session().web;
        div()
            .id("web-chip")
            .h_flex()
            .items_center()
            .gap_1()
            .px_2()
            .h(px(22.))
            .rounded_md()
            .cursor_pointer()
            .when(on, |d| d.bg(rgb(SELECTED)))
            .hover(|d| d.bg(rgb(if on { SELECTED } else { HOVER })))
            .text_size(TEXT_SMALL)
            .text_color(rgb(if on { INK } else { INK_SOFT }))
            .tooltip(move |window, cx| {
                Tooltip::new(if on {
                    "联网开着：能搜网页、读链接。点了关掉"
                } else {
                    "联网关着：这一轮不发给它联网工具。点了打开"
                })
                .build(window, cx)
            })
            .child(Icon::new(IconAll::Globe).size_3p5())
            .child("联网")
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_web(!on, cx)))
    }

    /// 输入框上方的一张待发送图片缩略图（右上角 × 撤掉）。还在后台转正 / 缩小时半透明。
    fn pending_image_thumb(&self, i: usize, cx: &mut Context<Self>) -> Stateful<Div> {
        let image = &self.pending[i];
        let preparing = image.preparing;
        let tip = image.tip();
        div()
            .id(("pending-image", i))
            .relative()
            .size(px(56.))
            .flex_shrink_0()
            .rounded(COMPOSER_INNER_RADIUS)
            .overflow_hidden()
            .border_1()
            .border_color(rgb(LINE))
            .when(preparing, |d| d.opacity(0.55))
            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
            .child(img(Arc::new(image.preview.clone())).size_full().object_fit(ObjectFit::Cover))
            .child(
                div()
                    .id(("pending-image-x", i))
                    .absolute()
                    .top_0p5()
                    .right_0p5()
                    .size(px(16.))
                    .rounded_full()
                    .bg(rgb(INK))
                    .opacity(0.75)
                    .hover(|d| d.opacity(1.))
                    .flex()
                    .justify_center()
                    .items_center()
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.remove_pending_image(i, cx);
                    }))
                    .child(Icon::new(IconAll::X).size_3().text_color(rgb(PAPER))),
            )
    }

    /// 输入框上方的一枚待发送文本文件 chip（文件名 + 大小，右侧 × 撤掉）。
    /// 聊天才用：内容会内联进消息，所以提示里说清带上的是内容。
    fn pending_file_chip(&self, i: usize, cx: &mut Context<Self>) -> Stateful<Div> {
        let file = &self.pending_files[i];
        let name: SharedString = file.name.clone().into();
        let size: SharedString = file.size_label().into();
        let path: SharedString = file.path.display().to_string().into();
        div()
            .id(("pending-file", i))
            .h_flex()
            .items_center()
            .gap_1p5()
            .pl_2()
            .pr_1()
            .h(px(56.))
            .rounded(COMPOSER_INNER_RADIUS)
            .border_1()
            .border_color(rgb(LINE))
            .bg(rgb(SAND))
            .tooltip({
                let path = path.clone();
                move |window, cx| {
                    Tooltip::new(format!("{path}\n内容会一起发给模型")).build(window, cx)
                }
            })
            .child(Icon::new(IconAll::FileText).size_4().text_color(rgb(INK_SOFT)))
            .child(
                div()
                    .v_flex()
                    .gap_0p5()
                    .max_w(rems(12.))
                    .overflow_hidden()
                    .child(
                        div()
                            .text_size(TEXT_SMALL)
                            .text_color(rgb(INK))
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(name),
                    )
                    .child(
                        div()
                            .text_size(TEXT_META)
                            .text_color(rgb(INK_SOFT))
                            .font_features(tabular_nums())
                            .child(size),
                    ),
            )
            .child(
                div()
                    .id(("pending-file-x", i))
                    .size(px(16.))
                    .flex_shrink_0()
                    .rounded_full()
                    .flex()
                    .justify_center()
                    .items_center()
                    .cursor_pointer()
                    .hover(|d| d.bg(rgb(HOVER)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.remove_pending_file(i, cx);
                    }))
                    .child(Icon::new(IconAll::X).size_3().text_color(rgb(INK_SOFT))),
            )
    }

    /// 只重画输入区：通知输入框的 view，它的祖先 `ChatApp` 跟着重画，内容区的缓存不动（见 content.rs）。
    /// 打字、补全、聚焦这类只影响输入区的变化用它；用 `cx.notify()` 会连对话一起重画。
    pub(super) fn refresh_composer(&self, cx: &mut App) {
        self.input.update(cx, |_, cx| cx.notify());
    }

    /// 输入框工具栏的内核 chip：空 Code 会话里点开换内核；已开始的 ACP 会话只读显示当前内核；
    /// 聊天会话、只有一个可用内核、自研内核已开始的会话都不占位置。
    fn render_kernel_chip(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let s = self.session();
        if s.is_chat() {
            return None;
        }
        let label = s.kernel.label(&self.settings.acp);
        let (icon, acp) = match s.kernel {
            KernelChoice::Acp(_) => (IconAll::Bot, true),
            _ => (IconAll::Sparkles, false),
        };
        if s.has_user_message() || s.busy {
            // 已开始：只读标识。自研内核是默认，不额外标注
            if !acp {
                return None;
            }
            let tip = format!("当前会话绑定在「{label}」内核");
            return Some(
                div()
                    .id("composer-kernel-badge")
                    .h_flex()
                    .items_center()
                    .gap_1p5()
                    // agent 名长度不由我们定：允许压窄、末尾省略，别把发送键挤出卡片
                    .flex_shrink_1()
                    .min_w(rems(3.5))
                    .overflow_hidden()
                    .px_1()
                    .py_1()
                    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                    .child(Icon::new(icon).size_3p5().flex_none().text_color(rgb(INK_SOFT)))
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(TEXT_SMALL)
                            .text_color(rgb(INK_SOFT))
                            .child(label),
                    )
                    .into_any_element(),
            );
        }
        // 只有一个可用内核时没必要选（自研内核没配好不算候选）
        let builtin = if self.settings.builtin.is_configured() { 1 } else { 0 };
        let count = builtin + self.settings.acp.agents.len();
        if count < 2 {
            return None;
        }
        let app = cx.entity();
        Some(
            menu_popover(
                "composer-kernel-menu",
                Anchor::BottomLeft,
                rems(14.),
                &app,
                &self.menu_focus,
                MenuChip::new(icon, label).id("composer-kernel-chip").shrink(),
                kernel_menu_items,
            )
            .default_open(
                cfg!(feature = "headless-screenshot")
                    && std::env::var("CLAUDESKTOP_OPEN").as_deref() == Ok("composer-kernel-menu"),
            )
            .into_any_element(),
        )
    }

    /// 输入框顶盖：空 Code 会话里选项目、切分支；开始对话后整条收起。
    fn render_context_bar(&self, cx: &mut Context<Self>) -> Div {
        let app = cx.entity();
        let s = self.session();
        let task = s.task;
        let (icon, label) = if task {
            (IconAll::ListTodo, SharedString::from("任务"))
        } else {
            (IconAll::Folder, project_label(s.group_dir()))
        };
        let project = menu_popover(
            "composer-project-menu",
            Anchor::BottomLeft,
            rems(18.),
            &app,
            &self.menu_focus,
            MenuChip::new(icon, label).id("composer-project-chip").on_sand().shrink(),
            project_menu_items,
        )
        .default_open(
            cfg!(feature = "headless-screenshot")
                && std::env::var("CLAUDESKTOP_OPEN").as_deref() == Ok("project-menu"),
        );

        let branch = if task {
            None
        } else if let Some(branch) = &s.worktree_busy {
            Some(
                div()
                    .h_flex()
                    .flex_shrink_1()
                    .min_w_0()
                    .overflow_hidden()
                    .items_center()
                    .gap_1p5()
                    .py_1()
                    .px_1()
                    .text_size(TEXT_SMALL)
                    .text_color(rgb(INK_SOFT))
                    .child(super::motion::spinner_in_place())
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(format!("正在建 worktree {branch}…")),
                    )
                    .into_any_element(),
            )
        } else if let Some(meta) = &s.worktree {
            let refresh = app.clone();
            Some(
                menu_popover(
                    "composer-worktree-menu",
                    Anchor::BottomLeft,
                    rems(21.),
                    &app,
                    &self.menu_focus,
                    MenuChip::new(IconAll::GitBranch, meta.branch.clone())
                        .id("composer-branch-chip")
                        .on_sand()
                        .mono()
                        .pill()
                        .shrink(),
                    ChatApp::worktree_menu_items,
                )
                .on_open_change(move |open, _, cx| {
                    if *open {
                        let active = refresh.read(cx).active;
                        refresh.update(cx, |app, cx| app.refresh_worktree(active, cx));
                    }
                })
                .into_any_element(),
            )
        } else if let Some(name) = &s.git_branch {
            let detached = s.git_detached;
            let refresh = app.clone();
            let chip = menu_popover(
                "composer-branch-menu",
                Anchor::BottomLeft,
                rems(16.),
                &app,
                &self.menu_focus,
                MenuChip::new(IconAll::GitBranch, name.clone())
                    .id("composer-branch-chip")
                    .on_sand()
                    .mono()
                    .pill()
                    .shrink(),
                branch_menu_items,
            )
            .on_open_change(move |open, _, cx| {
                if *open {
                    refresh.update(cx, |app, cx| app.fetch_branches_for_menu(cx));
                }
            })
            .default_open(
                cfg!(feature = "headless-screenshot")
                    && std::env::var("CLAUDESKTOP_OPEN").as_deref() == Ok("branch-menu"),
            )
            .into_any_element();
            Some(if detached {
                div()
                    .id("composer-detached-branch")
                    .min_w_0()
                    .flex_shrink_1()
                    .tooltip(|window, cx| {
                        Tooltip::new("当前是游离 HEAD：不在任何分支上").build(window, cx)
                    })
                    .child(chip)
                    .into_any_element()
            } else {
                chip
            })
        } else {
            None
        };

        div()
            .h_flex()
            .items_center()
            .gap_1p5()
            .min_w_0()
            .mx(COMPOSER_RADIUS)
            .px_2p5()
            .py_1()
            // 由后画的输入卡片盖住这 1px，上沿只留中间一条接缝
            .mb(px(-1.))
            .rounded_tl(COMPOSER_INNER_RADIUS)
            .rounded_tr(COMPOSER_INNER_RADIUS)
            .bg(rgb(SIDEBAR))
            .border_t_1()
            .border_l_1()
            .border_r_1()
            .border_color(rgb(LINE))
            .when(!task, |d| {
                d.child(
                    div()
                        .size(px(5.))
                        .flex_shrink_0()
                        .rounded_full()
                        .bg(rgb(if s.worktree.is_some() { GREEN } else { CORAL })),
                )
            })
            .child(project)
            .when_some(branch, |d, branch| {
                d.child(
                    div()
                        .flex_shrink_0()
                        .text_size(TEXT_SMALL)
                        .text_color(rgb(INK_FAINT))
                        .child("/"),
                )
                .child(branch)
            })
            .child(div().flex_1())
            .child(Self::render_grip_dots())
    }

    /// 顶盖右端静态微点阵（3×2），按容器中心排布，不请求动画帧。
    fn render_grip_dots() -> impl IntoElement {
        canvas(
            |_, _, _| {},
            |bounds, _, window, _| {
                let color = with_alpha(rgb(INK_FAINT), 0.55);
                let (radius, gap) = (px(1.1), px(4.));
                let span = point(gap * 2., gap);
                let start = bounds.center() - span * 0.5;
                for col in 0..3 {
                    for row in 0..2 {
                        super::motion::paint_dot(
                            start + point(gap * col as f32, gap * row as f32),
                            radius,
                            color,
                            window,
                        );
                    }
                }
            },
        )
        .flex_shrink_0()
        .w(rems(0.75))
        .h(rems(0.5))
    }

    pub(super) fn render_composer(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let app = cx.entity();
        let busy = self.session().busy;
        let mode_name: SharedString = self
            .session()
            .modes
            .get(self.session().permission_mode)
            .map(|m| m.name.clone())
            .unwrap_or_else(|| "—".into());
        // 焦点几乎总在输入框上（丢了由 restore_focus 找回），所以窗口不在前台时按没聚焦画：
        // 切去别的程序后输入框回到静止样子，鼠标移上来是悬停态，点回来才是聚焦态
        let focused =
            window.is_window_active() && self.input.read(cx).focus_handle(cx).is_focused(window);

        // 发送按钮三态：空（淡、不可点）/ 可发送（黑底白箭头，Kimi 风格）/ 忙（深色停止方块，同 Esc）
        // 还有图在后台处理时也按「不可点」画
        let can_send = (!self.input.read(cx).value().trim().is_empty()
            || !self.pending.is_empty()
            || !self.pending_files.is_empty())
            && !self.pending.iter().any(|p| p.preparing);
        let send_bg = match (busy, can_send) {
            (true, _) => INK,
            (false, true) => SEND_BG,
            (false, false) => SAND,
        };
        let send = div()
            .id("send")
            .size(px(30.))
            .flex_shrink_0()
            .rounded_full()
            .bg(rgb(send_bg))
            .flex()
            .justify_center()
            .items_center()
            .when(busy || can_send, |d| {
                d.cursor_pointer().hover(|d| d.bg(rgb(if busy { INK } else { SEND_BG_HOVER })))
            })
            .tooltip(move |window, cx| {
                Tooltip::new(match (busy, can_send) {
                    (true, _) => "停止（Esc）；回车把输入排进队列",
                    (false, true) => "发送（回车）",
                    (false, false) => "先输入内容或附上图片",
                })
                .build(window, cx)
            })
            .child(if busy {
                // 停止方块取纸色：浅色下是深底白块，深色下是浅底深块
                div().size(px(9.)).rounded_xs().bg(rgb(SEND_FG)).into_any_element()
            } else {
                Icon::new(IconAll::ArrowUp)
                    .size_4()
                    .text_color(rgb(if can_send { SEND_FG } else { INK_FAINT }))
                    .into_any_element()
            })
            .on_click(cx.listener(|this, _, window, cx| {
                if this.session().busy {
                    this.stop(window, cx);
                } else {
                    // 空输入时 send 自己会忽略
                    this.send(window, cx);
                }
            }));

        // 卡片底边一条工具栏：左侧权限模式，右侧上下文用量 / 模型 / 发送。
        // 和输入框是一整块（对照 Claude.ai 的输入卡片），chip 的 open 状态由 Popover 自管。
        // 中间的空白点了也把焦点给输入框（chip 和按钮自己处理点击，不挂这个）。
        // 聊天空间不画权限模式，换成「联网」开关
        let chat = self.session().is_chat();
        // 没有权限模式（还没连上的 ACP 会话、把思考强度报在 `modes` 里的 agent）就不画空的盾牌
        let has_modes = !self.session().modes.is_empty();
        let unviewable = self.pending_images_unviewable();
        let toolbar = div()
            .h_flex()
            .items_center()
            .gap_0p5()
            .when(!chat && has_modes, |d| {
                d.child(
                    menu_popover(
                        "mode-menu",
                        Anchor::BottomLeft,
                        rems(10.5),
                        &app,
                        &self.menu_focus,
                        MenuChip::new(IconAll::Shield, mode_name),
                        mode_menu_items,
                    )
                    .p_0p5()
                    .default_open(
                        cfg!(feature = "headless-screenshot")
                            && std::env::var("CLAUDESKTOP_OPEN").as_deref()
                                == Ok("history-mode-menu"),
                    ),
                )
            })
            .when(chat, |d| d.child(self.render_web_chip(cx)))
            .children(self.render_kernel_chip(cx))
            .child(
                Self::quiet_button("attach-file", IconAll::Plus, Surface::Paper, cx)
                    .small()
                    .tooltip(if chat {
                        "添加图片或文本文件"
                    } else {
                        "添加图片或 @ 引用文件"
                    })
                    .on_click(cx.listener(|this, _, window, cx| this.pick_attachment(window, cx))),
            )
            .child(div().flex_1().self_stretch().on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| this.focus_input(window, cx)),
            ))
            .children(self.render_context_gauge())
            // 模型名（尤其带端点标签的）比别的 chip 长得多：允许压窄、末尾打省略号
            .child(self.render_model_picker(cx))
            // 发送键不参与收缩：一行排不下时先让 chip 让路，它永远在最右边
            .child(div().pl_1().flex_shrink_0().child(send));

        // 一张卡片：附件缩略图、随内容长高的多行输入（至少两行），底边是工具栏。
        // 静止：极淡细边，无阴影；悬停边框加深一档（提示整块都能点）。
        // 聚焦：边框保持 `LINE_STRONG`，外面只套一层淡强调色光晕（`composer_focus_shadow`）。
        // 卡片与内部附件使用同心圆角；鼠标点进来也出聚焦态。
        let card = div()
            .id("composer")
            .v_flex()
            .w_full()
            .bg(rgb(CARD))
            .border_1()
            .border_color(rgb(if focused { LINE_STRONG } else { COMPOSER_LINE }))
            .when(!focused, |d| d.hover(|d| d.border_color(rgb(LINE_STRONG))))
            .rounded(COMPOSER_RADIUS)
            // 兜底：外面的 chip / 按钮都允许压窄，正常排不下时先缩它们；万一还有排不下的
            // （极端窄 + 超大字号），按卡片边界裁掉，别让按钮飘到卡片外面。
            // 弹层是 deferred 画的（不套这里的 content mask），菜单照常露出卡片
            .overflow_hidden()
            .shadow(if focused { composer_focus_shadow() } else { Vec::new() })
            // 发送键离右边和底边都是 8px
            .px_2()
            .pt_1p5()
            .pb_2()
            // 待发送附件：图片缩略图 + 文本文件 chip（点右上角 × 撤掉），左边和输入文字对齐
            .when(!self.pending.is_empty() || !self.pending_files.is_empty(), |d| {
                d.child(
                    div()
                        .h_flex()
                        .flex_wrap()
                        .gap_1p5()
                        .px_2()
                        .pt_1()
                        .pb_1()
                        .children((0..self.pending.len()).map(|i| self.pending_image_thumb(i, cx)))
                        .children(
                            (0..self.pending_files.len()).map(|i| self.pending_file_chip(i, cx)),
                        ),
                )
            })
            // 当前模型看不了图：说在前面，图留在输入框里（换模型后再发）
            .when_some(unviewable, |d, why| {
                d.child(div().px_2().pb_1().child(super::settings_kit::note(IconAll::Info, why)))
            })
            .child(
                div()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| this.focus_input(window, cx)),
                    )
                    .child(
                        div()
                            .relative()
                            .w_full()
                            .child(
                                Textarea::new(&self.input)
                                    .appearance(false)
                                    .bordered(false)
                                    .on_paste(paste_to_attachments(app.clone())),
                            )
                            // 光标后面那句补全灰字（见 suggest.rs）：跟着光标画，只重画输入区。
                            // 这里只看输入框有没有焦点，不看窗口在不在前台——灰字是内容，
                            // 切走再切回来不该闪一下
                            .children(
                                self.input
                                    .read(cx)
                                    .focus_handle(cx)
                                    .is_focused(window)
                                    .then(|| self.suggested_tail(cx))
                                    .flatten()
                                    .map(|tail| self.suggestion_ghost(tail)),
                            )
                            // 一轮结束后猜的那句（见 suggest.rs）：输入框空着时用淡色画在里面，
                            // 按 Tab 填入。占位文字已经让开了，这里画的就是顶格的第一行
                            .children(
                                self.next_suggestion_text(cx)
                                    .map(|text| self.next_suggestion_preview(text)),
                            ),
                    ),
            )
            .child(toolbar);

        let open = self.session().show_context_bar();
        let reveal = super::motion::reveal_progress(
            ("composer-context-bar", self.session().token),
            open,
            window,
            cx,
        );
        // 展开态也挂着 Reveal：首帧量好高度，开始对话收起时不会先塌一下
        let composer_stack = div()
            .v_flex()
            .w_full()
            .when(open || reveal > 0.001, |d| {
                d.child(super::motion::Reveal::new(
                    ("composer-context-bar", self.session().token),
                    reveal,
                    self.render_context_bar(cx),
                ))
            })
            // 卡片外面叠一层棱镜描边：不接事件、不参与布局，只在指针靠近时亮起来
            // （见 prism_rim.rs；它自己一个 view，鼠标移动不会让 ChatApp 重排）
            .child(self.with_completion_keys(
                div()
                    .id("composer-rim-host")
                    .relative()
                    .w_full()
                    .child(card)
                    .child(self.rim_view.clone()),
                cx,
            ));

        div()
            .v_flex()
            .w_full()
            .gap_2()
            // 进行中这一轮的计划钉在最上面（对话往下走它也一直看得见）
            .children(self.render_pinned_plan(cx))
            .child(
                div()
                    .v_flex()
                    .gap_1()
                    .children(self.render_context_warning(cx))
                    .children(self.render_queue(cx))
                    // `/`、`@` 补全列表浮在卡片正上方，盖住对话而不是把输入框往下挤。
                    // 量一次卡片上沿（`composer_top`），补全面板按它限高，不会顶出窗口
                    .child(
                        div()
                            .relative()
                            .child(composer_stack)
                            .child(self.composer_top.probe())
                            // 提问面板（`ask_user`）贴着输入框上沿：一次问一题，选完自动交给模型
                            .children(self.render_question_panel(window, cx).map(|panel| {
                                div()
                                    .absolute()
                                    .left_0()
                                    .right_0()
                                    .bottom_full()
                                    .mb_1p5()
                                    .child(panel)
                            }))
                            .children(self.render_completion(window, cx).map(|list| {
                                div()
                                    .absolute()
                                    .left_0()
                                    .right_0()
                                    .bottom_full()
                                    .mb_1p5()
                                    .child(list)
                            })),
                    ),
            )
    }

    pub(crate) fn authenticate_agent(&mut self, method_id: String, cx: &mut Context<Self>) {
        let active = self.active;
        if self.sessions[active].auth.as_ref().is_none_or(|auth| auth.busy) {
            return;
        }
        let Some(kernel) = self.session_kernel(active) else {
            if let Some(auth) = &mut self.sessions[active].auth {
                auth.message = "当前内核不可用，无法登录".into();
            }
            cx.notify();
            return;
        };
        match kernel.authenticate(&method_id) {
            Ok(()) => {
                if let Some(auth) = &mut self.sessions[active].auth {
                    auth.busy = true;
                }
            }
            Err(err) => {
                if let Some(auth) = &mut self.sessions[active].auth {
                    auth.message = format!("登录失败：{err}").into();
                }
            }
        }
        cx.notify();
    }

    fn render_auth_panel(&self, cx: &mut Context<Self>) -> Div {
        let auth = self.session().auth.as_ref().expect("登录面板状态");
        let mut methods = div().h_flex().gap_2().flex_wrap();
        for (i, method) in auth.methods.iter().enumerate() {
            let id = method.id.clone();
            methods = methods.child(
                Self::text_button(("auth-method", i), method.name.clone(), i == 0)
                    .small()
                    .loading(auth.busy)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.authenticate_agent(id.clone(), cx);
                    })),
            );
        }
        div()
            .v_flex()
            .gap_2()
            .p_3()
            .rounded_xl()
            .border_1()
            .border_color(rgb(LINE))
            .bg(rgb(CARD))
            .shadow(float_shadow())
            .child(
                div()
                    .h_flex()
                    .gap_2()
                    .items_center()
                    .child(Icon::new(IconAll::Shield).size_4().text_color(rgb(CORAL)))
                    .child(div().text_size(TEXT_BODY).text_color(rgb(INK)).child("需要登录")),
            )
            .child(
                div().text_size(TEXT_SMALL).text_color(rgb(INK_SOFT)).child(auth.message.clone()),
            )
            .child(methods)
            .child(
                div()
                    .text_size(TEXT_META)
                    .text_color(rgb(INK_SOFT))
                    .child("登录由 agent 完成，claudesktop 不读取凭据"),
            )
    }

    /// 输入框正上方的提问面板（`ask_user`）：一次只问一题，点一个选项就走到下一题，
    /// 最后一题选完直接交给模型——所以没有「就这样」那一排按钮。
    /// 和补全面板一样按输入框上沿量到的位置限高，窗口矮 / 字号大时在面板里滚。
    fn render_question_panel(&self, window: &Window, cx: &mut Context<Self>) -> Option<Div> {
        if self.session().auth.is_some() {
            return Some(self.render_auth_panel(cx));
        }
        let item_idx = self.session().pending_question()?;
        let crate::model::ChatItem::Question { questions, picks, multi_picks, .. } =
            &self.session().transcript[item_idx]
        else {
            return None;
        };
        let cursor = self.session().question_cursor(item_idx)?;
        let question = questions.get(cursor)?;
        let multi = question.multi_select;
        let freeform = matches!(
            question.kind,
            claudesktop_core::QuestionKind::Text
                | claudesktop_core::QuestionKind::Number
                | claudesktop_core::QuestionKind::Integer
        );
        let total = questions.len();
        let last = cursor + 1 >= total;
        let is_form = questions.iter().any(|question| question.id.is_some());
        let rem = window.rem_size();
        let max_h = self.composer_top.get().map(|top| {
            let gap = rems(1.).to_pixels(rem);
            let chrome = CHROME_H.to_pixels(rem);
            let above = rems(0.375).to_pixels(rem);
            (top - chrome - gap - above).max(rems(6.).to_pixels(rem))
        });

        let header = div()
            .h_flex()
            .items_center()
            .gap_2()
            .px_2p5()
            .pt_1()
            .pb_0p5()
            .child(Icon::new(IconAll::MessageSquare).size_4().text_color(rgb(CORAL)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(TEXT_META)
                    .text_color(rgb(INK_SOFT))
                    .child(question.header.clone().unwrap_or_else(|| "需要你定一下".to_string())),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(TEXT_META)
                    .text_color(rgb(INK_SOFT))
                    .font_features(tabular_nums())
                    .child(format!("第 {}/{} 题", cursor + 1, total)),
            )
            .when(cursor > 0, |d| {
                d.child(
                    div()
                        .id("question-back")
                        .flex_shrink_0()
                        .px_1p5()
                        .py_0p5()
                        .rounded_md()
                        .cursor_pointer()
                        .text_size(TEXT_META)
                        .text_color(rgb(INK_SOFT))
                        .hover(|d| d.bg(rgb(HOVER)).text_color(rgb(INK)))
                        .child("上一题")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.back_question(item_idx, cx);
                        })),
                )
            });

        let prompt = div()
            .px_2p5()
            .pb_1()
            .text_size(TEXT_BODY)
            .text_color(rgb(INK))
            .child(question.question.clone());

        let mut rows = div().id("question-rows").v_flex().gap_0p5().min_h_0().px_1();
        // 条目多了才开滚动区（限高是从输入框上沿倒推的）
        if let Some(h) = max_h {
            rows = rows.max_h(h).overflow_y_scroll();
        }
        for (o, option) in question.options.iter().enumerate() {
            let key = super::keys::PERMISSION_KEYS.get(o).copied();
            let selected = if multi {
                multi_picks.get(cursor).is_some_and(|selected| selected.contains(&o))
            } else {
                picks.get(cursor) == Some(&Some(o))
            };
            rows =
                rows.child(
                    div()
                        .id(("question-option", o))
                        .h_flex()
                        .items_center()
                        .gap_2()
                        .px_2p5()
                        .py_1p5()
                        .rounded_md()
                        .cursor_pointer()
                        .hover(|d| d.bg(rgb(HOVER)))
                        .when_some(key.map(Self::key_text), |d, key| d.aria_keyshortcuts(key))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.pick_question(item_idx, cursor, o, cx);
                        }))
                        .child(
                            Icon::new(if selected { IconAll::Check } else { IconAll::Circle })
                                .size_3p5()
                                .flex_shrink_0()
                                .text_color(rgb(if selected { CORAL } else { INK_FAINT })),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .v_flex()
                                .gap_0p5()
                                .child(
                                    div()
                                        .text_size(TEXT_BODY)
                                        .text_color(rgb(INK))
                                        .child(option.label.clone()),
                                )
                                .children(option.description.clone().map(|d| {
                                    div().text_size(TEXT_META).text_color(rgb(INK_SOFT)).child(d)
                                })),
                        )
                        .children(key.map(|key| {
                            div().flex_shrink_0().child(Self::kbd(key).appearance(false))
                        })),
                );
        }

        // 底边一句提示 + 两个退路（不再是一排按钮）
        let link = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .flex_shrink_0()
                .rounded_md()
                .px_1()
                .cursor_pointer()
                .hover(|d| d.bg(rgb(HOVER)).text_color(rgb(INK)))
                .child(label)
        };
        let footer = div()
            .h_flex()
            .items_center()
            .gap_3()
            .px_2p5()
            .pt_1()
            .pb_1p5()
            .text_size(TEXT_META)
            .text_color(rgb(INK_SOFT))
            .child(div().flex_1().min_w_0().child(if freeform {
                "在下方输入回答，回车提交"
            } else if multi {
                "选完点提交"
            } else if last {
                "选一个就交给模型"
            } else {
                "选一个继续下一题"
            }))
            .when(multi || (is_form && !freeform && last), |d| {
                d.child(link("question-submit", if last { "提交" } else { "继续" }).on_click(
                    cx.listener(move |this, _, _, cx| this.submit_question(item_idx, cx)),
                ))
            })
            .child(link("question-self", "自己说").on_click(cx.listener(
                move |this, _, window, cx| {
                    this.self_answer_question(item_idx, window, cx);
                },
            )))
            .child(link("question-model", "你来定").on_click(cx.listener(
                move |this, _, _, cx| {
                    this.answer_question(
                        item_idx,
                        claudesktop_core::QuestionAnswer::ModelDecides,
                        cx,
                    );
                },
            )));

        Some(
            div()
                .v_flex()
                .min_h_0()
                .gap_0p5()
                .pt_0p5()
                .rounded_xl()
                .border_1()
                .border_color(rgb(LINE))
                .bg(rgb(CARD))
                .shadow(float_shadow())
                .child(header)
                .child(prompt)
                .child(rows)
                .child(footer),
        )
    }
}

impl ChatApp {
    /// 上下文用到九成以上：输入框正上方一行提示，建议新开会话（再发很可能超出上限失败）。
    /// 正在跑的一轮不提示，等它结束再说。
    fn render_context_warning(&self, cx: &mut Context<Self>) -> Option<Div> {
        // 自研内核过八成就会在下一次请求前自动压缩（见 claudesktop-builtin 的 compact 模块），
        // 这里提前说一声，并给「现在压缩」「新开会话」两条路
        let percent = self.context_gauge()?.percent.filter(|p| *p >= 80)?;
        if self.session().busy {
            return None;
        }
        let action = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .flex_shrink_0()
                .px_2()
                .py_0p5()
                .rounded_md()
                .cursor_pointer()
                .text_color(rgb(INK))
                .hover(|d| d.bg(rgb(SELECTED)))
                .child(label)
        };
        Some(
            div()
                .h_flex()
                .items_center()
                .gap_2()
                .pl_3()
                .pr_1p5()
                .py_1()
                .rounded_lg()
                .bg(rgb(SAND))
                .text_size(TEXT_SMALL)
                .text_color(rgb(INK_SOFT))
                .child(Icon::new(IconAll::TriangleAlert).size_3p5().text_color(rgb(RED)))
                .child(div().flex_1().min_w_0().font_features(tabular_nums()).child(format!(
                    "上下文已用 {percent}%，下一条消息发出前会先把较早的对话压缩成摘要"
                )))
                .child(
                    action("context-compact", "现在压缩")
                        .on_click(cx.listener(|this, _, _, cx| this.compact_session(cx))),
                )
                .child(action("context-new-session", "新开会话").on_click(cx.listener(
                    |this, _, window, cx| {
                        this.add_session(cx);
                        this.focus_input(window, cx);
                    },
                ))),
        )
    }

    /// 输入框正上方的排队消息（`Session::queued`）：一条一行，可以退回输入框改、删掉；
    /// 上一轮没有正常结束（停了 / 出错）时队列不自己走，每行多一个「现在发送」。
    /// 排着的「压缩上下文」（`Session::compact_queued`）也占一行，最先发出（压完再发消息）。
    fn render_queue(&self, cx: &mut Context<Self>) -> Option<Div> {
        let s = self.session();
        let compact = s.compact_queued;
        if s.queued.is_empty() && !compact {
            return None;
        }
        let count = s.queued.len() + usize::from(compact);
        let busy = s.busy;
        // 自研内核支持运行中插话（ACP agent 由对方决定何时读输入，不提供这个入口）
        let can_steer = busy && s.kernel == crate::settings::KernelChoice::Builtin;
        let hint = if busy {
            format!("排队 {count} 条 · 这一轮结束后依次发出")
        } else if s.stopping {
            format!("排队 {count} 条 · 停稳后发出")
        } else {
            format!("排队 {count} 条 · 上一轮没有正常结束，先不自动发")
        };
        let mut card = div()
            .v_flex()
            .gap_0p5()
            .pl_3()
            .pr_1()
            .py_1()
            .rounded_lg()
            .bg(rgb(SAND))
            .text_size(TEXT_SMALL)
            .text_color(rgb(INK_SOFT))
            .child(div().py_0p5().font_features(tabular_nums()).child(hint));
        if compact {
            let label = if s.queued.is_empty() {
                "压缩上下文 · 这一轮结束后开始"
            } else {
                "压缩上下文 · 先压再发下面的消息"
            };
            card = card.child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_1()
                    .child(Icon::new(IconAll::ListCollapse).size_3p5().flex_shrink_0())
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_color(rgb(INK))
                            .child(label),
                    )
                    .child(
                        Self::quiet_button("queue-compact-remove", IconAll::X, Surface::Sand, cx)
                            .xsmall()
                            .tooltip("不压了")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.sessions[this.active].compact_queued = false;
                                cx.notify();
                            })),
                    ),
            );
        }
        for (i, item) in s.queued.iter().enumerate() {
            let text: SharedString = item.summary().into();
            card = card.child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_1()
                    .child(Icon::new(IconAll::CornerDownRight).size_3p5().flex_shrink_0())
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_color(rgb(INK))
                            .child(text),
                    )
                    .when(can_steer, |d| {
                        d.child(
                            Self::quiet_button(
                                ("queue-steer", i),
                                IconAll::MessageSquarePlus,
                                Surface::Sand,
                                cx,
                            )
                            .xsmall()
                            .tooltip("插话：下一步就带上它")
                            .on_click(cx.listener(move |this, _, _, cx| this.steer_queued(i, cx))),
                        )
                    })
                    .when(!busy, |d| {
                        d.child(
                            Self::quiet_button(
                                ("queue-send", i),
                                IconAll::ArrowUp,
                                Surface::Sand,
                                cx,
                            )
                            .xsmall()
                            .tooltip("现在发送")
                            .on_click(cx.listener(move |this, _, _, cx| this.send_queued(i, cx))),
                        )
                    })
                    .child(
                        Self::quiet_button(("queue-edit", i), IconAll::Pencil, Surface::Sand, cx)
                            .xsmall()
                            .tooltip("退回输入框修改")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.unqueue(Some(i), window, cx)
                            })),
                    )
                    .child(
                        Self::quiet_button(("queue-remove", i), IconAll::X, Surface::Sand, cx)
                            .xsmall()
                            .tooltip("不发了")
                            .on_click(cx.listener(move |this, _, _, cx| this.remove_queued(i, cx))),
                    ),
            );
        }
        Some(card)
    }
}

impl ChatApp {
    /// 会话区接系统拖进来的文件：拖到上方时盖一层提示，松开后图片收成附件、其余插入 `@引用`。
    pub(super) fn with_file_drop(&mut self, area: Div, cx: &mut Context<Self>) -> Div {
        // 拖出窗口或取消时系统会清掉进行中的拖拽，提示跟着收起
        if !cx.has_active_drag() {
            self.file_drag = false;
        }
        area.on_drag_move(cx.listener(|this, e: &DragMoveEvent<ExternalPaths>, _, cx| {
            let over = e.bounds.contains(&e.event.position);
            if this.file_drag != over {
                this.file_drag = over;
                cx.notify();
            }
        }))
        .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
            this.drop_paths(paths.paths(), window, cx)
        }))
        .when(self.file_drag, |d| {
            let chat = self.session().is_chat();
            d.child(Self::render_drop_hint(chat))
        })
    }

    pub(crate) fn drop_paths(
        &mut self,
        paths: &[std::path::PathBuf],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.file_drag = false;
        let cwd = self.session().cwd.clone();
        let chat = self.session().is_chat();
        let mut images = Vec::new();
        let mut mentions = Vec::new();
        for path in paths {
            match PendingImage::from_path(path) {
                Some(image) => images.push(image),
                // 聊天没有 read_file，@ 引用没用：文本直接内联进来
                None if chat => {
                    let one = [path.clone()];
                    self.add_pending_files(&one, cx);
                }
                None => mentions.push(crate::complete::mention_for(path, &cwd)),
            }
        }
        self.add_pending_images(images, cx);
        if !mentions.is_empty() {
            // 插在光标处，和前后文字各隔一个空格
            self.input.update(cx, |state, cx| {
                let value = state.value();
                let spaced = value[..state.cursor().min(value.len())]
                    .chars()
                    .next_back()
                    .is_none_or(char::is_whitespace);
                let text = mentions.join(" ");
                state.insert(
                    if spaced { format!("{text} ") } else { format!(" {text} ") },
                    window,
                    cx,
                );
            });
        }
        self.focus_input(window, cx);
        cx.notify();
    }

    /// 输入框工具栏「+」：系统文件对话框选图片或文件，复用 `drop_paths` 的添加逻辑。
    pub(crate) fn pick_attachment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("选择图片或文件".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await
                && !paths.is_empty()
            {
                let _ = this.update_in(cx, |app, window, cx| app.drop_paths(&paths, window, cx));
            }
        })
        .detach();
    }

    /// 拖拽中盖在会话区上的提示（不挡事件：松开落在下面的会话区上）。
    /// 框用原版 `--cds-shadow-drop-ring` + `--cds-shadow-drop-glow` 的两层：
    /// 2px 赤陶实圈（`inset`）+ 12px（σ，CSS 24px 减半）赤陶 20% 柔光，比一圈虚线更像「这里能放」。
    fn render_drop_hint(chat: bool) -> Div {
        let mut scrim = rgb(PAPER);
        scrim.a = 0.94;
        let accent = rgb(CORAL);
        let ring = BoxShadow::new(px(0.), px(0.), accent.into()).spread_radius(px(2.)).inset();
        let glow = BoxShadow::new(px(0.), px(0.), with_alpha(accent, 0.2).into())
            .blur_radius(px(12.))
            .spread_radius(px(-2.))
            .inset();
        div().absolute().inset_0().p_4().bg(scrim).child(
            div()
                .size_full()
                .v_flex()
                .items_center()
                .justify_center()
                .gap_1p5()
                .rounded_2xl()
                .shadow(vec![ring, glow])
                .child(Icon::new(IconAll::Plus).size_6().text_color(rgb(CORAL)))
                .child(div().text_size(TEXT_LEAD).font_medium().child("松开即可添加"))
                .child(div().text_size(TEXT_SMALL).text_color(rgb(INK_SOFT)).child(if chat {
                    "图片作为附件，文本文件的内容一起发过去"
                } else {
                    "图片作为附件，其他文件和文件夹插入 @ 引用"
                })),
        )
    }
}

/// 粘贴处理：剪贴板里是图片（截图）或图片文件路径时收成待发送附件并吃掉这次粘贴；
/// 纯文字返回 false，交给输入框照常插入。
fn paste_to_attachments(
    app: Entity<ChatApp>,
) -> impl Fn(&gpui::ClipboardItem, &mut Window, &mut App) -> bool {
    move |item, _window, cx| {
        let mut images = Vec::new();
        for entry in item.entries() {
            match entry {
                ClipboardEntry::Image(image) => images.push(PendingImage::from_bytes(
                    "剪贴板图片".into(),
                    image.bytes.clone(),
                    image.format,
                )),
                // 从资源管理器复制的文件：认得出图片扩展名的收下
                ClipboardEntry::ExternalPaths(paths) => {
                    for path in paths.0.iter() {
                        if let Some(image) = PendingImage::from_path(path) {
                            images.push(image);
                        }
                    }
                }
                _ => {}
            }
        }
        if images.is_empty() {
            return false;
        }
        app.update(cx, |app, cx| app.add_pending_images(images, cx));
        true
    }
}

// ---------- 菜单条目（在 Popover 面板里渲染） ----------

/// 顶盖的项目菜单：最近项目、改成任务、选择文件夹。
fn project_menu_items(
    app: &ChatApp,
    handle: &Entity<ChatApp>,
    popover: &Entity<PopoverState>,
) -> MenuList {
    let session = app.session();
    let current = session.group_dir();
    let mut menu = MenuList::new("composer-project-list", handle, popover)
        .without_shortcuts()
        .compact()
        .scroll_with_bar(rems(18.));
    for (i, dir) in app.recent_workspaces().into_iter().enumerate() {
        let active = !session.task && dir == current;
        let path = short_path(&dir);
        menu = menu.pick(
            project_menu_item(
                ("composer-project-item", i),
                IconAll::Folder,
                project_label(&dir),
                active,
            )
            .tooltip(move |window, cx| super::chat::text_tip(&path, window, cx)),
            move |this, cx| this.switch_workspace(dir.clone(), cx),
        );
    }
    menu = menu.child(div().my_0p5().border_t_1().border_color(rgb(LINE)));
    menu.pick(
        project_menu_item(
            "composer-project-task",
            IconAll::ListTodo,
            "不选项目（任务）",
            session.task,
        ),
        |this, cx| this.as_task(cx),
    )
    .pick(
        project_menu_item("composer-project-pick", IconAll::FolderOpen, "选择文件夹…", false),
        |this, cx| this.pick_workspace(cx),
    )
}

fn project_menu_item(
    id: impl Into<ElementId>,
    icon: IconAll,
    name: impl Into<SharedString>,
    active: bool,
) -> Stateful<Div> {
    menu_item(id, icon, name, "", active)
        .h(rems(1.75))
        .gap_1p5()
        .px_2()
        .py_0()
        .line_height(relative(1.25))
        .text_size(TEXT_SMALL)
}

/// 顶盖的分支菜单：列本地分支（最近提交在前），底部保留复制与新建 worktree。
fn branch_menu_items(
    app: &ChatApp,
    handle: &Entity<ChatApp>,
    popover: &Entity<PopoverState>,
) -> MenuList {
    let session = app.session();
    let current = session.git_branch.clone();
    let mut menu = MenuList::new("composer-branch-list", handle, popover)
        .without_shortcuts()
        .compact()
        .scroll_with_bar(rems(16.));
    if app.git_branches.is_empty() {
        menu = menu.child(
            div().px_2p5().py_2().text_size(TEXT_SMALL).text_color(rgb(INK_SOFT)).child(
                if app.git_branches_loading {
                    "正在读取分支…"
                } else {
                    "没有本地分支"
                },
            ),
        );
    }
    for (i, name) in app.git_branches.iter().enumerate() {
        let active = current.as_deref() == Some(name.as_str());
        let target = name.clone();
        menu = menu.pick(
            menu_item(("composer-branch-item", i), IconAll::GitBranch, name.clone(), "", active)
                .h(rems(1.75))
                .gap_1p5()
                .px_2()
                .py_0()
                .line_height(relative(1.25))
                .text_size(TEXT_SMALL),
            move |this, cx| this.switch_branch(target.clone(), cx),
        );
    }
    menu = menu.footer(div().border_t_1().border_color(rgb(LINE)));
    menu = menu.footer_pick(
        project_menu_item("composer-branch-worktree", IconAll::GitBranch, "新建 worktree", false),
        |this, cx| this.start_in_worktree(cx),
    );
    if let Some(name) = current {
        let copy = name.to_string();
        menu = menu.footer_pick(
            project_menu_item("composer-branch-copy", IconAll::Copy, "复制分支名", false),
            move |this, cx| {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(copy.clone()));
                this.toast(Notification::success("分支名已复制"), cx);
            },
        );
    }
    menu
}

/// 会话当前模型的档位表：能选哪些档、字段怎么发，都以它为准
pub(crate) fn session_thinking_spec(app: &ChatApp) -> claudesktop_builtin::ThinkingSpec {
    let profile = app.settings.builtin.current();
    // 内核还没报模型（刚建、没连上）时按档案的模型查，别先显示成认不出的「默认 / 兜底」
    let model = if app.session().model_label == crate::session::UNKNOWN_MODEL {
        profile.model.as_str()
    } else {
        app.session().model_label.as_ref()
    };
    claudesktop_builtin::thinking_spec(profile.protocol, model)
}

/// 档位 → 交给内核的字符串（和设置文件里存的是同一套：`low` / `high` / `max` …）
pub(crate) fn thinking_wire(level: ThinkingLevel) -> String {
    serde_json::to_value(level)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "high".into())
}

/// 内核条目：自研内核（配好时）+ 每个启用的 agent，当前项打勾。
fn kernel_menu_items(
    app: &ChatApp,
    handle: &Entity<ChatApp>,
    popover: &Entity<PopoverState>,
) -> MenuList {
    let session = app.session();
    let mut menu =
        MenuList::new("composer-kernel-list", handle, popover).without_shortcuts().compact();
    if app.settings.builtin.is_configured() {
        let active = session.kernel == KernelChoice::Builtin;
        menu = menu.pick(
            menu_item(
                ElementId::Name("kernel-item-builtin".into()),
                IconAll::Sparkles,
                "自研内核",
                "",
                active,
            ),
            |this, cx| this.choose_session_kernel(KernelChoice::Builtin, cx),
        );
    }
    for (i, agent) in app.settings.acp.agents.iter().enumerate() {
        let kind = KernelChoice::Acp(agent.id.clone());
        let active = session.kernel == kind;
        menu = menu.pick(
            menu_item(("kernel-item", i), IconAll::Bot, agent.display_name(), "", active),
            move |this, cx| this.choose_session_kernel(kind.clone(), cx),
        );
    }
    menu
}

/// 权限模式条目：单行图标与名称，当前项打勾。
fn mode_menu_items(
    app: &ChatApp,
    handle: &Entity<ChatApp>,
    popover: &Entity<PopoverState>,
) -> MenuList {
    let session = app.session();
    let mut menu = MenuList::new("mode-menu-list", handle, popover).without_shortcuts().compact();
    for (i, mode) in session.modes.iter().enumerate() {
        let active = session.permission_mode == i;
        menu = menu.pick(
            menu_item(("mode-item", i), mode.icon, mode.name.clone(), "", active)
                .h(rems(1.5))
                .gap_1p5()
                .px_2()
                .py_0()
                .line_height(relative(1.25))
                .text_size(TEXT_SMALL),
            move |this, cx| this.choose_mode(i, cx),
        );
    }
    menu
}

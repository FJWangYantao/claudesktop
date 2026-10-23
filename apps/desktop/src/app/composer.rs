//! 底部输入区（上方环境 / 工作目录，多行输入 + 发送，下方一行权限模式 / 附件 / 模型）与导出、附件等动作。

use super::chrome::CHROME_H;
use super::menu::{MenuChip, MenuList, menu_item, menu_popover};
use super::settings_view::active_thinking;
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
use gpui_kit::component::popover::Popover;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::sync::Arc;

impl ChatApp {
    /// 「分享」：当前会话导出为 Markdown，系统保存对话框选位置。
    pub(crate) fn share_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let idx = self.active;
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

    /// 加号按钮：选文件加进待发送附件。
    /// 聊天会话把文本内容内联进消息（聊天没有 `read_file`）；代码会话还是把路径插进输入框，
    /// 等 agent 自己读。
    pub(crate) fn attach_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let chat = self.session().is_chat();
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: !chat,
            multiple: true,
            prompt: Some(
                if chat {
                    "选择要一起发过去的文件"
                } else {
                    "选择要交给 agent 的文件"
                }
                .into(),
            ),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                if paths.is_empty() {
                    return;
                }
                let _ = this.update_in(cx, |app, window, cx| {
                    if chat {
                        app.add_pending_files(&paths, cx);
                        app.focus_input(window, cx);
                        return;
                    }
                    let add = paths
                        .iter()
                        .map(|p| format!("\"{}\"", p.display()))
                        .collect::<Vec<_>>()
                        .join("\n");
                    app.input.update(cx, |state, cx| {
                        let current = state.value().to_string();
                        let next = if current.trim().is_empty() {
                            add
                        } else {
                            format!("{current}\n{add}")
                        };
                        state.set_value(next, window, cx);
                    });
                    cx.notify();
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
        let kernel = self.session_kernel(self.active);
        let s = &mut self.sessions[self.active];
        s.choose_mode_index(i);
        if let Some(mode) = s.modes.get(i) {
            match (kernel, s.acp_id.clone()) {
                (Some(kernel), Some(id)) => {
                    let _ = kernel.set_mode(&id, &mode.id);
                }
                _ => s.pending_mode = Some(mode.id.clone()),
            }
        }
        cx.notify();
    }

    /// 思考强度下拉里选了一档：只作用于**这个会话**，不动设置文件里的档案。
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
            .rounded_md()
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
            .rounded_md()
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

    pub(super) fn render_composer(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let app = cx.entity();
        let busy = self.session().busy;
        let mode_name: SharedString = self
            .session()
            .modes
            .get(self.session().permission_mode)
            .map(|m| m.name.clone())
            .unwrap_or_else(|| "—".into());
        let project = project_label(&self.session().cwd);
        // 任务（建立时没选过文件夹的对话）：chip 上直说，点开可以挂到某个文件夹上
        let task = self.session().task;
        let project_chip = if task {
            MenuChip::new(IconAll::ListTodo, "任务")
        } else if let Some(branch) = self.session().worktree.as_ref().map(|w| w.repo_name()) {
            // worktree 会话：chip 写仓库名（目录是数据目录里的 worktree，显出来没意义）
            MenuChip::new(IconAll::GitBranch, format!("{branch} · worktree"))
        } else {
            MenuChip::new(IconAll::Folder, project)
        };
        // 焦点几乎总在输入框上（丢了由 restore_focus 找回），所以窗口不在前台时按没聚焦画：
        // 切去别的程序后输入框回到静止样子，鼠标移上来是悬停态，点回来才是聚焦态
        let focused =
            window.is_window_active() && self.input.read(cx).focus_handle(cx).is_focused(window);

        // 发送按钮三态：空（淡、不可点）/ 可发送（赤陶）/ 忙（深色停止方块，同 Esc）
        // 还有图在后台处理时也按「不可点」画
        let can_send = (!self.input.read(cx).value().trim().is_empty()
            || !self.pending.is_empty()
            || !self.pending_files.is_empty())
            && !self.pending.iter().any(|p| p.preparing);
        let send_bg = match (busy, can_send) {
            (true, _) => INK,
            (false, true) => CORAL,
            (false, false) => SAND,
        };
        let send = div()
            .id("send")
            .size(px(28.))
            .flex_shrink_0()
            .rounded_lg()
            .bg(rgb(send_bg))
            .flex()
            .justify_center()
            .items_center()
            .when(busy || can_send, |d| d.cursor_pointer())
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
                div().size(px(9.)).rounded_xs().bg(rgb(PAPER)).into_any_element()
            } else {
                Icon::new(IconAll::ArrowUp)
                    .size_4()
                    // CORAL 底上的箭头用 ON_ACCENT：有的配色深色版这个底很亮，白箭头看不清
                    .text_color(rgb(if can_send { ON_ACCENT } else { INK_FAINT }))
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

        // 卡片底边一条工具栏：左侧项目目录 / 权限模式 / 添加文件，右侧上下文用量 / 模型 / 发送。
        // 和输入框是一整块（对照 Claude.ai 的输入卡片），chip 的 open 状态由 Popover 自管。
        // 中间的空白点了也把焦点给输入框（chip 和按钮自己处理点击，不挂这个）。
        // 聊天空间没有工作目录和权限模式（没有工具要批），那两个 chip 不画，换成「联网」开关
        let chat = self.session().is_chat();
        let unviewable = self.pending_images_unviewable();
        let toolbar = div()
            .h_flex()
            .items_center()
            .gap_0p5()
            .when(!chat, |d| {
                d.child(menu_popover(
                    "workspace-menu",
                    Anchor::BottomLeft,
                    rems(20.),
                    &app,
                    &self.menu_focus,
                    project_chip,
                    workspace_menu_items,
                ))
                .child(menu_popover(
                    "mode-menu",
                    Anchor::BottomLeft,
                    rems(20.),
                    &app,
                    &self.menu_focus,
                    MenuChip::new(IconAll::Shield, mode_name),
                    mode_menu_items,
                ))
            })
            .child(
                Self::quiet_button("attach-files", IconAll::Paperclip, Surface::Paper, cx)
                    .small()
                    .tooltip("添加文件")
                    .on_click(cx.listener(|this, _, window, cx| this.attach_files(window, cx))),
            )
            .when(chat, |d| d.child(self.render_web_chip(cx)))
            .child(div().flex_1().self_stretch().on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| this.focus_input(window, cx)),
            ))
            .children(self.render_context_gauge())
            .children(render_thinking_chip(self, &app))
            // 模型名可能很长（`global:deepseek-v4.1-flash`）：只有它可以收窄、截成省略号，
            // 其余按内容宽度排；不然整排撑出卡片，发送键被挤到外面
            .child(
                model_menu_popover(
                    &app,
                    &self.menu_focus,
                    "model-menu",
                    Anchor::BottomRight,
                    MenuChip::plain(self.session().model_label.clone()),
                )
                .trigger_style(StyleRefinement::default().min_w_0()),
            )
            .child(div().flex_shrink_0().pl_1().child(send));

        // 一张卡片：附件缩略图、随内容长高的多行输入（至少两行），底边是工具栏。
        // 静止：细边 + 一层贴地的阴影；悬停边框加深一档（提示整块都能点）；
        // 聚焦：边框保持 `LINE_STRONG`，外面套三层焦点环（一道纸色缝 + 实边 + 光晕，见 `focus_ring`）。
        // 鼠标点进来也出环：输入框是这张卡片的主体，点了它就是「正在这里打字」，
        // 和按钮那种「键盘 Tab 才出环」的语义不同（见 `focus_visible`）。
        let card = div()
            .id("composer")
            .v_flex()
            .w_full()
            .bg(rgb(CARD))
            .border_1()
            .border_color(rgb(if focused { LINE_STRONG } else { LINE }))
            .when(!focused, |d| d.hover(|d| d.border_color(rgb(LINE_STRONG))))
            .rounded_2xl()
            .shadow(if focused { focus_ring(INK_FAINT) } else { raised_shadow() })
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

        div()
            .v_flex()
            .max_w(READING_W)
            .mx_auto()
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
                            .child(self.with_completion_keys(card, cx))
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
            // 首页：输入框下方一排建议入口（见 overview.rs）
            .when(!self.session().has_user_message() && !self.settings.needs_setup(), |d| {
                d.child(self.render_suggestions(cx))
            })
    }

    /// 输入框正上方的提问面板（`ask_user`）：一次只问一题，点一个选项就走到下一题，
    /// 最后一题选完直接交给模型——所以没有「就这样」那一排按钮。
    /// 和补全面板一样按输入框上沿量到的位置限高，窗口矮 / 字号大时在面板里滚。
    fn render_question_panel(&self, window: &Window, cx: &mut Context<Self>) -> Option<Div> {
        let item_idx = self.session().pending_question()?;
        let crate::model::ChatItem::Question { questions, .. } =
            &self.session().transcript[item_idx]
        else {
            return None;
        };
        let cursor = self.session().question_cursor(item_idx)?;
        let question = questions.get(cursor)?;
        let total = questions.len();
        let last = cursor + 1 >= total;
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
                            Icon::new(IconAll::Circle)
                                .size_3p5()
                                .flex_shrink_0()
                                .text_color(rgb(INK_FAINT)),
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
            .child(div().flex_1().min_w_0().child(if last {
                "选一个就交给模型"
            } else {
                "选一个继续下一题"
            }))
            .child(link("question-self", "自己说").on_click(cx.listener(move |this, _, _, cx| {
                this.answer_question(item_idx, claudesktop_core::QuestionAnswer::SelfAnswer, cx);
            })))
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

/// 模型下拉：
/// 打开时若自研内核还没有模型列表，顺手获取一次。
fn model_menu_popover(
    handle: &Entity<ChatApp>,
    focus: &FocusHandle,
    id: &'static str,
    anchor: Anchor,
    chip: MenuChip,
) -> Popover {
    let fetch = handle.clone();
    menu_popover(id, anchor, rems(17.5), handle, focus, chip, model_menu_items).on_open_change(
        move |open, _, cx| {
            if *open {
                fetch.update(cx, |app, cx| app.fetch_models_for_menu(cx));
            }
        },
    )
}

// ---------- 菜单条目（在 Popover 面板里渲染） ----------

/// 会话级思考档位 chip 的文字：没选过就是「默认」（跟随设置里的档案）。
/// 显示的是这个模型上实际生效的那一档（换了模型后存着的档位可能被降级，或在认不出的模型上不发）
fn thinking_chip(app: &ChatApp) -> Option<MenuChip> {
    if app.session().kernel != KernelChoice::Builtin {
        return None;
    }
    let spec = session_thinking_spec(app);
    let label = active_thinking(&spec, app.session().thinking).map_or("默认", ThinkingLevel::label);
    Some(MenuChip::new(IconAll::Brain, label))
}

/// 会话级思考档位 chip：ACP 内核不画（模型由 agent 自己定）。点开是 `thinking_menu_items`。
/// 在 `ChatApp` 自己的 render 里调：状态直接从 `this` 读，别 `handle.read(cx)`
///（实体正在 update，再读一次就是 double lease，一打开窗口就崩）。
fn render_thinking_chip(this: &ChatApp, handle: &Entity<ChatApp>) -> Option<AnyElement> {
    let chip = thinking_chip(this)?;
    Some(
        menu_popover(
            "thinking-menu",
            Anchor::BottomRight,
            rems(17.5),
            handle,
            &this.menu_focus,
            chip,
            thinking_menu_items,
        )
        .into_any_element(),
    )
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

/// 思考档位条目：能选哪些档跟当前模型现算，实际生效的那一档打勾。
/// 认不出的模型只有「默认 / 兜底」：别的档位在它上面什么都不发（见 `apply_thinking`），列出来是误导
fn thinking_menu_items(
    app: &ChatApp,
    handle: &Entity<ChatApp>,
    popover: &Entity<PopoverState>,
) -> MenuList {
    let spec = session_thinking_spec(app);
    let active = active_thinking(&spec, app.session().thinking);
    let mut menu = MenuList::new("thinking-menu-list", handle, popover);
    // 「默认」= 这个会话不覆盖，用设置里给档案配的那一档
    menu = menu.pick(
        menu_item(
            "thinking-default",
            IconAll::Circle,
            "默认",
            "用设置里「内核与模型」给这个档案配的那一档",
            active.is_none(),
        ),
        |this, cx| this.choose_thinking(None, cx),
    );
    if spec.is_known() {
        for level in spec.choices() {
            let level = *level;
            menu = menu.pick(
                menu_item(
                    ("thinking-item", thinking_key(level)),
                    thinking_icon(level),
                    level.label(),
                    thinking_hint(level),
                    active == Some(level),
                ),
                move |this, cx| this.choose_thinking(Some(level), cx),
            );
        }
    } else {
        // 认不出的模型：只有「兜底」（按通用的 effort 字段发）
        menu = menu.pick(
            menu_item(
                "thinking-fallback",
                IconAll::Sparkles,
                ThinkingLevel::Default.label(),
                "还不知道这个模型认不认，按通用的 effort 字段发",
                active == Some(ThinkingLevel::Default),
            ),
            |this, cx| this.choose_thinking(Some(ThinkingLevel::Default), cx),
        );
    }
    menu
}

/// 档位 → `MenuList` 的稳定下标（不能用文案，文案会变）
fn thinking_key(level: ThinkingLevel) -> usize {
    match level {
        ThinkingLevel::Off => 0,
        ThinkingLevel::Minimal => 1,
        ThinkingLevel::Low => 2,
        ThinkingLevel::Medium => 3,
        ThinkingLevel::High => 4,
        ThinkingLevel::XHigh => 5,
        ThinkingLevel::Max => 6,
        ThinkingLevel::Default => 7,
    }
}

fn thinking_icon(level: ThinkingLevel) -> IconAll {
    match level {
        ThinkingLevel::Off => IconAll::Circle,
        ThinkingLevel::Minimal | ThinkingLevel::Low | ThinkingLevel::Medium => IconAll::Gauge,
        ThinkingLevel::High | ThinkingLevel::XHigh | ThinkingLevel::Max => IconAll::Zap,
        ThinkingLevel::Default => IconAll::Sparkles,
    }
}

/// 条目第二行的灰色说明：写「这是干什么的」，不写具体档位名（各家叫法不一样，说了反而误导）
fn thinking_hint(level: ThinkingLevel) -> &'static str {
    match level {
        ThinkingLevel::Off => "不思考，最快也最便宜",
        ThinkingLevel::Minimal => "极短的思考",
        ThinkingLevel::Low => "简单任务足够",
        ThinkingLevel::Medium => "日常任务的平衡档",
        ThinkingLevel::High => "推理充分一些，慢一点",
        ThinkingLevel::XHigh => "复杂任务用，明显更慢",
        ThinkingLevel::Max => "最高档，最慢也最贵",
        ThinkingLevel::Default => "按通用的 effort 字段发",
    }
}

/// 权限模式条目：图标 + 名称（+说明），当前项打勾。
fn mode_menu_items(
    app: &ChatApp,
    handle: &Entity<ChatApp>,
    popover: &Entity<PopoverState>,
) -> MenuList {
    let session = app.session();
    let mut menu = MenuList::new("mode-menu-list", handle, popover);
    for (i, mode) in session.modes.iter().enumerate() {
        let active = session.permission_mode == i;
        menu = menu.pick(
            menu_item(("mode-item", i), mode.icon, mode.name.clone(), mode.desc.clone(), active),
            move |this, cx| this.choose_mode(i, cx),
        );
    }
    menu
}

/// 工作目录条目：用过的目录（当前会话所在的打对勾）+「不选项目（任务）」+「选择文件夹…」。
fn workspace_menu_items(
    app: &ChatApp,
    handle: &Entity<ChatApp>,
    popover: &Entity<PopoverState>,
) -> MenuList {
    let current = &app.session().cwd;
    let task = app.session().task;
    let mut menu = MenuList::new("workspace-menu-list", handle, popover);
    for (i, dir) in app.recent_workspaces().into_iter().enumerate() {
        // 任务会话没有「当前项目」，一个都不打勾
        let active = !task && dir == *current;
        let path = short_path(&dir);
        let label = project_label(&dir);
        menu = menu.pick(
            menu_item(("workspace-item", i), IconAll::Folder, label, path, active),
            move |this, cx| this.switch_workspace(dir.clone(), cx),
        );
    }
    if app.git_repo_here() {
        // 在新 worktree 中开始：从当前分支的最新提交拉一个分支，未提交的改动不会带过去
        menu = menu.child(div().mt_0p5().border_t_1().border_color(rgb(LINE))).pick(
            menu_item(
                "workspace-worktree",
                IconAll::GitBranch,
                "在新 worktree 中开始",
                "从当前分支的最新提交开始，未提交的改动不会带过去",
                false,
            ),
            |this, cx| this.start_in_worktree(cx),
        );
    }
    menu.child(div().mt_0p5().border_t_1().border_color(rgb(LINE)))
        // 不选项目：这段对话算一件任务，归到侧栏下半区。已经跑在默认目录里就只改归类
        .pick(
            menu_item(
                "workspace-task",
                IconAll::ListTodo,
                "不选项目（任务）",
                if task {
                    "这段对话已归在侧栏的「任务」里"
                } else {
                    "不指定文件夹，只作为一件任务"
                },
                task,
            ),
            |this, cx| this.as_task(cx),
        )
        .pick(
            menu_item("workspace-pick", IconAll::FolderOpen, "选择文件夹…", "", false),
            |this, cx| this.pick_workspace(cx),
        )
}

/// 模型条目：非自研内核给提示；列表 + 刷新入口（刷新不收起面板，看得到进度）。
fn model_menu_items(
    app: &ChatApp,
    handle: &Entity<ChatApp>,
    popover: &Entity<PopoverState>,
) -> MenuList {
    let hint = |text: SharedString| {
        div().px_2p5().py_2().text_size(TEXT_SMALL).text_color(rgb(INK_SOFT)).child(text)
    };
    let builtin = app.session().kernel == KernelChoice::Builtin;
    let mut menu = MenuList::new("model-menu-list", handle, popover).scroll(rems(24.));
    if !builtin {
        return menu.child(hint("当前内核的模型由 agent 自己决定，这里不能切换".into()));
    }
    let models = app.settings.builtin.models();
    if models.is_empty() {
        menu = menu.child(hint(if app.fetching_models {
            "正在获取模型列表…".into()
        } else {
            "没有模型列表，可在设置里获取".into()
        }));
    }
    let current = app.session().model_label.clone();
    for (i, model) in models.iter().enumerate() {
        let active = current.as_ref() == model.as_str();
        let m = model.clone();
        menu = menu.pick(
            div()
                .id(("model-item", i))
                .h_flex()
                .items_center()
                .gap_2()
                .px_2p5()
                .py_1p5()
                .rounded_md()
                .cursor_pointer()
                .hover(|d| d.bg(rgb(HOVER)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(TEXT_BODY)
                        .font_family(MONO)
                        .text_color(rgb(INK))
                        .child(model.clone()),
                )
                .when(active, |d| {
                    d.child(Icon::new(IconAll::Check).size_4().text_color(rgb(CORAL)))
                }),
            move |this, cx| this.choose_model(m.clone(), cx),
        );
    }
    let handle = handle.clone();
    menu.child(
        div()
            .id("model-refresh")
            .h_flex()
            .gap_2()
            .px_2p5()
            .py_1p5()
            .mt_0p5()
            .border_t_1()
            .border_color(rgb(LINE))
            .cursor_pointer()
            .hover(|d| d.bg(rgb(HOVER)))
            .text_size(TEXT_SMALL)
            .text_color(rgb(INK_SOFT))
            .on_click(move |_, _, cx| {
                handle.update(cx, |this, cx| this.fetch_models(false, cx));
            })
            .child(Icon::new(IconAll::RefreshCw).size_3p5())
            .child("刷新模型列表"),
    )
}

//! 底部输入区（上方环境 / 工作目录，多行输入 + 发送，下方一行权限模式 / 附件 / 模型）与导出、附件等动作。

use super::menu::{MenuChip, menu_action, menu_item, menu_popover};
use super::{ChatApp, project_label, short_path};
use crate::model::PendingImage;
use crate::settings::KernelChoice;
use crate::theme::rgb;
use crate::theme::*;
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::base::PopoverState;
use gpui_kit::component::input::Textarea;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::popover::Popover;
use gpui_kit::component::progress::ProgressCircle;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::sync::Arc;

impl ChatApp {
    /// 「分享」：当前会话导出为 Markdown，系统保存对话框选位置。
    pub(crate) fn share_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let idx = self.active;
        let markdown = self.sessions[idx].to_markdown();
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

    /// 加号按钮：选文件，把路径插进输入框（提示词里引用，交给 agent 读）。
    pub(crate) fn attach_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("选择要交给 agent 的文件".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                if paths.is_empty() {
                    return;
                }
                let _ = this.update_in(cx, |app, window, cx| {
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
    pub(super) fn sidebar_toggle(&self, id: &'static str, cx: &mut Context<Self>) -> Stateful<Div> {
        div()
            .id(id)
            .size(px(28.))
            .flex_shrink_0()
            .rounded_md()
            .flex()
            .justify_center()
            .items_center()
            .cursor_pointer()
            // 沙色侧栏和纸色顶栏上都会出现，取深的一档两边都看得见
            .hover(|d| d.bg(rgb(SELECTED)))
            .on_click(cx.listener(|this, _, _, cx| {
                this.sidebar_open = !this.sidebar_open;
                cx.notify();
            }))
            .child(Icon::new(IconAll::PanelLeft).size_4().text_color(rgb(INK_SOFT)))
    }

    /// 权限模式菜单里选了一项：会话立即生效，已建立的内核会话同步 set_mode。
    pub(crate) fn choose_mode(&mut self, i: usize, cx: &mut Context<Self>) {
        let kernel = self.session_kernel(self.active);
        let s = &mut self.sessions[self.active];
        s.permission_mode = i;
        if let (Some(kernel), Some(id), Some(mode)) = (kernel, s.acp_id.clone(), s.modes.get(i)) {
            let _ = kernel.set_mode(&id, &mode.id);
        }
        cx.notify();
    }

    /// 输入框上方的一张待发送图片缩略图（右上角 × 撤掉）。
    fn pending_image_thumb(&self, i: usize, cx: &mut Context<Self>) -> Stateful<Div> {
        div()
            .id(("pending-image", i))
            .relative()
            .size(px(56.))
            .flex_shrink_0()
            .rounded_md()
            .overflow_hidden()
            .border_1()
            .border_color(rgb(LINE))
            .tooltip({
                let name = self.pending[i].name.clone();
                move |window, cx| Tooltip::new(name.clone()).build(window, cx)
            })
            .child(
                img(Arc::new(self.pending[i].preview.clone()))
                    .size_full()
                    .object_fit(ObjectFit::Cover),
            )
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
        // 焦点几乎总在输入框上（丢了由 restore_focus 找回），所以窗口不在前台时按没聚焦画：
        // 切去别的程序后输入框回到静止样子，鼠标移上来是悬停态，点回来才是聚焦态
        let focused =
            window.is_window_active() && self.input.read(cx).focus_handle(cx).is_focused(window);

        // 发送按钮三态：空（淡、不可点）/ 可发送（赤陶）/ 忙（深色停止方块，同 Esc）
        let can_send = !self.input.read(cx).value().trim().is_empty() || !self.pending.is_empty();
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
                    (true, _) => "停止（Esc）",
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
                    .text_color(rgb(if can_send { WHITE } else { INK_FAINT }))
                    .into_any_element()
            })
            .on_click(cx.listener(|this, _, window, cx| {
                if this.session().busy {
                    this.stop(cx);
                } else {
                    // 空输入时 send 自己会忽略
                    this.send(window, cx);
                }
            }));

        // 卡片下方紧贴一行：左侧权限模式 / 添加文件，右侧模型。纯文字 chip，open 状态由 Popover 自管；
        // 左右内缩到和输入文字对齐，读起来仍是输入框的一部分
        let toolbar = div()
            .h_flex()
            .items_center()
            .gap_0p5()
            .px_2()
            .child(menu_popover(
                "mode-menu",
                Anchor::BottomLeft,
                px(300.),
                &app,
                MenuChip::plain(mode_name),
                mode_menu_items,
            ))
            .child(
                div()
                    .id("attach-files")
                    .size(px(26.))
                    .flex_shrink_0()
                    .rounded_md()
                    .flex()
                    .justify_center()
                    .items_center()
                    .cursor_pointer()
                    .hover(|d| d.bg(rgb(HOVER)))
                    .tooltip(|window, cx| Tooltip::new("添加文件").build(window, cx))
                    .on_click(cx.listener(|this, _, window, cx| this.attach_files(window, cx)))
                    .child(Icon::new(IconAll::Plus).size_4().text_color(rgb(INK_SOFT))),
            )
            .child(div().flex_1())
            .children(self.render_context_gauge())
            .child(model_menu_popover(
                &app,
                "model-menu",
                Anchor::BottomRight,
                MenuChip::plain(self.session().model_label.clone()),
            ));

        // 卡片里只有附件缩略图和随内容长高的多行输入 + 发送键（对照官方 Code 桌面端的紧凑输入框）。
        // 三档：静止细边；悬停边框加深一档（提示整块都能点）；聚焦再深一档并加阴影。
        // 点卡片空白处也把焦点给输入框。
        let card =
            div()
                .id("composer")
                .v_flex()
                .w_full()
                .bg(rgb(CARD))
                .border_1()
                .border_color(rgb(if focused { INK_FAINT } else { LINE }))
                .when(!focused, |d| d.hover(|d| d.border_color(rgb(LINE_STRONG))))
                .rounded_2xl()
                .map(|d| if focused { d.shadow_sm() } else { d.shadow_xs() })
                // 发送键离右边和上下都是 6px
                .px_1p5()
                .py_1()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.focus_input(window, cx)),
                )
                // 待发送图片缩略图（点右上角 × 撤掉），左边和输入文字对齐
                .when(!self.pending.is_empty(), |d| {
                    d.child(
                        div().h_flex().flex_wrap().gap_1p5().px_2p5().pt_1p5().pb_1().children(
                            (0..self.pending.len()).map(|i| self.pending_image_thumb(i, cx)),
                        ),
                    )
                })
                // 发送键贴底：输入框长高后它留在右下角（单行时和文字齐平）
                .child(
                    div()
                        .h_flex()
                        .items_end()
                        .gap_1()
                        .child(
                            div().flex_1().min_w_0().child(
                                Textarea::new(&self.input)
                                    .appearance(false)
                                    .bordered(false)
                                    .on_paste(paste_to_attachments(app.clone())),
                            ),
                        )
                        .child(send),
                );

        div()
            .v_flex()
            .max_w(READING_W)
            .mx_auto()
            .w_full()
            .gap_2()
            // 进行中这一轮的计划钉在最上面（对话往下走它也一直看得见）
            .children(self.render_pinned_plan(cx))
            // 输入框上方：运行环境 + 项目目录（点开切换工作目录）
            .child(div().h_flex().gap_1p5().child(Self::chip(IconAll::Monitor, "本地")).child(
                menu_popover(
                    "workspace-menu",
                    Anchor::BottomLeft,
                    px(320.),
                    &app,
                    MenuChip::new(IconAll::Folder, project).filled(),
                    workspace_menu_items,
                ),
            ))
            .child(
                div()
                    .v_flex()
                    .gap_1()
                    .children(self.render_context_warning(cx))
                    // `/`、`@` 补全列表浮在卡片正上方，盖住对话而不是把输入框往下挤
                    .child(div().relative().child(self.with_completion_keys(card, cx)).children(
                        self.render_completion(cx).map(|list| {
                            div().absolute().left_0().right_0().bottom_full().mb_1p5().child(list)
                        }),
                    ))
                    .child(toolbar),
            )
    }
}

impl ChatApp {
    /// 当前会话的上下文用量；没报告过用量时为 None（只有自研内核报告，窗口大小也只有它知道）。
    fn context_gauge(&self) -> Option<crate::usage::ContextGauge> {
        let s = self.session();
        if s.context_tokens == 0 {
            return None;
        }
        let window = match s.kernel {
            KernelChoice::Builtin => self.settings.builtin.context_window(&s.model_label),
            _ => None,
        };
        Some(crate::usage::context_gauge(s.context_tokens, window))
    }

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

    /// 模型名左边的上下文用量：小圆环 + 百分比，悬停看具体数字；端点没报窗口大小时只写用了多少。
    /// 只有自研内核报告用量（ACP 协议没有），一轮结束后才出现。过七成圆环转赤陶，过九成连字一起变红。
    fn render_context_gauge(&self) -> Option<Stateful<Div>> {
        let gauge = self.context_gauge()?;
        let tone = match gauge.percent {
            Some(p) if p >= 90 => RED,
            Some(p) if p >= 70 => CORAL,
            _ => INK_SOFT,
        };
        // 悬停提示第二行：上一轮命中提示缓存的比例（命中的部分按折扣计费，越高越省）
        let tooltip: SharedString = match self.session().cache_hit {
            Some(hit) => format!("{}\n上一轮命中缓存 {hit}%", gauge.tooltip),
            None => gauge.tooltip,
        }
        .into();
        Some(
            div()
                .id("context-gauge")
                .h_flex()
                .flex_shrink_0()
                .items_center()
                .gap_1p5()
                .px_2()
                .py_1()
                .text_size(TEXT_SMALL)
                .text_color(rgb(if tone == RED { RED } else { INK_SOFT }))
                .font_features(tabular_nums())
                .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                .when_some(gauge.percent, |d, p| {
                    d.child(
                        ProgressCircle::new("context-ring")
                            .value(p as f32)
                            .color(rgb(tone))
                            .size(px(14.)),
                    )
                })
                .child(gauge.label),
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
    id: &'static str,
    anchor: Anchor,
    chip: MenuChip,
) -> Popover {
    let fetch = handle.clone();
    menu_popover(id, anchor, px(280.), handle, chip, model_menu_items).on_open_change(
        move |open, _, cx| {
            if *open {
                fetch.update(cx, |app, cx| app.fetch_models_for_menu(cx));
            }
        },
    )
}

// ---------- 菜单条目（在 Popover 面板里渲染） ----------

/// 权限模式条目：图标 + 名称（+说明），当前项打勾。
fn mode_menu_items(app: &ChatApp, handle: &Entity<ChatApp>, popover: &Entity<PopoverState>) -> Div {
    let session = app.session();
    let mut menu = div().v_flex().gap_0p5();
    for (i, mode) in session.modes.iter().enumerate() {
        let active = session.permission_mode == i;
        let (handle, popover) = (handle.clone(), popover.clone());
        menu = menu.child(
            menu_item(("mode-item", i), mode.icon, mode.name.clone(), mode.desc.clone(), active)
                .on_click(menu_action!(handle, popover, |this, cx| {
                    this.choose_mode(i, cx);
                })),
        );
    }
    menu
}

/// 工作目录条目：用过的目录（当前会话所在的打对勾）+「选择文件夹…」。
fn workspace_menu_items(
    app: &ChatApp,
    handle: &Entity<ChatApp>,
    popover: &Entity<PopoverState>,
) -> Div {
    let current = &app.session().cwd;
    let mut menu = div().v_flex().gap_0p5();
    for (i, dir) in app.recent_workspaces().into_iter().enumerate() {
        let active = dir == *current;
        let path = short_path(&dir);
        let (handle, popover) = (handle.clone(), popover.clone());
        menu = menu.child(
            menu_item(("workspace-item", i), IconAll::Folder, project_label(&dir), path, active)
                .on_click(menu_action!(handle, popover, |this, cx| {
                    this.switch_workspace(dir.clone(), cx);
                })),
        );
    }
    let (handle, popover) = (handle.clone(), popover.clone());
    menu.child(
        div().mt_0p5().pt_0p5().border_t_1().border_color(rgb(LINE)).child(
            menu_item("workspace-pick", IconAll::FolderOpen, "选择文件夹…", "", false)
                .on_click(menu_action!(handle, popover, |this, cx| this.pick_workspace(cx))),
        ),
    )
}

/// 模型条目：非自研内核给提示；列表 + 刷新入口（刷新不收起面板，看得到进度）。
fn model_menu_items(
    app: &ChatApp,
    handle: &Entity<ChatApp>,
    popover: &Entity<PopoverState>,
) -> Stateful<Div> {
    let hint = |text: SharedString| {
        div().px_2p5().py_2().text_size(TEXT_SMALL).text_color(rgb(INK_SOFT)).child(text)
    };
    let builtin = app.session().kernel == KernelChoice::Builtin;
    let mut menu =
        div().id("model-menu-list").max_h(px(380.)).overflow_y_scroll().v_flex().gap_0p5();
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
        let (handle, popover) = (handle.clone(), popover.clone());
        menu = menu.child(
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
                .on_click(menu_action!(handle, popover, |this, cx| {
                    this.choose_model(m.clone(), cx);
                }))
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

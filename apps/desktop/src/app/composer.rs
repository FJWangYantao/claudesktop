//! 底部输入区（含权限模式 / 内核 / 模型下拉）与导出、附件等动作。

use super::ChatApp;
use super::menu::{menu_action, menu_item, menu_popover, MenuChip};
use crate::model::{ChatItem, PendingImage};
use crate::settings::KernelChoice;
use crate::theme::*;
use crate::theme::rgb;
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::base::PopoverState;
use gpui_kit::component::input::Input;
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
        let markdown = self.sessions[idx].to_markdown();
        let suggested = format!("{}.md", self.sessions[idx].title);
        let dir = self.cwd.clone();
        let rx = cx.prompt_for_new_path(&dir, Some(&suggested));
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(path))) = rx.await {
                let _ = this.update_in(cx, |app, _, cx| {
                    let note = match std::fs::write(&path, &markdown) {
                        Ok(()) => format!("会话已导出：{}", path.display()),
                        Err(err) => format!("导出失败：{err}"),
                    };
                    app.sessions[app.active].note(note);
                    cx.notify();
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

    // ---------- 图片附件 ----------

    /// 图片按钮：选图片文件，作为附件随下一条消息发给模型。
    pub(crate) fn attach_images(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("选择要附上的图片（png / jpg / gif / webp / bmp）".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                let _ = this.update_in(cx, |app, _window, cx| {
                    let images: Vec<_> =
                        paths.iter().filter_map(|p| PendingImage::from_path(p)).collect();
                    let bad = paths.len() - images.len();
                    app.add_pending_images(images, cx);
                    if bad > 0 {
                        app.sessions[app.active]
                            .note(format!("{bad} 个文件不是可识别的图片，已跳过"));
                    }
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
        let (mode_name, mode_icon): (SharedString, IconAll) = self
            .session()
            .modes
            .get(self.session().permission_mode)
            .map(|m| (m.name.clone(), m.icon))
            .unwrap_or_else(|| ("—".into(), IconAll::Gauge));
        let project = self
            .session()
            .cwd
            .file_name()
            .map(|n| SharedString::from(n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| self.project_name.clone());
        let focused = self.input.read(cx).focus_handle(cx).is_focused(window);

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
            .ml_1()
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

        // 卡片底部工具栏：权限模式 / 内核 / 附件，右侧档案 / 模型 / 发送。open 状态由 Popover 自管
        let toolbar = div()
            .h_flex()
            .items_center()
            .gap_1()
            .child(menu_popover(
                "mode-menu",
                Anchor::BottomLeft,
                px(300.),
                &app,
                MenuChip::new(mode_icon, mode_name),
                mode_menu_items,
            ))
            .child(menu_popover(
                "kernel-menu",
                Anchor::BottomLeft,
                px(300.),
                &app,
                MenuChip::new(IconAll::Cpu, self.session().kernel.label()),
                kernel_menu_items,
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
            .child(
                div()
                    .id("attach-images")
                    .size(px(26.))
                    .flex_shrink_0()
                    .rounded_md()
                    .flex()
                    .justify_center()
                    .items_center()
                    .cursor_pointer()
                    .hover(|d| d.bg(rgb(HOVER)))
                    .tooltip(|window, cx| {
                        Tooltip::new("附加图片（也可直接粘贴）").build(window, cx)
                    })
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.attach_images(window, cx)
                    }))
                    .child(Icon::new(IconAll::ImagePlus).size_4().text_color(rgb(INK_SOFT))),
            )
            .child(div().flex_1())
            .when(self.settings.builtin.profiles.len() > 1, |d| {
                d.child(menu_popover(
                    "profile-menu",
                    Anchor::BottomRight,
                    px(280.),
                    &app,
                    MenuChip::new(
                        IconAll::Server,
                        self.settings.builtin.current().display_name(),
                    ),
                    profile_menu_items,
                ))
            })
            .child(model_menu_popover(
                &app,
                "model-menu",
                Anchor::BottomRight,
                MenuChip::text(self.session().model_label.clone()),
            ))
            .child(send);

        // 输入框是一个整体：附件缩略图、文字、工具栏都在同一张卡片里（对照 claude.ai / 官方 Code 桌面端）。
        // 聚焦时边框和阴影加深一档；点卡片空白处也把焦点给输入框。
        let card = div()
            .id("composer")
            .v_flex()
            .gap_1()
            .w_full()
            .bg(rgb(CARD))
            .border_1()
            .border_color(rgb(if focused { INK_FAINT } else { LINE }))
            .rounded_2xl()
            .map(|d| if focused { d.shadow_sm() } else { d.shadow_xs() })
            .px_2()
            .pt_2()
            .pb_2()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| this.focus_input(window, cx)),
            )
            // 待发送图片缩略图（点右上角 × 撤掉）
            .when(!self.pending.is_empty(), |d| {
                d.child(
                    div()
                        .h_flex()
                        .flex_wrap()
                        .gap_1p5()
                        .px_2()
                        .pt_1()
                        .children(
                            (0..self.pending.len()).map(|i| self.pending_image_thumb(i, cx)),
                        ),
                )
            })
            .child(
                div().px_2().py_1().child(
                    Input::new(&self.input)
                        .appearance(false)
                        .bordered(false)
                        .on_paste(paste_to_attachments(app.clone())),
                ),
            )
            .child(toolbar);

        div()
            .v_flex()
            .max_w(READING_W)
            .mx_auto()
            .w_full()
            .gap_2()
            // 输入框上方：运行环境 + 项目目录
            .child(
                div()
                    .h_flex()
                    .gap_1p5()
                    .child(Self::chip(IconAll::Monitor, "本地"))
                    .child(Self::chip(IconAll::Folder, project)),
            )
            .child(card)
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
fn mode_menu_items(
    app: &ChatApp,
    handle: &Entity<ChatApp>,
    popover: &Entity<PopoverState>,
) -> Div {
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

/// 内核条目：空会话直接换内核（并记为默认）；有对话的会话固定在原内核上，
/// 列出「在别的内核上重跑首条提示词」。
fn kernel_menu_items(
    app: &ChatApp,
    handle: &Entity<ChatApp>,
    popover: &Entity<PopoverState>,
) -> Div {
    let session = app.session();
    let current = session.kernel;
    let first_prompt = session
        .transcript
        .iter()
        .position(|i| matches!(i, ChatItem::User { .. }));
    let mut menu = div().v_flex().gap_0p5();
    if first_prompt.is_some() {
        menu = menu.child(
            div()
                .px_2p5()
                .py_1p5()
                .text_size(TEXT_SMALL)
                .text_color(rgb(INK_SOFT))
                .child(format!(
                    "这个会话跑在「{}」上。换个内核重跑首条提示词：",
                    current.label()
                )),
        );
    }
    for (i, kind) in KernelChoice::ALL.into_iter().enumerate() {
        if first_prompt.is_some() && kind == current {
            continue;
        }
        let usable = kind != KernelChoice::Builtin || app.settings.builtin.is_configured();
        let detail: SharedString = match kind {
            KernelChoice::Builtin if usable => app.settings.builtin.model().trim().to_string().into(),
            KernelChoice::Builtin => "还没配置端点和模型（见设置）".into(),
            KernelChoice::Acp => match app.settings.acp.command.trim() {
                "" => "内置 mock agent".into(),
                cmd => cmd.to_string().into(),
            },
            KernelChoice::Demo => "不连模型，只播放演示".into(),
        };
        let title: SharedString = if first_prompt.is_some() {
            format!("在「{}」上重跑", kind.label()).into()
        } else {
            kind.label().into()
        };
        let active = first_prompt.is_none() && kind == current;
        let (handle, popover) = (handle.clone(), popover.clone());
        menu = menu.child(
            div()
                .id(("kernel-item", i))
                .h_flex()
                .items_center()
                .gap_2p5()
                .px_2p5()
                .py_2()
                .rounded_md()
                .when(usable, |d| {
                    d.cursor_pointer()
                        .hover(|d| d.bg(rgb(HOVER)))
                        .on_click(menu_action!(handle, popover, |this, cx| {
                            match first_prompt {
                                Some(item) => this.rerun_on(item, kind, cx),
                                None => {
                                    this.update_settings(|s| s.kernel = Some(kind));
                                    this.rebind_session(this.active, kind);
                                }
                            }
                        }))
                })
                .child(
                    Icon::new(if first_prompt.is_some() { IconAll::RefreshCw } else { IconAll::Cpu })
                        .size_4()
                        .text_color(rgb(if usable { INK_SOFT } else { INK_FAINT })),
                )
                .child(
                    div()
                        .v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .text_size(TEXT_BODY)
                                .text_color(rgb(if usable { INK } else { INK_FAINT }))
                                .child(title),
                        )
                        .child(
                            div()
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .text_size(TEXT_SMALL)
                                .text_color(rgb(INK_SOFT))
                                .child(detail),
                        ),
                )
                .when(active, |d| {
                    d.child(Icon::new(IconAll::Check).size_4().text_color(rgb(CORAL)))
                }),
        );
    }
    menu
}

/// 模型条目：非自研内核给提示；列表 + 刷新入口（刷新不收起面板，看得到进度）。
fn model_menu_items(
    app: &ChatApp,
    handle: &Entity<ChatApp>,
    popover: &Entity<PopoverState>,
) -> Stateful<Div> {
    let hint = |text: SharedString| {
        div()
            .px_2p5()
            .py_2()
            .text_size(TEXT_SMALL)
            .text_color(rgb(INK_SOFT))
            .child(text)
    };
    let builtin = app.session().kernel == KernelChoice::Builtin;
    let mut menu = div()
        .id("model-menu-list")
        .max_h(px(380.))
        .overflow_y_scroll()
        .v_flex()
        .gap_0p5();
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

/// provider 档案条目：切档案并重启自研内核（当前项打勾）。
fn profile_menu_items(
    app: &ChatApp,
    handle: &Entity<ChatApp>,
    popover: &Entity<PopoverState>,
) -> Div {
    let current = app.settings.builtin.active;
    let mut menu = div().v_flex().gap_0p5();
    for (i, p) in app.settings.builtin.profiles.iter().enumerate() {
        let active = i == current;
        let configured = p.is_configured();
        let title: SharedString = p.display_name().into();
        let detail: SharedString = if configured {
            p.model.trim().to_string().into()
        } else {
            "还没填完（见设置）".into()
        };
        let (handle, popover) = (handle.clone(), popover.clone());
        menu = menu.child(
            div()
                .id(("profile-item", i))
                .h_flex()
                .items_center()
                .gap_2p5()
                .px_2p5()
                .py_1p5()
                .rounded_md()
                .cursor_pointer()
                .hover(|d| d.bg(rgb(HOVER)))
                .on_click(menu_action!(handle, popover, |this, cx| {
                    this.switch_profile(i, cx);
                }))
                .child(
                    Icon::new(IconAll::Server)
                        .size_4()
                        .text_color(rgb(if configured { INK_SOFT } else { INK_FAINT })),
                )
                .child(
                    div()
                        .v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .text_size(TEXT_BODY)
                                .text_color(rgb(INK))
                                .child(title),
                        )
                        .child(
                            div()
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .text_size(TEXT_SMALL)
                                .text_color(rgb(INK_SOFT))
                                .child(detail),
                        ),
                )
                .when(active, |d| {
                    d.child(Icon::new(IconAll::Check).size_4().text_color(rgb(CORAL)))
                }),
        );
    }
    menu.child(
        div()
            .px_2p5()
            .py_1p5()
            .text_size(TEXT_SMALL)
            .text_color(rgb(INK_SOFT))
            .child("切换会重启自研内核，进行中的对话下次发消息时续接"),
    )
}

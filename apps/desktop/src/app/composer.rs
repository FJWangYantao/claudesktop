//! 会话区顶栏与底部输入区（含模式下拉）。

use super::ChatApp;
use super::menu::{menu_popover, MenuChip};
use crate::model::ChatItem;
use crate::settings::KernelChoice;
use crate::theme::*;
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::base::PopoverState;
use gpui_kit::component::input::Input;
use gpui_kit::component::popover::Popover;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

impl ChatApp {
    pub(super) fn render_top_bar(&self, cx: &mut Context<Self>) -> Div {
        let app = cx.entity();
        div()
            .h_flex()
            .items_center()
            .gap_2p5()
            .px_5()
            .py_2p5()
            .border_b_1()
            .border_color(rgb(LINE))
            // 侧栏开关放顶栏最左（标题栏是系统拖拽区，收不到点击）
            .child(self.sidebar_toggle("topbar-sidebar-toggle", cx))
            .child(
                div()
                    .h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .size(px(18.))
                            .rounded_md()
                            .bg(rgb(0xC2694E))
                            .flex()
                            .justify_center()
                            .items_center()
                            .child(
                                div()
                                    .text_size(TEXT_TAG)
                                    .text_color(rgb(0xFFFFFF))
                                    .child("甲"),
                            ),
                    )
                    .child(
                        div()
                            .text_size(TEXT_LARGE)
                            .text_color(rgb(INK))
                            .child(self.session().title.clone()),
                    ),
            )
            .child(div().flex_1())
            .child(
                div()
                    .h_flex()
                    .gap_1p5()
                    .px_2p5()
                    .py_1()
                    .rounded_full()
                    .bg(rgb(BLUE_TINT))
                    .child(
                        Icon::new(IconAll::Folder)
                            .size_3p5()
                            .text_color(rgb(BLUE)),
                    )
                    .child(
                        div()
                            .text_size(TEXT_SMALL)
                            .text_color(rgb(BLUE))
                            .child(self.project_name.clone()),
                    ),
            )
            .child(model_menu_popover(
                &app,
                "model-menu-top",
                Anchor::TopRight,
                MenuChip::text(self.session().model_label.clone()).bordered(),
            ))
            .child(
                div()
                    .id("share-session")
                    .h_flex()
                    .gap_1p5()
                    .px_2p5()
                    .py_1()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|d| d.bg(rgb(SAND)))
                    .on_click(cx.listener(|this, _, window, cx| this.share_session(window, cx)))
                    .child(
                        Icon::new(IconAll::Share)
                            .size_3p5()
                            .text_color(rgb(INK_SOFT)),
                    )
                    .child(
                        div()
                            .text_size(TEXT_SMALL)
                            .text_color(rgb(INK_SOFT))
                            .child("分享"),
                    ),
            )
    }

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

    // ---------- 输入区 ----------
    /// 侧栏开关。Windows 上标题栏整条是系统拖拽区，里面的按钮收不到点击，
    /// 所以开关放在侧栏头部（展开）和左侧窄栏（收起）。
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
            .hover(|d| d.bg(rgb(SAND_ACTIVE)))
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

    pub(super) fn render_composer(&self, cx: &mut Context<Self>) -> Div {
        let app = cx.entity();
        let busy = self.session().busy;
        let send_bg = if busy { rgb(INK_SOFT) } else { rgb(CORAL) };
        let (mode_name, mode_icon): (SharedString, IconAll) = self
            .session()
            .modes
            .get(self.session().permission_mode)
            .map(|m| (m.name.clone(), m.icon))
            .unwrap_or_else(|| ("—".into(), IconAll::Gauge));
        let card = div()
            .id("composer")
            .relative()
            .v_flex()
            .w_full()
            .gap_2()
            .bg(rgb(CARD))
            .border_1()
            .border_color(rgb(LINE))
            .rounded(px(22.))
            .shadow_sm()
            .px_4()
            .pt_3()
            .pb_3()
            .child(Input::new(&self.input).appearance(false).bordered(false))
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .id("attach-files")
                            .size(px(30.))
                            .flex_shrink_0()
                            .rounded_full()
                            .border_1()
                            .border_color(rgb(LINE))
                            .flex()
                            .justify_center()
                            .items_center()
                            .cursor_pointer()
                            .hover(|d| d.bg(rgb(SAND)))
                            .on_click(cx.listener(|this, _, window, cx| this.attach_files(window, cx)))
                            .child(Icon::new(IconAll::Plus).size_4().text_color(rgb(INK_SOFT))),
                    )
                    // 权限模式 / 内核 / 模型三个下拉，open 状态由 Popover 自管
                    .child(
                        menu_popover(
                            "mode-menu",
                            Anchor::BottomLeft,
                            px(300.),
                            &app,
                            MenuChip::new(mode_icon, mode_name),
                            mode_menu_items,
                        ),
                    )
                    .child(
                        menu_popover(
                            "kernel-menu",
                            Anchor::BottomLeft,
                            px(300.),
                            &app,
                            MenuChip::new(IconAll::Cpu, self.session().kernel.label()),
                            kernel_menu_items,
                        ),
                    )
                    .child(div().flex_1())
                    .child(model_menu_popover(
                        &app,
                        "model-menu",
                        Anchor::BottomRight,
                        MenuChip::text(self.session().model_label.clone()),
                    ))
                    .child(
                        div()
                            .id("send")
                            .size(px(32.))
                            .flex_shrink_0()
                            .rounded_full()
                            .bg(send_bg)
                            .flex()
                            .justify_center()
                            .items_center()
                            .cursor_pointer()
                            .child(
                                Icon::new(if busy {
                                    IconAll::CircleStop
                                } else {
                                    IconAll::ArrowUp
                                })
                                .size_4()
                                .text_color(rgb(0xFFFFFF)),
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                if this.session().busy {
                                    this.stop(cx);
                                } else {
                                    this.send(window, cx);
                                }
                            })),
                    ),
            );

        div()
            .v_flex()
            .max_w(px(760.))
            .mx_auto()
            .w_full()
            .gap_2p5()
            // 忙时卡片上方出现运行状态条
            .children(self.render_turn_status())
            .child(card)
            .child(
                // 卡片下方：环境 + 项目
                div()
                    .h_flex()
                    .gap_2()
                    .child(Self::chip(IconAll::Monitor, "Local"))
                    .child(Self::chip(IconAll::Folder, self.project_name.clone())),
            )
    }
}

/// 模型下拉（输入框旁与顶栏共用同一个面板，两个锚点）：
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

/// 条目点击的公共收尾：执行动作后收起面板。
macro_rules! menu_action {
    ($handle:expr, $popover:expr, $body:expr) => {
        move |_, window, cx: &mut App| {
            $handle.update(cx, $body);
            $popover.update(cx, |s, cx| s.dismiss(window, cx));
        }
    };
}

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
        let has_desc = !mode.desc.is_empty();
        let (handle, popover) = (handle.clone(), popover.clone());
        menu = menu.child(
            div()
                .id(("mode-item", i))
                .h_flex()
                .items_center()
                .gap_2p5()
                .px_2p5()
                .py_2()
                .rounded_md()
                .cursor_pointer()
                .hover(|d| d.bg(rgb(SAND)))
                .on_click(menu_action!(handle, popover, |this, cx| {
                    this.choose_mode(i, cx);
                }))
                .child(Icon::new(mode.icon).size_4().text_color(rgb(INK_SOFT)))
                .child(
                    div()
                        .v_flex()
                        .flex_1()
                        .child(
                            div()
                                .text_size(TEXT_BODY)
                                .text_color(rgb(INK))
                                .child(mode.name.clone()),
                        )
                        .when(has_desc, |d| {
                            d.child(
                                div()
                                    .text_size(TEXT_SMALL)
                                    .text_color(rgb(INK_SOFT))
                                    .child(mode.desc.clone()),
                            )
                        }),
                )
                .when(active, |d| {
                    d.child(Icon::new(IconAll::Check).size_4().text_color(rgb(CORAL)))
                }),
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
        .position(|i| matches!(i, ChatItem::User(_)));
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
            KernelChoice::Builtin if usable => app.settings.builtin.model.trim().to_string().into(),
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
                        .hover(|d| d.bg(rgb(SAND)))
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
    let models = &app.settings.builtin.models;
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
                .hover(|d| d.bg(rgb(SAND)))
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
            .hover(|d| d.bg(rgb(SAND)))
            .text_size(TEXT_SMALL)
            .text_color(rgb(INK_SOFT))
            .on_click(move |_, _, cx| {
                handle.update(cx, |this, cx| this.fetch_models(false, cx));
            })
            .child(Icon::new(IconAll::RefreshCw).size_3p5())
            .child("刷新模型列表"),
    )
}

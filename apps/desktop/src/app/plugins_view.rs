//! 设置页「插件」：数据目录 `plugins/` 下的插件，看里面有什么、启用 / 停用。
//!
//! 插件的读取和格式在 `claudesktop_core::plugin`；启用了哪些记在设置的 `plugins_enabled` 里
//! （新放进来的默认停用：钩子和 MCP 服务会在本机跑插件作者写的命令，看过再开）。
//! 启用状态一变就经 `Kernel::set_plugins` 交给已经起来的内核，没起来的内核启动时带上。
//! 斜杠命令在发消息时由界面展开（`submit_to`），所以对所有内核都管用。
//!
//! 也可以填 git 地址安装（`claudesktop_core::plugin_git`，克隆在后台线程跑）；
//! 从 git 装的插件卡片上有「更新」「卸载」，手动放进来的没有。

use super::ChatApp;
use super::settings_kit::{how_to, note, section};
use crate::theme::rgb;
use crate::theme::*;
use claudesktop_core::plugin::{self, Plugin};
use claudesktop_core::plugin_git;
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::button::{
    Button, ButtonCustomVariant, ButtonVariant, ButtonVariants as _,
};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::path::PathBuf;

/// 插件页自己的状态：安装地址输入框、进行中的 git 操作、等确认的卸载。
pub(super) struct PluginsForm {
    url: Entity<InputState>,
    /// 正在跑的 git 操作（安装 / 更新）；跑着时别的按钮先不让点
    busy: Option<SharedString>,
    /// 点了「卸载」、等确认的仓库目录
    confirm_uninstall: Option<PathBuf>,
    /// 上次安装失败的原因：留在输入框下面，改了地址才消失（toast 一会儿就没了，来不及看）
    error: Option<SharedString>,
}

impl PluginsForm {
    pub(super) fn new(window: &mut Window, cx: &mut Context<ChatApp>) -> Self {
        let url = cx.new(|cx| {
            InputState::new(window, cx).placeholder("git 仓库地址，或 GitHub 的「作者/仓库」")
        });
        // 回车直接装
        cx.subscribe_in(&url, window, |this: &mut ChatApp, _, event: &InputEvent, window, cx| {
            match event {
                InputEvent::PressEnter { .. } => this.install_plugin(window, cx),
                InputEvent::Change if this.plugins_form.error.is_some() => {
                    this.plugins_form.error = None;
                    cx.notify();
                }
                _ => {}
            }
        })
        .detach();
        Self { url, busy: None, confirm_uninstall: None, error: None }
    }
}

impl ChatApp {
    /// 启用了的插件（按名字记，插件目录里找不到的名字忽略）。
    pub(crate) fn enabled_plugins(&self) -> Vec<Plugin> {
        self.plugins
            .iter()
            .filter(|p| self.settings.plugins_enabled.contains(&p.name))
            .cloned()
            .collect()
    }

    /// 重新扫一遍插件目录，结果交给内核。
    pub(crate) fn rescan_plugins(&mut self, cx: &mut Context<Self>) {
        self.plugins =
            plugin::default_plugins_dir().map(|d| plugin::discover(&d)).unwrap_or_default();
        self.push_plugins();
        cx.notify();
    }

    /// 把启用的插件交给已经起来的内核（没起来的启动时自己带上，见 `spawn_kernel`）。
    pub(crate) fn push_plugins(&self) {
        let enabled = self.enabled_plugins();
        for kernel in self.kernels.iter().filter_map(|k| k.kernel.as_ref()) {
            kernel.set_plugins(enabled.clone());
        }
    }

    fn set_plugin_enabled(&mut self, name: String, on: bool, cx: &mut Context<Self>) {
        self.save_ui_pref(
            move |s| {
                s.plugins_enabled.retain(|n| *n != name);
                if on {
                    s.plugins_enabled.push(name.clone());
                }
            },
            cx,
        );
        self.push_plugins();
        cx.notify();
    }

    /// 克隆输入框里的仓库到插件目录；装好的和手动放进来的一样默认停用。
    fn install_plugin(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.plugins_form.busy.is_some() {
            return;
        }
        let url = self.plugins_form.url.read(cx).value().to_string();
        if let Err(err) = plugin_git::normalize_url(&url) {
            self.plugins_form.error = Some(err.into());
            cx.notify();
            return;
        }
        self.plugins_form.error = None;
        let Some(dir) = plugin::default_plugins_dir() else {
            self.toast(Notification::error("找不到数据目录"), cx);
            return;
        };
        self.plugins_form.busy = Some("正在安装…".into());
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { plugin_git::install(&dir, &url) })
                .await;
            let _ = this.update_in(cx, |app, window, cx| {
                app.plugins_form.busy = None;
                match result {
                    Ok(repo) => {
                        app.plugins_form.url.update(cx, |s, cx| s.set_value("", window, cx));
                        app.rescan_plugins(cx);
                        let n =
                            app.plugins.iter().filter(|p| p.repo.as_ref() == Some(&repo)).count();
                        let text = match n {
                            0 => "装好了，但没在里面找到插件（看看是不是插件仓库）".to_string(),
                            1 => "装好了，默认停用：看过下面列出的内容再打开".to_string(),
                            n => format!("装好了 {n} 个插件，默认停用：看过下面列出的内容再打开"),
                        };
                        app.toast(Notification::success(text), cx);
                    }
                    Err(err) => app.plugins_form.error = Some(format!("没装上：{err}").into()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// `git pull` 一个从 git 装的插件仓库，再重新扫描（内核拿到新内容）。
    fn update_plugin(&mut self, repo: PathBuf, cx: &mut Context<Self>) {
        if self.plugins_form.busy.is_some() {
            return;
        }
        self.plugins_form.busy = Some("正在更新…".into());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result =
                cx.background_executor().spawn(async move { plugin_git::update(&repo) }).await;
            let _ = this.update(cx, |app, cx| {
                app.plugins_form.busy = None;
                match result {
                    Ok(out) => {
                        app.rescan_plugins(cx);
                        let text = if out.contains("up to date") {
                            "已经是最新的"
                        } else {
                            "更新好了"
                        };
                        app.toast(Notification::success(text), cx);
                    }
                    Err(err) => app.toast(Notification::error(format!("更新失败：{err}")), cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 删掉克隆下来的仓库，它带来的插件一并从启用名单里拿掉。
    fn uninstall_plugin(&mut self, repo: PathBuf, cx: &mut Context<Self>) {
        self.plugins_form.confirm_uninstall = None;
        let Some(dir) = plugin::default_plugins_dir() else {
            return;
        };
        if let Err(err) = plugin_git::uninstall(&dir, &repo) {
            self.toast(Notification::error(err), cx);
            cx.notify();
            return;
        }
        let names: Vec<String> = self
            .plugins
            .iter()
            .filter(|p| p.repo.as_ref() == Some(&repo))
            .map(|p| p.name.clone())
            .collect();
        self.save_ui_pref(move |s| s.plugins_enabled.retain(|n| !names.contains(n)), cx);
        self.rescan_plugins(cx);
    }

    /// 在资源管理器里打开插件目录（还没有就建一个）。
    fn open_plugins_dir(&mut self, cx: &mut Context<Self>) {
        let Some(dir) = plugin::default_plugins_dir() else {
            self.toast(Notification::error("找不到数据目录"), cx);
            return;
        };
        if let Err(err) = std::fs::create_dir_all(&dir) {
            self.toast(Notification::error(format!("建不了 {}：{err}", dir.display())), cx);
            return;
        }
        cx.open_with_system(&dir);
    }

    pub(super) fn render_plugins(&self, cx: &mut Context<Self>) -> Div {
        let dir = plugin::default_plugins_dir()
            .map_or_else(|| "plugins".into(), |p| p.display().to_string());
        let actions = div()
            .h_flex()
            .gap_1p5()
            .child(
                Self::text_button("plugins-open", "打开插件目录", false)
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| this.open_plugins_dir(cx))),
            )
            .child(
                Self::text_button("plugins-rescan", "重新扫描", false)
                    .small()
                    .tooltip("手动往插件目录里放了文件夹后点它")
                    .on_click(cx.listener(|this, _, _, cx| this.rescan_plugins(cx))),
            )
            .into_any_element();
        let busy = self.plugins_form.busy.clone();
        // 从 git 地址装：输入框 + 安装按钮（回车也行）
        let install = div()
            .h_flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(LINE))
                    .bg(rgb(CARD))
                    .px_1()
                    .child(Input::new(&self.plugins_form.url).appearance(false).bordered(false)),
            )
            .child(
                Self::text_button("plugins-install", "安装", true)
                    .when(busy.is_some(), |b| b.loading(true))
                    .disabled(busy.is_some())
                    .on_click(cx.listener(|this, _, window, cx| this.install_plugin(window, cx))),
            );
        let error = self.plugins_form.error.clone();
        let mut body = div().v_flex().gap_4().child(
            div()
                .v_flex()
                .gap_2()
                .child(install)
                .when_some(busy, |d, text| {
                    d.child(div().text_size(TEXT_SMALL).text_color(rgb(INK_SOFT)).child(text))
                })
                .when_some(error, |d, text| {
                    d.child(note(IconAll::CircleX, text).text_color(rgb(RED)))
                }),
        );
        if self.plugins.is_empty() {
            body = body.child(how_to(
                "还没有插件",
                &[
                    "在上面填 GitHub 的「作者/仓库」或完整的 git 地址，点「安装」（插件市场仓库也行）",
                    "或者点「打开插件目录」，把插件文件夹直接放进去，再点「重新扫描」",
                    "装好的插件列在这里，默认停用；看过它带了什么，再打开右边的开关",
                ],
            ));
        } else {
            let mut list = div().v_flex().gap_2();
            for (i, p) in self.plugins.iter().enumerate() {
                list = list.child(self.plugin_card(i, p, cx));
            }
            body = body.child(list);
        }
        let notes = div()
            .v_flex()
            .gap_1()
            .child(note(
                IconAll::ShieldCheck,
                "新装的插件默认停用：钩子和 MCP 服务会在你的电脑上运行插件作者写的命令，看过再启用",
            ))
            .child(note(
                IconAll::Info,
                "自研内核里指令、技能、子 agent、MCP 工具、钩子都生效；ACP agent（如 Claude Code）只用指令和 MCP 服务，\
                 对之后新开的会话起作用",
            ))
            .child(note(IconAll::Folder, format!("插件目录：{dir}")));
        section(
            "插件",
            "加斜杠命令、技能、子 agent、MCP 服务和钩子；格式兼容 Claude Code",
            Some(actions),
            body.child(notes),
        )
    }

    /// 一个插件：名字、版本、开关、说明，下面逐项列出它带来的东西（钩子和 MCP 要跑的命令写全）。
    fn plugin_card(&self, i: usize, p: &Plugin, cx: &mut Context<Self>) -> Div {
        let on = self.settings.plugins_enabled.contains(&p.name);
        let name = p.name.clone();
        let line = |label: &'static str, text: String| {
            div()
                .h_flex()
                .items_start()
                .gap_2()
                .text_size(TEXT_SMALL)
                .child(div().w(rems(4.5)).flex_shrink_0().text_color(rgb(INK_SOFT)).child(label))
                .child(div().flex_1().min_w_0().text_color(rgb(INK)).child(text))
        };
        let mut contents = div().v_flex().gap_1();
        if !p.commands.is_empty() {
            let names: Vec<String> = p.commands.iter().map(|c| format!("/{}", c.name)).collect();
            contents = contents.child(line("指令", names.join("  ")));
        }
        if !p.skills.is_empty() {
            let names: Vec<&str> = p.skills.iter().map(|s| s.name.as_str()).collect();
            contents = contents.child(line("技能", names.join("、")));
        }
        // 子 agent：工具清单写出来（没写 = 全部工具），看得出它能不能改文件、跑命令
        for a in &p.agents {
            let tools = a.tools.as_ref().map_or_else(|| "全部工具".to_string(), |t| t.join(", "));
            contents = contents.child(line("子 agent", format!("{}：{tools}", a.name)));
        }
        for s in &p.mcp_servers {
            // 远程的写地址，本地的写命令
            let target = if s.url.is_empty() {
                std::iter::once(s.command.as_str())
                    .chain(s.args.iter().map(String::as_str))
                    .collect::<Vec<_>>()
                    .join(" ")
            } else {
                s.url.clone()
            };
            contents = contents.child(line("MCP", format!("{}：{target}", s.name)));
        }
        for h in &p.hooks {
            let when = match h.matcher.trim() {
                "" | "*" => h.event.label().to_string(),
                m => format!("{}（{m}）", h.event.label()),
            };
            contents = contents.child(line("钩子", format!("{when}：{}", h.command)));
        }
        let empty = p.commands.is_empty()
            && p.skills.is_empty()
            && p.agents.is_empty()
            && p.mcp_servers.is_empty()
            && p.hooks.is_empty();
        div()
            .v_flex()
            .gap_2()
            .px_3()
            .py_2p5()
            .rounded_xl()
            .border_1()
            .border_color(rgb(LINE))
            .bg(rgb(CARD))
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .child(Icon::new(IconAll::Blocks).size_4().text_color(rgb(INK_SOFT)))
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(TEXT_BODY)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(rgb(INK))
                            .child(p.name.clone()),
                    )
                    .when_some(p.version.clone(), |d, v| {
                        d.child(
                            div()
                                .flex_shrink_0()
                                .text_size(TEXT_SMALL)
                                .text_color(rgb(INK_SOFT))
                                .child(format!("v{v}")),
                        )
                    })
                    .child(div().flex_1())
                    // 从 git 装的：更新 / 卸载（卡片自带边框，按钮不描边）
                    .when_some(p.repo.clone(), |d, repo| {
                        let update = repo.clone();
                        d.child(
                            Self::text_button(("plugin-update", i), "更新", false)
                                .small()
                                .with_variant(ButtonVariant::Secondary)
                                .disabled(self.plugins_form.busy.is_some())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.update_plugin(update.clone(), cx)
                                })),
                        )
                        .child(
                            Button::new(("plugin-uninstall", i))
                                .small()
                                .label("卸载")
                                .cursor_pointer()
                                .custom(
                                    ButtonCustomVariant::new(cx)
                                        .foreground(rgb(RED).into())
                                        .hover(rgb(HOVER).into())
                                        .active(rgb(HOVER).into()),
                                )
                                .disabled(self.plugins_form.busy.is_some())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.plugins_form.confirm_uninstall = Some(repo.clone());
                                    cx.notify();
                                })),
                        )
                    })
                    .child(
                        Switch::new(("plugin-enabled", i)).checked(on).color(rgb(CORAL)).on_change(
                            cx.listener(move |this, checked: &bool, _, cx| {
                                this.set_plugin_enabled(name.clone(), *checked, cx)
                            }),
                        ),
                    ),
            )
            .when(!p.description.is_empty(), |d| {
                d.child(
                    div()
                        .text_size(TEXT_SMALL)
                        .text_color(rgb(INK_SOFT))
                        .child(p.description.clone()),
                )
            })
            .child(if empty {
                div()
                    .text_size(TEXT_SMALL)
                    .text_color(rgb(INK_SOFT))
                    .child("没找到指令、技能、MCP 服务或钩子")
            } else {
                contents
            })
            .children(p.problems.iter().map(|problem| {
                div().text_size(TEXT_SMALL).text_color(rgb(RED)).child(problem.clone())
            }))
            .when_some(self.uninstall_prompt(i, p, cx), |d, prompt| d.child(prompt))
    }

    /// 点了「卸载」之后卡片底部的确认行：删哪个目录、连带几个插件。
    fn uninstall_prompt(&self, i: usize, p: &Plugin, cx: &mut Context<Self>) -> Option<Div> {
        let repo =
            p.repo.clone().filter(|r| self.plugins_form.confirm_uninstall.as_ref() == Some(r))?;
        let siblings = self.plugins.iter().filter(|q| q.repo.as_ref() == Some(&repo)).count();
        let folder = repo.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let what = if siblings > 1 {
            format!("从插件目录删除 {folder} 文件夹（连同其中 {siblings} 个插件）？")
        } else {
            format!("从插件目录删除 {folder} 文件夹？")
        };
        Some(
            div()
                .h_flex()
                .items_center()
                .gap_2()
                .child(
                    div().flex_1().min_w_0().text_size(TEXT_SMALL).text_color(rgb(RED)).child(what),
                )
                .child(
                    Self::text_button(("plugin-uninstall-yes", i), "删除", false)
                        .small()
                        .with_variant(ButtonVariant::Danger)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.uninstall_plugin(repo.clone(), cx)
                        })),
                )
                .child(
                    Self::text_button(("plugin-uninstall-no", i), "取消", false)
                        .small()
                        .with_variant(ButtonVariant::Secondary)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.plugins_form.confirm_uninstall = None;
                            cx.notify();
                        })),
                ),
        )
    }
}

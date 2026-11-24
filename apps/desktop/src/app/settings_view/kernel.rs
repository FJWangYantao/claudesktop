//! 设置页「内核」一栏的绘制。

use super::*;

impl ChatApp {
    /// 「内核与模型」拆页后的「内核」：本机内核清单 + 自研内核的选项（自动模式、运行参数）。
    pub(super) fn render_kernel_tab(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let f = &self.settings_form;
        let body = div().v_flex().gap_4().child(self.render_kernel_list(window, cx));
        match f.kernel {
            KernelChoice::Builtin => body
                .child(self.render_auto_mode(window, cx))
                .child(self.render_suggest(cx))
                .child(self.render_memory_options(cx))
                .child(self.render_chat(cx))
                .child(
                    section(
                        "运行",
                        None,
                        compact_setting_group([
                            setting_row(
                                "一轮最多几步",
                                div().w(NUMBER_INPUT_W).child(Self::text_input_with(
                                    Input::new(&f.max_steps)
                                        .small()
                                        .appearance(false)
                                        .bordered(false),
                                )),
                            ),
                            setting_row(
                                "子任务模型",
                                div()
                                    .w(rems(20.))
                                    .child(Input::new(&f.subagent_model).small().cleanable(true)),
                            ),
                        ]),
                    )
                    .gap_2(),
                )
                .child(self.render_advanced(window, cx)),
            KernelChoice::Acp(_) | KernelChoice::Demo => {
                body.child(self.render_advanced(window, cx))
            }
        }
    }

    pub(super) fn render_kernel_list(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let f = &self.settings_form;
        let compact = f.content_w.get().unwrap_or(FORM_W) < rems(34.).to_pixels(window.rem_size());
        let status_w = if compact { rems(9.) } else { rems(12.) };
        let action_w = rems(4.5);
        let row = |id: SharedString,
                   icon: Icon,
                   name: SharedString,
                   status: SharedString,
                   failed: bool,
                   control: Option<AnyElement>| {
            let name_tip = name.clone();
            let status_tip = status.clone();
            let name_id = ElementId::Name(format!("{id}-name").into());
            let status_id = ElementId::Name(format!("{id}-status").into());
            div()
                .id(ElementId::Name(id))
                .h_flex()
                .items_center()
                .gap_2()
                .h(rems(2.25))
                .flex_none()
                .px_2()
                .border_b_1()
                .border_color(rgb(LINE))
                .hover(|d| d.bg(rgb(HOVER)))
                .child(
                    div()
                        .id(name_id)
                        .h_flex()
                        .items_center()
                        .gap_2()
                        .flex_1()
                        .min_w_0()
                        .tooltip(move |window, cx| {
                            crate::app::chat::text_tip(&name_tip, window, cx)
                        })
                        .child(icon.size_4().flex_none().text_color(rgb(INK)))
                        .child(
                            div()
                                .min_w_0()
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .text_size(TEXT_SMALL)
                                .child(name),
                        ),
                )
                .child(
                    div()
                        .id(status_id)
                        .w(status_w)
                        .flex_none()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_size(TEXT_META)
                        .text_color(rgb(if failed { RED } else { INK_SOFT }))
                        .font_features(tabular_nums())
                        .tooltip(move |window, cx| {
                            crate::app::chat::text_tip(&status_tip, window, cx)
                        })
                        .child(status),
                )
                .child(div().w(action_w).flex_none().h_flex().justify_center().children(control))
        };
        let mut list = div().v_flex().child(
            div()
                .h_flex()
                .items_center()
                .gap_2()
                .px_2()
                .py_1()
                .border_b_1()
                .border_color(rgb(LINE))
                .text_size(TEXT_META)
                .text_color(rgb(INK_SOFT))
                .child(div().flex_1().min_w_0().child("内核名称"))
                .child(div().w(status_w).flex_none().child("版本 / 状态"))
                .child(div().w(action_w).flex_none().text_center().child("启用")),
        );
        let mut risks =
            div().v_flex().gap_1().text_size(TEXT_META).text_color(rgb(INK_SOFT)).child(
                "风险：订阅账号经 magpie（第三方网关）使用可能违反各家条款；\
                 magpie 会读取各 agent 的登录信息，claudesktop 自己不碰凭据",
            );
        let m = &f.magpie;
        let exe_installed = claudesktop_core::default_data_dir()
            .ok()
            .map(|d| claudesktop_builtin::magpie::installed_path(&d))
            .is_some_and(|p| p.is_file());
        let mut status_error = false;
        let status: SharedString = if let Some((done, total)) = m.downloading {
            format!("正在下载… {} / {} MB", done >> 20, total.max(1) >> 20).into()
        } else if m.error.is_some() {
            status_error = true;
            "操作失败".into()
        } else if m.starting {
            "启动中…".into()
        } else if m.probing && !m.probed {
            "检测中…".into()
        } else if let Some(version) = &m.running {
            format!("运行中 · {version}").into()
        } else if exe_installed {
            "未运行".into()
        } else if m.probed {
            "未安装".into()
        } else {
            "检测中…".into()
        };
        let action: Option<(&'static str, &'static str)> =
            if m.downloading.is_some() || m.starting || m.probing {
                None
            } else if m.error.is_some() {
                Some(("重试", "magpie-row-retry"))
            } else if m.running.is_some() {
                None
            } else if exe_installed {
                Some(("启动", "magpie-row-start"))
            } else if m.probed {
                Some(("下载", "magpie-row-setup"))
            } else {
                None
            };
        let trailing = action.map(|(label, id)| {
            Self::text_button(id, label, false)
                .xsmall()
                .tooltip(if id == "magpie-row-setup" {
                    "下载并启动（约 75 MB）"
                } else {
                    label
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.setup_magpie(cx);
                }))
                .into_any_element()
        });
        list = list.child(row(
            "kernel-magpie".into(),
            kernel_logo("magpie"),
            "magpie 网关".into(),
            status,
            status_error,
            trailing,
        ));

        // 本机 agent：检测到的直接列出，勾选即接入（启用 = 出现在输入框的内核菜单里）
        for preset in claudesktop_core::agents::PRESETS {
            let enabled = f.agents.iter().any(|a| a.id == preset.id);
            let detect = match &f.agent_detect {
                AgentDetect::Idle | AgentDetect::Running => None,
                AgentDetect::Done(found) => found.iter().find(|d| d.preset.id == preset.id),
            };
            // 没装 / 版本不对：给「如何安装 / 如何升级」小按钮（复制命令到剪贴板）；
            // 装好的：开关接入。检测中只显示说明。
            let trailing: Option<AnyElement> = match detect {
                Some(d) if d.state == claudesktop_core::agents::DetectState::Ready => Some(
                    Checkbox::new(format!("agent-sw-{}", preset.id))
                        .small()
                        .accessibility_label(format!("启用 {}", preset.name))
                        .checked(enabled)
                        .on_change(cx.listener(move |this, checked: &bool, _, cx| {
                            if *checked {
                                if !this.settings_form.agents.iter().any(|a| a.id == preset.id) {
                                    this.settings_form
                                        .agents
                                        .push(AgentProfile::from_preset(preset));
                                }
                            } else {
                                this.settings_form.agents.retain(|a| a.id != preset.id);
                            }
                            cx.notify();
                        }))
                        .into_any_element(),
                ),
                Some(d) => {
                    let hint = d.preset.install_hint.to_string();
                    let label = d.fix_label();
                    Some(
                        Self::text_button(format!("agent-fix-{}", preset.id), label, false)
                            .xsmall()
                            .tooltip(d.summary())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                    hint.clone(),
                                ));
                                this.toast(Notification::success("安装命令已复制"), cx);
                            }))
                            .into_any_element(),
                    )
                }
                None if matches!(f.agent_detect, AgentDetect::Done(_)) => {
                    let hint = preset.install_hint.to_string();
                    Some(
                        Self::text_button(
                            format!("agent-install-{}", preset.id),
                            "如何安装",
                            false,
                        )
                        .xsmall()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(hint.clone()));
                            this.toast(Notification::success("安装命令已复制"), cx);
                        }))
                        .into_any_element(),
                    )
                }
                None => None,
            };
            // 行的说明：装没装 / 版本
            let desc: String = match detect {
                Some(d) => d.summary(),
                None => match &f.agent_detect {
                    AgentDetect::Idle | AgentDetect::Running => "检测中…".into(),
                    AgentDetect::Done(_) => "未安装".into(),
                },
            };
            let needs_fix = detect.is_some_and(|d| !d.is_ready());
            list = list.child(row(
                format!("agent-{}", preset.id).into(),
                kernel_logo(preset.id),
                preset.name.into(),
                desc.into(),
                needs_fix,
                trailing,
            ));
            if let Some(risk) = preset.risk {
                risks = risks.child(risk);
            }
        }

        div()
            .v_flex()
            .gap_2()
            .child(list)
            .when_some(m.error.as_ref(), |d, error| {
                d.child(div().text_size(TEXT_SMALL).text_color(rgb(RED)).child(error.clone()))
            })
            .child(risks)
    }

    /// 页底「高级」折叠区：agent 选项、自定义命令、用量记录与单价表。
    /// 手填端点档案在「服务商」页；这里的开关保存才生效。
    pub(super) fn render_advanced(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let f = &self.settings_form;
        let gateway_rows: Vec<_> = f
            .agents
            .iter()
            .filter(|a| a.supports_gateway())
            .map(|a| {
                let id = a.id.clone();
                let name = a.display_name();
                setting_row(
                    format!("{} 经过用量网关", name),
                    Switch::new(format!("agent-gw-{}", id))
                        .small()
                        .checked(a.gateway)
                        .color(rgb(CORAL))
                        .on_change(cx.listener(move |this, checked: &bool, _, cx| {
                            if let Some(a) =
                                this.settings_form.agents.iter_mut().find(|a| a.id == id)
                            {
                                a.gateway = *checked;
                            }
                            cx.notify();
                        })),
                )
            })
            .collect();
        let content = div()
            .v_flex()
            .gap_4()
            .when(!gateway_rows.is_empty(), |d| {
                d.child(section("agent 选项", None, compact_setting_group(gateway_rows)).gap_2())
            })
            .child(
                section(
                    "自定义 agent",
                    None,
                    form_field(
                        "启动命令",
                        Self::text_input_with(
                            Input::new(&f.custom_command).small().appearance(false).bordered(false),
                        ),
                    ),
                )
                .gap_2(),
            )
            .when(f.kernel == KernelChoice::Builtin, |d| {
                let mut rows = vec![self.gateway_row(cx)];
                if f.usage_gateway {
                    rows.push(self.bodies_row(cx));
                }
                rows.push(self.prices_row(window, cx));
                d.child(section("用量记录", None, compact_setting_group(rows)).gap_2())
            });
        self.disclosure("kernel-advanced", "高级", content, window, cx)
    }

    /// 「自动」模式：开关 + 分类器用哪个模型 + 信任边界。默认关，开了必须配齐。
    pub(super) fn render_auto_mode(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let f = &self.settings_form;
        let enabled = f.auto_enabled;
        let follow = f.auto_follow;
        let switch =
            Switch::new("auto-mode-on").small().checked(enabled).color(rgb(CORAL)).on_change(
                cx.listener(|this, checked: &bool, _, cx| {
                    this.settings_form.auto_enabled = *checked;
                    cx.notify();
                }),
            );
        let follow_switch =
            Switch::new("auto-mode-follow").small().checked(follow).color(rgb(CORAL)).on_change(
                cx.listener(|this, checked: &bool, _, cx| {
                    this.settings_form.auto_follow = *checked;
                    cx.notify();
                }),
            );
        let mut rows = vec![setting_row("启用「自动」", switch)];
        if enabled {
            rows.push(setting_row("分类器跟随当前模型", follow_switch));
            if !follow {
                rows.push(setting_row(
                    "分类器模型",
                    div().w(rems(20.)).child(Input::new(&f.auto_model).small().cleanable(true)),
                ));
            }
            rows.push(setting_row(
                "信任边界",
                div()
                    .w(rems(20.))
                    .max_w_full()
                    .child(Textarea::new(&f.auto_environment).small().appearance(false)),
            ));
        }
        let rows = compact_setting_group(rows);
        // 没配好时把话说在前面（保存时也会拦）
        let unconfigured = enabled && !self.settings.builtin.is_configured();
        let body = div()
            .v_flex()
            .gap_3()
            .child(rows)
            .when(unconfigured, |d| {
                d.child(note(IconAll::Info, "先在「服务商」页配好端点，再到「模型」页选模型"))
            })
            .when(enabled, |d| {
                d.child(self.disclosure(
                    "auto-mode-rules",
                    "它拦什么、放什么",
                    auto_mode_docs(),
                    window,
                    cx,
                ))
            });
        section("自动模式", None, body).gap_2()
    }

    /// 聊天空间的一组：用哪个模型、新建的聊天默认开不开联网、带一份「聊天说明」文件。
    /// 只影响之后新建的聊天（说明例外：每轮重读，改了下一轮就生效）。
    pub(super) fn render_chat(&self, cx: &mut Context<Self>) -> Div {
        let f = &self.settings_form;
        let path = f.chat_instructions.read(cx).value().trim().to_string();
        let pick =
            Self::quiet_button("chat-instructions-pick", IconAll::FolderOpen, Surface::Paper, cx)
                .small()
                .tooltip("选择一个说明文件")
                .on_click(cx.listener(|_, _, window, cx| {
                    let rx = cx.prompt_for_paths(PathPromptOptions {
                        files: true,
                        directories: false,
                        multiple: false,
                        prompt: Some("选择聊天说明文件".into()),
                    });
                    cx.spawn_in(window, async move |this, cx| {
                        if let Ok(Ok(Some(paths))) = rx.await
                            && let Some(path) = paths.into_iter().next()
                        {
                            let _ = this.update_in(cx, |app, window, cx| {
                                let text = path.to_string_lossy().into_owned();
                                app.settings_form
                                    .chat_instructions
                                    .update(cx, |state, cx| state.set_value(text, window, cx));
                                cx.notify();
                            });
                        }
                    })
                    .detach();
                }));
        // 填了路径但文件不在：说在前面（不然聊天提示词里悄悄少一截）
        let missing = !path.is_empty() && !std::path::Path::new(&path).is_file();
        section(
            "聊天",
            None,
            div()
                .v_flex()
                .gap_3()
                .child(compact_setting_group([
                    setting_row(
                        "聊天用的模型",
                        div().w(rems(20.)).child(Input::new(&f.chat_model).small().cleanable(true)),
                    ),
                    setting_row(
                        "默认联网",
                        Switch::new("chat-web")
                            .small()
                            .checked(f.chat_web)
                            .color(rgb(CORAL))
                            .on_change(cx.listener(|this, checked: &bool, _, cx| {
                                this.settings_form.chat_web = *checked;
                                cx.notify();
                            })),
                    ),
                    setting_row(
                        "聊天说明",
                        div()
                            .h_flex()
                            .items_center()
                            .gap_1p5()
                            .w(rems(24.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(Input::new(&f.chat_instructions).small()),
                            )
                            .child(pick),
                    ),
                ]))
                .when(missing, |d| d.child(note(IconAll::Info, "这个文件不存在，聊天不会带上它")))
                .children(self.render_no_vision(cx)),
        )
        .gap_2()
    }

    /// 被端点回过 400、确认看不了图片的模型：列出来，能清掉（万一是误判）。
    pub(super) fn render_no_vision(&self, cx: &mut Context<Self>) -> Option<Div> {
        let models = self.settings.no_vision_models.clone();
        if models.is_empty() {
            return None;
        }
        let mut chips = div().h_flex().flex_wrap().gap_1p5();
        for (i, model) in models.iter().enumerate() {
            let name: SharedString = model.clone().into();
            let picked = model.clone();
            chips = chips.child(
                div()
                    .id(("no-vision", i))
                    .h_flex()
                    .items_center()
                    .gap_1p5()
                    .pl_2()
                    .pr_1()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(SAND))
                    .text_size(TEXT_SMALL)
                    .text_color(rgb(INK_SOFT))
                    .child(div().font_family(MONO).child(name))
                    .child(
                        Self::quiet_button(("no-vision-drop", i), IconAll::X, Surface::Sand, cx)
                            .xsmall()
                            .tooltip("这个模型其实能看图")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let gone = picked.clone();
                                this.save_ui_pref(
                                    move |s| s.no_vision_models.retain(|m| *m != gone),
                                    cx,
                                );
                                this.push_no_vision_models();
                                cx.notify();
                            })),
                    ),
            );
        }
        Some(
            div()
                .v_flex()
                .gap_2()
                .child(note(IconAll::Info, "这些模型发图时被端点拒过，之后附上图会先提醒你换模型"))
                .child(chips),
        )
    }

    /// 输入补全：两个独立的开关 + 用哪个模型。每次都是一次请求，所以默认关、推荐另填小模型。
    pub(super) fn render_suggest(&self, cx: &mut Context<Self>) -> Div {
        let f = &self.settings_form;
        let typing = Switch::new("suggest-on")
            .small()
            .checked(f.suggest_enabled)
            .color(rgb(CORAL))
            .on_change(cx.listener(|this, checked: &bool, _, cx| {
                this.settings_form.suggest_enabled = *checked;
                cx.notify();
            }));
        let next = Switch::new("next-suggestion-on")
            .small()
            .checked(f.next_suggestion)
            .color(rgb(CORAL))
            .on_change(cx.listener(|this, checked: &bool, _, cx| {
                this.settings_form.next_suggestion = *checked;
                // 关掉时把已经猜出来的那句淡色字也撤掉
                if !*checked {
                    this.next_suggestion.clear();
                    this.refresh_composer(cx);
                }
                cx.notify();
            }));
        let mut rows =
            vec![setting_row("打字时续写", typing), setting_row("一轮结束后猜下一句", next)];
        if f.suggest_enabled || f.next_suggestion {
            rows.push(setting_row(
                "补全模型",
                div().w(rems(20.)).child(Input::new(&f.suggest_model).small().cleanable(true)),
            ));
        }
        section("输入补全", None, compact_setting_group(rows)).gap_2()
    }

    /// 记忆：一轮结束后要不要让模型提炼「候选」（只进设置页待确认，收下才落盘）。
    pub(super) fn render_memory_options(&self, cx: &mut Context<Self>) -> Div {
        let f = &self.settings_form;
        let suggest = Switch::new("memory-suggest")
            .small()
            .checked(f.memory_suggest)
            .color(rgb(CORAL))
            .on_change(cx.listener(|this, checked: &bool, _, cx| {
                this.settings_form.memory_suggest = *checked;
                cx.notify();
            }));
        section("记忆", None, compact_setting_group([setting_row("回合结束后提炼记忆", suggest)]))
            .gap_2()
    }
}

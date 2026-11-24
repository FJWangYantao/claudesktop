//! 设置页「模型」一栏的绘制。

use super::*;

impl ChatApp {
    /// 全局默认模型跨来源选择；来源下拉只筛选下面用于管理的模型清单。
    pub(super) fn render_model_tab(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let f = &self.settings_form;
        let app = cx.entity();
        let selected = f.profiles.get(f.active).zip(f.inputs.get(f.active));
        let label: SharedString = selected.filter(|(p, _)| !p.model.trim().is_empty()).map_or_else(
            || "选择默认模型".into(),
            |(p, inputs)| {
                let model = p.model.trim();
                let source =
                    profile_title(p, &value(&inputs.name, cx), &value(&inputs.base_url, cx));
                format!("{model} · {source}").into()
            },
        );
        let default_model = menu_popover(
            "default-model-menu",
            Anchor::BottomLeft,
            rems(20.),
            &app,
            &self.menu_focus,
            MenuChip::new(IconAll::Bot, label).shrink(),
            default_model_menu_items,
        )
        .default_open(std::env::var("CLAUDESKTOP_OPEN").as_deref() == Ok("model-menu"));
        let mut defaults = div()
            .v_flex()
            .gap_2()
            .child(form_field("默认模型", div().h_flex().min_w_0().child(default_model)));
        if let Some((p, _)) = selected {
            defaults = defaults.child(self.render_thinking_row(p, &p.model, cx));
        }
        div()
            .v_flex()
            .h_full()
            .min_h_0()
            .gap_4()
            .child(defaults)
            .child(self.render_model_list(window, cx))
    }

    pub(super) fn render_model_list(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let f = &self.settings_form;
        let search = value(&f.model_search, cx);
        let options = model_options(&f.profiles, f.model_source, &search);
        let source =
            f.model_source.and_then(|i| f.profiles.get(i).zip(f.inputs.get(i))).map_or_else(
                || SharedString::from("全部来源"),
                |(p, inputs)| {
                    profile_title(p, &value(&inputs.name, cx), &value(&inputs.base_url, cx))
                },
            );
        let toolbar = div()
            .h_flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .flex_none()
            .child(div().flex_1().min_w(rems(9.)).child(Self::text_input(&f.model_search)))
            .child(menu_popover(
                "model-provider-menu",
                Anchor::BottomLeft,
                rems(20.),
                &cx.entity(),
                &self.menu_focus,
                MenuChip::new(IconAll::Server, source).filled().shrink(),
                profile_menu_items,
            ))
            .child(
                Self::text_button("fetch-models", "刷新", false)
                    .small()
                    .loading(self.fetching_models)
                    .disabled(self.fetching_models || f.profiles.is_empty())
                    .on_click(cx.listener(|this, _, _, cx| this.refresh_model_sources(cx))),
            );
        let mut body = div().v_flex().flex_1().min_h_0().gap_2().child(toolbar);
        if let Some(source) = f.model_source
            && !search.is_empty()
            && !options.iter().any(|(_, model)| *model == search)
        {
            let query = search.clone();
            body = body.child(div().h_flex().child(
                Self::text_button("use-custom-model", "使用此模型", false).small().on_click(
                    cx.listener(move |this, _, window, cx| {
                        this.select_form_model(source, query.clone(), window, cx);
                    }),
                ),
            ));
        }
        if let Some((error, text, _)) = &f.fetch_result {
            body = body.child(
                div()
                    .text_size(TEXT_SMALL)
                    .text_color(rgb(if *error { RED } else { GREEN }))
                    .child(text.clone()),
            );
        }
        if options.is_empty() {
            return body.child(div().py_4().text_size(TEXT_SMALL).text_color(rgb(INK_SOFT)).child(
                if !search.is_empty() {
                    "没有匹配的模型"
                } else if f.profiles.is_empty() {
                    "先在「服务商」页添加一个来源"
                } else {
                    "还没有模型列表，点「刷新」获取"
                },
            ));
        }
        let compact_rows =
            f.content_w.get().unwrap_or(FORM_W) < rems(34.).to_pixels(window.rem_size());
        let source_w = if compact_rows { rems(5.) } else { rems(8.) };
        let action_w = if compact_rows { rems(2.25) } else { rems(5.) };
        let columns = div()
            .h_flex()
            .items_center()
            .gap_2()
            .px_2()
            .pr_4()
            .py_1()
            .text_size(TEXT_META)
            .text_color(rgb(INK_SOFT))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child("模型名称"),
            )
            .when(f.model_source.is_none(), |d| {
                d.child(div().w(source_w).flex_none().child("来源"))
            })
            .child(div().w(action_w).flex_none())
            .child(div().w(rems(2.)).flex_none().text_center().child("菜单"));
        let row_h = rems(2.25).to_pixels(window.rem_size());
        let start =
            ((-f.model_scroll.offset().y / row_h) as usize).saturating_sub(4).min(options.len());
        let visible = ((f.model_scroll.bounds().size.height / row_h).ceil() as usize).max(12) + 8;
        let end = (start + visible).min(options.len());
        let mut rows = div()
            .id("settings-model-rows")
            .v_flex()
            .h_full()
            .overflow_y_scroll()
            .track_scroll(&f.model_scroll)
            .pr_4()
            .child(div().h(row_h * start as f32).flex_shrink_0());
        for &(i, model) in &options[start..end] {
            let p = &f.profiles[i];
            let is_default = i == f.active && p.model.trim() == model;
            let pick = model.to_string();
            let toggle = model.to_string();
            let id: SharedString = format!("settings-model-{i}-{model}").into();
            let hover_id = id.clone();
            let opacity = motion::hover_opacity(
                ElementId::Name(format!("actions-{id}").into()),
                f.hovered_action.as_ref() == Some(&id),
                window,
                cx,
            );
            let source =
                profile_title(p, &value(&f.inputs[i].name, cx), &value(&f.inputs[i].base_url, cx));
            rows = rows.child(
                div()
                    .id(ElementId::Name(id.clone()))
                    .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                        this.hover_settings_action(&hover_id, *hovered, cx)
                    }))
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .h(row_h)
                    .flex_none()
                    .px_2()
                    .border_b_1()
                    .border_color(rgb(LINE))
                    .when(is_default, |d| d.bg(rgb(SELECTED)))
                    .when(!is_default, |d| d.hover(|s| s.bg(rgb(HOVER))))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(TEXT_SMALL)
                            .child(model.to_string()),
                    )
                    .when(f.model_source.is_none(), |d| {
                        d.child(
                            div()
                                .w(source_w)
                                .flex_none()
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .text_size(TEXT_META)
                                .text_color(rgb(INK_SOFT))
                                .child(source.clone()),
                        )
                    })
                    .child(div().w(action_w).flex_none().h_flex().justify_end().map(|d| {
                        if is_default {
                            d.child(div().text_size(TEXT_META).text_color(rgb(INK)).child("默认"))
                        } else {
                            d.child(
                                Self::quiet_button(
                                    ElementId::Name(format!("default-{id}").into()),
                                    IconAll::Star,
                                    Surface::Paper,
                                    cx,
                                )
                                .small()
                                .when(!compact_rows, |b| b.label("设为默认"))
                                .tooltip("设为默认")
                                .opacity(opacity)
                                .focus_visible(|s| s.opacity(1.))
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        this.select_form_model(i, pick.clone(), window, cx)
                                    },
                                )),
                            )
                        }
                    }))
                    .child(
                        div().w(rems(2.)).flex_none().h_flex().justify_center().child(
                            Checkbox::new(ElementId::Name(format!("visible-{id}").into()))
                                .small()
                                .accessibility_label(format!("在菜单显示 {source} 的 {model}"))
                                .checked(!p.menu_hidden.iter().any(|m| m == model))
                                .on_change(cx.listener(move |this, checked: &bool, _, cx| {
                                    if let Some(p) = this.settings_form.profiles.get_mut(i) {
                                        p.menu_hidden.retain(|m| m != &toggle);
                                        if !*checked {
                                            p.menu_hidden.push(toggle.clone());
                                        }
                                    }
                                    cx.notify();
                                })),
                        ),
                    ),
            );
        }
        rows = rows.child(div().h(row_h * (options.len() - end) as f32).flex_shrink_0());
        body.child(columns).child(
            div()
                .relative()
                .flex_1()
                .min_h_0()
                .child(rows)
                .child(Scrollbar::vertical(&f.model_scroll).mode(ScrollbarMode::Always)),
        )
    }

    /// 「思考强度」：紧凑下拉，只编辑全局默认模型所属的档案。
    ///
    /// 能选哪些档跟着选中的全局默认模型现算，
    /// 所以换模型之后这一排跟着变；存在档案里的档位如果新模型不支持，高亮和保存都按
    /// `ThinkingSpec::resolve` 降级后的那一档。
    ///
    /// 模型认不出时照样给这一排，但只有「默认 / 兜底」两项：兜底按 `FALLBACK_LEVELS`
    /// 发 effort，其余档位不发（见 `apply_thinking`）。以前整行不出现，用户看着设置页
    /// 以为没地方调（其实是被能力表挡掉了）。
    pub(super) fn render_thinking_row(
        &self,
        p: &ProviderProfile,
        model: &str,
        cx: &mut Context<Self>,
    ) -> Div {
        // 档位按模型现算：各家能选的档差了不止一点（DeepSeek 没有中档、GLM-5.3 关不掉、
        // Gemini 3 Pro 只有低/高），所以选项一律来自 `spec.choices()`，不手写固定列表
        let spec = claudesktop_builtin::thinking_spec(p.protocol, model);
        let current = p.params.thinking;
        let options = thinking_options(&spec);
        let active = active_thinking(&spec, current);
        let label = options
            .iter()
            .find(|(level, _)| *level == active)
            .map_or("默认", |(_, label)| *label);
        form_field(
            "思考强度",
            div().h_flex().child(menu_popover(
                "settings-thinking-menu",
                Anchor::BottomLeft,
                rems(12.),
                &cx.entity(),
                &self.menu_focus,
                MenuChip::new(IconAll::Brain, label).filled(),
                settings_thinking_menu_items,
            )),
        )
    }

    /// 服务端工具：一排开关（沙色面板里的行，和 MCP / 插件那两页一个样式）。
    /// 只有 Anthropic 原生协议认，别的协议下整块不显示（填了也不生效，免得误会）。
    pub(super) fn render_server_tools(&self, p: &ProviderProfile, cx: &mut Context<Self>) -> Div {
        let mut tools = choice_panel();
        for (i, (name, desc)) in
            claudesktop_builtin::RequestParams::server_tool_choices().enumerate()
        {
            let on = p.params.server_tools.iter().any(|s| s == name);
            tools = tools.child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .rounded_lg()
                    .child(
                        div()
                            .v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_size(TEXT_BODY)
                                    .text_color(rgb(INK))
                                    .child(name.to_string()),
                            )
                            .child(
                                div().text_size(TEXT_SMALL).text_color(rgb(INK_SOFT)).child(desc),
                            ),
                    )
                    .child(
                        Switch::new(("server-tool", i)).checked(on).color(rgb(CORAL)).on_change(
                            cx.listener(move |this, checked: &bool, _, cx| {
                                let checked = *checked;
                                if let Some(profile) =
                                    this.settings_form.profiles.get_mut(this.settings_form.view)
                                {
                                    let list = &mut profile.params.server_tools;
                                    list.retain(|s| s != name);
                                    if checked {
                                        list.push(name.to_string());
                                    }
                                }
                                cx.notify();
                            }),
                        ),
                    ),
            );
        }
        div().v_flex().gap_4().child(form_field("服务端工具", tools))
    }
}

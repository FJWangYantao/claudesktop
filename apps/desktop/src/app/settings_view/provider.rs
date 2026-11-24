//! 设置页「服务商」一栏的绘制。

use super::*;

impl ChatApp {
    pub(super) fn render_provider_tab(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let f = &self.settings_form;
        let mut list = choice_panel().gap_0();
        let mut order: Vec<usize> = (0..f.profiles.len()).collect();
        order.sort_by_key(|&i| (!f.profiles[i].magpie, i));
        for (row, i) in order.into_iter().enumerate() {
            let p = &f.profiles[i];
            let inputs = &f.inputs[i];
            let active = i == f.view;
            let group: SharedString = format!("provider-row-{i}").into();
            let opacity = motion::hover_opacity(
                ElementId::Name(format!("actions-{group}").into()),
                active || f.hovered_action.as_ref() == Some(&group),
                window,
                cx,
            );
            let actions = (!p.magpie).then(|| {
                let surface = if active { Surface::Paper } else { Surface::Sand };
                div()
                    .h_flex()
                    .flex_none()
                    .gap_1()
                    .child(
                        Self::quiet_button(("profile-edit", i), IconAll::Pencil, surface, cx)
                            .small()
                            .when(!active, |b| b.opacity(opacity).focus_visible(|s| s.opacity(1.)))
                            .tooltip(if active && f.editing_profile {
                                "收起编辑"
                            } else {
                                "编辑这个来源"
                            })
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                let editing = this.settings_form.view != i
                                    || !this.settings_form.editing_profile;
                                this.select_form_profile(i, window, cx);
                                this.settings_form.editing_profile = editing;
                                cx.notify();
                            })),
                    )
                    .child(
                        Self::quiet_button(("profile-remove", i), IconAll::Trash, surface, cx)
                            .small()
                            .when(!active, |b| b.opacity(opacity).focus_visible(|s| s.opacity(1.)))
                            .tooltip("删除这个来源")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.ask_remove_form_profile(i, window, cx);
                            })),
                    )
                    .into_any_element()
            });
            if row > 0 {
                list = list.child(div().mx_3().h(px(1.)).bg(rgb(LINE)).flex_shrink_0());
            }
            let mut entry = div().v_flex().rounded_lg().when(active, |d| d.bg(rgb(CARD)));
            entry = entry.child(
                choice_row(
                    ("provider-source", i),
                    if p.magpie { Icon::new(IconAll::Cpu) } else { provider_icon(&p.base_url) },
                    profile_title(p, &value(&inputs.name, cx), &value(&inputs.base_url, cx)),
                    "",
                    active,
                    actions,
                )
                .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                    this.hover_settings_action(&group, *hovered, cx)
                }))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.select_form_profile(i, window, cx);
                })),
            );
            if active {
                let detail = if !p.magpie && f.editing_profile {
                    self.render_profile_editor(p, inputs, window, cx)
                } else {
                    self.render_profile_summary(p, inputs, cx)
                };
                entry = entry.child(
                    div().mx_3().pt_2().pb_3().border_t_1().border_color(rgb(LINE)).child(detail),
                );
            }
            list = list.child(entry);
        }
        list
    }

    pub(super) fn render_profile_summary(
        &self,
        p: &ProviderProfile,
        inputs: &ProfileInputs,
        cx: &App,
    ) -> Div {
        let field = |label: &'static str, text: SharedString| {
            div()
                .h_flex()
                .items_center()
                .min_w_0()
                .gap_2()
                .text_size(TEXT_SMALL)
                .child(div().flex_none().text_color(rgb(INK_SOFT)).child(label))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_color(rgb(INK))
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(text),
                )
        };
        let endpoint = value(&inputs.base_url, cx);
        let key_set = !inputs.api_key.read(cx).value().trim().is_empty();
        let mut summary = div()
            .v_flex()
            .gap_2()
            .child(field(
                "端点地址",
                if endpoint.is_empty() { "未设置".into() } else { endpoint.into() },
            ))
            .child(
                div()
                    .h_flex()
                    .flex_wrap()
                    .gap_x_5()
                    .gap_y_2()
                    .child(field("协议", p.protocol.label().into()).flex_1().flex_basis(rems(12.)))
                    .child(
                        field("API Key", if key_set { "已设置" } else { "未设置" }.into())
                            .flex_1()
                            .flex_basis(rems(12.)),
                    ),
            );
        if p.protocol == Protocol::Anthropic {
            summary = summary.child(field(
                "服务端工具",
                if p.params.server_tools.is_empty() {
                    "未启用".into()
                } else {
                    p.params.server_tools.join("、").into()
                },
            ));
        }
        summary
    }

    /// 资料卡的编辑表单；默认模型与思考强度在「模型」页设置。
    pub(super) fn render_profile_editor(
        &self,
        p: &ProviderProfile,
        inputs: &ProfileInputs,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        div()
            .v_flex()
            .gap_2p5()
            .child(form_field("名称", Self::text_input(&inputs.name)))
            .child(form_field(
                "协议",
                div().h_flex().child(menu_popover(
                    "profile-protocol-menu",
                    Anchor::BottomLeft,
                    rems(15.),
                    &cx.entity(),
                    &self.menu_focus,
                    MenuChip::new(IconAll::Server, p.protocol.label()).filled(),
                    protocol_menu_items,
                )),
            ))
            .child(form_field("端点地址", Self::text_input(&inputs.base_url)))
            .child(form_field(
                "API Key",
                div().rounded_lg().border_1().border_color(rgb(LINE)).bg(rgb(CARD)).px_1().child(
                    Input::new(&inputs.api_key).appearance(false).bordered(false).mask_toggle(),
                ),
            ))
            .when(p.protocol == Protocol::Anthropic, |d| d.child(self.render_server_tools(p, cx)))
    }

    pub(super) fn render_save_actions(
        &self,
        compact: bool,
        dirty: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        div()
            .h_flex()
            .items_center()
            .justify_end()
            .gap_2()
            .when(!compact && dirty, |d| d.min_w(rems(13.)))
            .h(rems(2.))
            .when(dirty, |d| {
                d.child(div().text_size(TEXT_META).text_color(rgb(INK_SOFT)).child("未保存"))
                    .child(
                        Self::text_button("settings-discard", "撤销更改", false).small().on_click(
                            cx.listener(|this, _, window, cx| {
                                let tab = this.settings_form.tab;
                                this.open_settings_at(tab, window, cx);
                            }),
                        ),
                    )
                    .child(
                        Self::text_button("settings-save", "保存", true)
                            .small()
                            .tooltip("保存内核、服务商和模型的全部改动")
                            .on_click(cx.listener(|this, _, _, cx| this.save_settings(cx))),
                    )
            })
    }
}

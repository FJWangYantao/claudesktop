//! 设置页（内核 / 端点 / key / 模型）与输入框旁的模型下拉。

use super::ChatApp;
use crate::settings::*;
use crate::theme::*;
use claudesktop_builtin::{PermissionRules, ProviderConfig, default_rules_path};
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

/// 设置页的编辑中状态（保存前不影响正在用的设置）。
pub struct SettingsForm {
    kernel: KernelChoice,
    base_url: Entity<InputState>,
    api_key: Entity<InputState>,
    model: Entity<InputState>,
    acp_command: Entity<InputState>,
    /// 表单里获取到的模型（点一下填进模型框）
    models: Vec<String>,
    /// (是否错误, 文本)
    message: Option<(bool, SharedString)>,
    /// 已保存的「总是允许」规则：(项目目录, 规则)；删除即时生效，不等「保存」
    rules: Vec<(String, Vec<String>)>,
}

impl SettingsForm {
    pub fn new(window: &mut Window, cx: &mut Context<ChatApp>) -> Self {
        let input = |placeholder: &'static str, masked: bool, window: &mut Window, cx: &mut Context<ChatApp>| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder).masked(masked))
        };
        Self {
            kernel: KernelChoice::default(),
            base_url: input("http://127.0.0.1:8045/v1", false, window, cx),
            api_key: input("没有可留空", true, window, cx),
            model: input("如 gemini-3.8-flash", false, window, cx),
            acp_command: input("留空 = 内置 mock agent", false, window, cx),
            models: Vec::new(),
            message: None,
            rules: Vec::new(),
        }
    }
}

fn value(input: &Entity<InputState>, cx: &App) -> String {
    input.read(cx).value().trim().to_string()
}

impl ChatApp {
    pub(crate) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let s = self.settings.clone();
        let f = &mut self.settings_form;
        f.kernel = s.kernel();
        f.models = s.builtin.models.clone();
        f.message = None;
        f.rules = PermissionRules::load(default_rules_path()).list();
        for (input, text) in [
            (&f.base_url, s.builtin.base_url),
            (&f.api_key, s.builtin.api_key),
            (&f.model, s.builtin.model),
            (&f.acp_command, s.acp.command),
        ] {
            input.update(cx, |state, cx| state.set_value(text, window, cx));
        }
        self.settings_open = true;
        self.mode_menu_open = false;
        self.model_menu_open = false;
        self.kernel_menu_open = false;
        cx.notify();
    }

    /// 表单当前的值 → 设置（模型列表沿用已保存的，除非表单里重新获取过）。
    fn form_settings(&self, cx: &App) -> Settings {
        let f = &self.settings_form;
        let mut s = self.settings.clone();
        s.kernel = Some(f.kernel);
        s.builtin.base_url = value(&f.base_url, cx);
        s.builtin.api_key = value(&f.api_key, cx);
        s.builtin.model = value(&f.model, cx);
        s.builtin.models = f.models.clone();
        s.acp.command = value(&f.acp_command, cx);
        s
    }

    fn save_settings(&mut self, cx: &mut Context<Self>) {
        let new = self.form_settings(cx);
        if new.kernel() == KernelChoice::Builtin {
            if let Err(err) = new.builtin_config(self.cwd.clone()) {
                self.settings_form.message = Some((true, err.to_string().into()));
                cx.notify();
                return;
            }
        }
        if let Some(path) = &self.settings_path {
            if let Err(err) = new.save(path) {
                self.settings_form.message = Some((true, format!("保存失败：{err}").into()));
                cx.notify();
                return;
            }
        }
        // 只重启配置变了的内核（模型列表变了不算）
        let builtin_key = |s: &Settings| {
            let mut b = s.builtin.clone();
            b.models.clear();
            b
        };
        let restart_builtin = builtin_key(&new) != builtin_key(&self.settings);
        let restart_acp = new.acp != self.settings.acp;
        let default = new.kernel();
        let default_changed = default != self.settings.kernel();
        self.settings = new;
        self.settings_open = false;
        if restart_builtin {
            self.restart_kernel(KernelChoice::Builtin, cx);
        }
        if restart_acp {
            self.restart_kernel(KernelChoice::Acp, cx);
        }
        if default_changed {
            // 默认内核换了：还没对话的会话跟着换，有对话的留在原来的内核上
            for idx in 0..self.sessions.len() {
                self.rebind_session(idx, default);
            }
        }
        cx.notify();
    }

    /// 改一处设置并只把这一处写回文件：生效中的设置可能含有环境变量给的值
    /// （如 key），没点「保存」就不该落盘。
    pub(super) fn update_settings(&mut self, change: impl Fn(&mut Settings)) {
        change(&mut self.settings);
        if let Some(path) = &self.settings_path {
            let (mut saved, _) = Settings::load(Some(path));
            change(&mut saved);
            let _ = saved.save(path);
        }
    }

    /// 后台获取模型列表。`from_form` = 用设置页里正在编辑的端点；否则用生效中的设置。
    pub(crate) fn fetch_models(&mut self, from_form: bool, cx: &mut Context<Self>) {
        if self.fetching_models {
            return;
        }
        let config = if from_form {
            let f = &self.settings_form;
            ProviderConfig {
                base_url: value(&f.base_url, cx),
                api_key: Some(value(&f.api_key, cx)).filter(|k| !k.is_empty()),
                model: String::new(),
            }
        } else {
            self.settings.builtin.provider()
        };
        if config.base_url.is_empty() {
            self.settings_form.message = Some((true, "先填端点地址".into()));
            cx.notify();
            return;
        }
        self.fetching_models = true;
        cx.notify();
        let base_url = config.base_url.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { claudesktop_builtin::fetch_models(config) })
                .await;
            let _ = this.update(cx, |app, cx| {
                app.fetching_models = false;
                match result {
                    Ok(models) => {
                        let n = models.len();
                        if from_form {
                            app.settings_form.models = models.clone();
                            app.settings_form.message = Some((false, format!("获取到 {n} 个模型").into()));
                        }
                        // 和生效中的端点一致时，顺便更新下拉用的列表
                        if base_url == app.settings.builtin.base_url.trim() {
                            app.update_settings(|s| s.builtin.models = models.clone());
                        }
                    }
                    Err(err) => {
                        let text: SharedString = format!("获取模型失败：{err}").into();
                        if from_form {
                            app.settings_form.message = Some((true, text));
                        } else {
                            app.sessions[app.active].note(text);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 删一条「总是允许」规则。内核每次判断前都重读规则文件，所以立即生效。
    fn remove_rule(&mut self, project: String, rule: String, cx: &mut Context<Self>) {
        let mut rules = PermissionRules::load(default_rules_path());
        if let Err(err) = rules.remove(&project, &rule) {
            self.settings_form.message = Some((true, format!("删除规则失败：{err}").into()));
        }
        self.settings_form.rules = rules.list();
        cx.notify();
    }

    fn render_rules(&self, cx: &mut Context<Self>) -> Div {
        let mut section = div()
            .v_flex()
            .gap_2()
            .pt_4()
            .border_t_1()
            .border_color(rgb(LINE))
            .child(
                div()
                    .text_size(px(13.5))
                    .font_weight(FontWeight::MEDIUM)
                    .child("已保存的权限规则"),
            )
            .child(
                div()
                    .text_size(px(12.5))
                    .text_color(rgb(INK_SOFT))
                    .child("自研内核里在权限卡片上选「总是允许」时记下的，按项目生效；删除后立即生效。"),
            );
        let rules = &self.settings_form.rules;
        if rules.is_empty() {
            return section.child(
                div()
                    .py_2()
                    .text_size(px(13.))
                    .text_color(rgb(INK_FAINT))
                    .child("还没有规则"),
            );
        }
        let mut n = 0usize;
        for (project, list) in rules {
            let mut card = div()
                .v_flex()
                .rounded_lg()
                .border_1()
                .border_color(rgb(LINE))
                .bg(rgb(CARD))
                .child(
                    div()
                        .h_flex()
                        .gap_2()
                        .px_3()
                        .py_2()
                        .border_b_1()
                        .border_color(rgb(LINE))
                        .child(Icon::new(IconAll::Folder).size_3p5().text_color(rgb(INK_SOFT)))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .font_family(MONO)
                                .text_size(px(12.5))
                                .text_color(rgb(INK))
                                .child(project.clone()),
                        ),
                );
            for rule in list {
                let (p, r) = (project.clone(), rule.clone());
                card = card.child(
                    div()
                        .h_flex()
                        .gap_2()
                        .px_3()
                        .py_1p5()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(px(13.))
                                .text_color(rgb(INK))
                                .child(rule_label(rule)),
                        )
                        .child(
                            div()
                                .id(("rule-remove", n))
                                .p_1()
                                .rounded_md()
                                .cursor_pointer()
                                .hover(|d| d.bg(rgb(SAND)))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.remove_rule(p.clone(), r.clone(), cx)
                                }))
                                .child(Icon::new(IconAll::X).size_3p5().text_color(rgb(INK_SOFT))),
                        ),
                );
                n += 1;
            }
            section = section.child(card);
        }
        section
    }

    fn import_antigravity(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((base_url, key)) = antigravity_gateway() else {
            return;
        };
        let f = &self.settings_form;
        f.base_url.update(cx, |s, cx| s.set_value(base_url.clone(), window, cx));
        f.api_key.update(cx, |s, cx| s.set_value(key, window, cx));
        self.settings_form.message = Some((
            false,
            format!("已填入 Antigravity Tools 本地网关 {base_url}（需要它的反代服务在运行）").into(),
        ));
        self.fetch_models(true, cx);
    }

    /// 在模型下拉里选了一个：当前会话立即切换（还没建立的会话等 Started），并记为默认。
    pub(crate) fn choose_model(&mut self, model: String, cx: &mut Context<Self>) {
        self.model_menu_open = false;
        self.update_settings(|s| s.builtin.model = model.clone());
        let kernel = self.session_kernel(self.active);
        let s = &mut self.sessions[self.active];
        match (kernel, s.acp_id.clone()) {
            (Some(kernel), Some(id)) => match kernel.set_model(&id, &model) {
                Ok(()) => s.model_label = model.into(),
                Err(err) => s.note(format!("切换模型失败：{err}")),
            },
            _ => {
                s.pending_model = Some(model.clone());
                s.model_label = model.into();
            }
        }
        cx.notify();
    }

    pub(crate) fn toggle_model_menu(&mut self, cx: &mut Context<Self>) {
        self.model_menu_open = !self.model_menu_open;
        self.mode_menu_open = false;
        self.kernel_menu_open = false;
        let builtin = self.session().kernel == KernelChoice::Builtin;
        if self.model_menu_open && builtin && self.settings.builtin.models.is_empty() {
            self.fetch_models(false, cx);
        }
        cx.notify();
    }

    pub(super) fn render_model_menu(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let mut menu = div()
            .id("model-menu")
            .occlude()
            .absolute()
            .right(px(52.))
            .bottom(px(104.))
            .w(px(280.))
            .max_h(px(380.))
            .overflow_y_scroll()
            .v_flex()
            .p_1()
            .gap_0p5()
            .bg(rgb(CARD))
            .border_1()
            .border_color(rgb(LINE))
            .rounded_lg()
            .shadow_md();
        let hint = |text: SharedString| {
            div()
                .px_2p5()
                .py_2()
                .text_size(px(13.))
                .text_color(rgb(INK_SOFT))
                .child(text)
        };
        let builtin = self.session().kernel == KernelChoice::Builtin;
        if !builtin {
            return menu.child(hint("当前内核的模型由 agent 自己决定，这里不能切换".into()));
        }
        let models = &self.settings.builtin.models;
        if models.is_empty() {
            menu = menu.child(hint(if self.fetching_models {
                "正在获取模型列表…".into()
            } else {
                "没有模型列表，可在设置里获取".into()
            }));
        }
        let current = self.session().model_label.clone();
        for (i, model) in models.iter().enumerate() {
            let active = current.as_ref() == model.as_str();
            let m = model.clone();
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
                    .on_click(cx.listener(move |this, _, _, cx| this.choose_model(m.clone(), cx)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(13.5))
                            .font_family(MONO)
                            .text_color(rgb(INK))
                            .child(model.clone()),
                    )
                    .when(active, |d| {
                        d.child(Icon::new(IconAll::Check).size_4().text_color(rgb(CORAL)))
                    }),
            );
        }
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
                .text_size(px(13.))
                .text_color(rgb(INK_SOFT))
                .on_click(cx.listener(|this, _, _, cx| this.fetch_models(false, cx)))
                .child(Icon::new(IconAll::RefreshCw).size_3p5())
                .child("刷新模型列表"),
        )
    }

    fn field(label: &'static str, hint: Option<&'static str>, input: impl IntoElement) -> Div {
        div()
            .v_flex()
            .gap_1p5()
            .child(
                div()
                    .text_size(px(13.5))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(rgb(INK))
                    .child(label),
            )
            .child(input)
            .when_some(hint, |d, h| {
                d.child(div().text_size(px(12.5)).text_color(rgb(INK_SOFT)).child(h))
            })
    }

    fn button(id: &'static str, label: impl Into<SharedString>, primary: bool) -> Stateful<Div> {
        div()
            .id(id)
            .px_3p5()
            .py_1p5()
            .rounded_md()
            .cursor_pointer()
            .text_size(px(13.5))
            .when(primary, |d| {
                d.bg(rgb(CORAL))
                    .text_color(rgb(0xFFFFFF))
                    .hover(|d| d.opacity(0.9))
            })
            .when(!primary, |d| {
                d.border_1()
                    .border_color(rgb(LINE))
                    .bg(rgb(CARD))
                    .text_color(rgb(INK))
                    .hover(|d| d.bg(rgb(SAND)))
            })
            .child(label.into())
    }

    fn text_input(state: &Entity<InputState>) -> Div {
        div()
            .rounded_md()
            .border_1()
            .border_color(rgb(LINE))
            .bg(rgb(CARD))
            .px_1()
            .child(Input::new(state).appearance(false).bordered(false))
    }

    pub(super) fn render_settings(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let f = &self.settings_form;
        let choice = |id: &'static str, kind: KernelChoice, title: &'static str, desc: &'static str| {
            let active = f.kernel == kind;
            div()
                .id(id)
                .flex_1()
                .min_w_0()
                .v_flex()
                .gap_1()
                .p_3()
                .rounded_lg()
                .border_1()
                .border_color(rgb(if active { CORAL } else { LINE }))
                .bg(rgb(CARD))
                .cursor_pointer()
                .hover(|d| d.bg(rgb(PAPER)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.settings_form.kernel = kind;
                    this.settings_form.message = None;
                    cx.notify();
                }))
                .child(
                    div()
                        .h_flex()
                        .gap_1p5()
                        .child(div().text_size(px(14.)).text_color(rgb(INK)).child(title))
                        .when(active, |d| {
                            d.child(Icon::new(IconAll::Check).size_4().text_color(rgb(CORAL)))
                        }),
                )
                .child(div().text_size(px(12.5)).text_color(rgb(INK_SOFT)).child(desc))
        };

        let mut body = div()
            .v_flex()
            .gap_5()
            .child(
                div()
                    .v_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_size(px(13.5))
                            .font_weight(FontWeight::MEDIUM)
                            .child("默认内核"),
                    )
                    .child(
                        div()
                            .h_flex()
                            .gap_2()
                            .items_start()
                            .child(choice(
                                "kernel-builtin",
                                KernelChoice::Builtin,
                                "自研内核",
                                "OpenAI 兼容接口，含 Ollama 和本地网关",
                            ))
                            .child(choice(
                                "kernel-acp",
                                KernelChoice::Acp,
                                "ACP agent",
                                "外部 agent：Claude Code、pi 等",
                            ))
                            .child(choice(
                                "kernel-demo",
                                KernelChoice::Demo,
                                "演示回放",
                                "不连模型，只播放界面演示",
                            )),
                    ),
            );

        match f.kernel {
            KernelChoice::Builtin => {
                let fetch_label = if self.fetching_models { "获取中…" } else { "获取模型列表" };
                let mut model_row = div()
                    .h_flex()
                    .gap_2()
                    .child(div().flex_1().child(Self::text_input(&f.model)))
                    .child(
                        Self::button("fetch-models", fetch_label, false)
                            .on_click(cx.listener(|this, _, _, cx| this.fetch_models(true, cx))),
                    );
                if antigravity_gateway().is_some() {
                    model_row = model_row.child(
                        Self::button("import-antigravity", "从 Antigravity Tools 导入", false).on_click(
                            cx.listener(|this, _, window, cx| this.import_antigravity(window, cx)),
                        ),
                    );
                }
                let current_model = value(&f.model, cx);
                let mut chips = div().flex().flex_wrap().gap_1p5();
                for (i, m) in f.models.iter().enumerate() {
                    let active = *m == current_model;
                    let pick = m.clone();
                    chips = chips.child(
                        div()
                            .id(("model-chip", i))
                            .px_2()
                            .py_0p5()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(if active { CORAL } else { LINE }))
                            .bg(rgb(if active { SAND } else { CARD }))
                            .cursor_pointer()
                            .hover(|d| d.bg(rgb(SAND)))
                            .text_size(px(12.5))
                            .font_family(MONO)
                            .text_color(rgb(INK))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let pick = pick.clone();
                                this.settings_form
                                    .model
                                    .update(cx, |s, cx| s.set_value(pick, window, cx));
                                cx.notify();
                            }))
                            .child(m.clone()),
                    );
                }
                body = body
                    .child(Self::field(
                        "端点地址",
                        Some("OpenAI 兼容的 base URL，不含 /chat/completions"),
                        Self::text_input(&f.base_url),
                    ))
                    .child(Self::field(
                        "API Key",
                        None,
                        div()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(LINE))
                            .bg(rgb(CARD))
                            .px_1()
                            .child(Input::new(&f.api_key).appearance(false).bordered(false).mask_toggle()),
                    ))
                    .child(Self::field("模型", Some("新会话的默认模型；会话里也可以在输入框旁切换"), model_row))
                    .when(!f.models.is_empty(), |d| d.child(chips));
            }
            KernelChoice::Acp => {
                body = body.child(Self::field(
                    "启动命令",
                    Some("例：cmd /c npx -y @agentclientprotocol/claude-agent-acp　或　cmd /c npx -y pi-acp"),
                    Self::text_input(&f.acp_command),
                ));
            }
            KernelChoice::Demo => {}
        }

        if let Some((error, text)) = &f.message {
            body = body.child(
                div()
                    .text_size(px(13.))
                    .text_color(rgb(if *error { RED } else { GREEN }))
                    .child(text.clone()),
            );
        }
        let path = self
            .settings_path
            .as_ref()
            .map_or_else(|| "（数据目录不可用，设置不会保存）".to_string(), |p| p.display().to_string());
        body = body
            .child(
                div()
                    .h_flex()
                    .gap_2()
                    .child(
                        Self::button("settings-save", "保存并应用", true)
                            .on_click(cx.listener(|this, _, _, cx| this.save_settings(cx))),
                    )
                    .child(Self::button("settings-cancel", "取消", false).on_click(cx.listener(
                        |this, _, _, cx| {
                            this.settings_open = false;
                            cx.notify();
                        },
                    ))),
            )
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(rgb(INK_FAINT))
                    .child(format!(
                        "保存在 {path}（API Key 为明文）。启动时环境变量优先于这里的设置。只重启配置变了的内核，它上面进行中的对话会中断，下次发消息时自动续接。"
                    )),
            );

        div()
            .id("settings-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .child(
                div()
                    .v_flex()
                    .max_w(px(720.))
                    .mx_auto()
                    .w_full()
                    .px_6()
                    .py_8()
                    .gap_6()
                    .child(
                        div()
                            .h_flex()
                            .items_center()
                            .child(
                                div()
                                    .flex_1()
                                    .font_family(SERIF)
                                    .text_size(px(28.))
                                    .text_color(rgb(INK))
                                    .child("设置"),
                            )
                            .child(
                                div()
                                    .id("settings-close")
                                    .p_1()
                                    .rounded_md()
                                    .cursor_pointer()
                                    .hover(|d| d.bg(rgb(SAND)))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.settings_open = false;
                                        cx.notify();
                                    }))
                                    .child(Icon::new(IconAll::X).size_4().text_color(rgb(INK_SOFT))),
                            ),
                    )
                    .child(body)
                    .child(self.render_rules(cx)),
            )
    }
}

/// 规则 → 给人看的说明
fn rule_label(rule: &str) -> String {
    match rule.strip_prefix("shell:") {
        Some(prefix) => format!("运行以「{prefix}」开头的命令"),
        None if rule == "edit" => "编辑项目内的文件".into(),
        None => rule.to_string(),
    }
}

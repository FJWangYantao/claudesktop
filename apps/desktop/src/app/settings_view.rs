//! 设置页（内核 / provider 档案 / 模型）与输入框旁的模型下拉。

use super::ChatApp;
use crate::settings::*;
use crate::theme::*;
use crate::theme::rgb;
use claudesktop_builtin::{PermissionRules, Protocol, ProviderConfig, default_rules_path};
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::theme::{Theme, ThemeMode};
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

/// 一套档案在表单里的输入框（与 `SettingsForm::profiles` 一一对应）。
pub struct ProfileInputs {
    name: Entity<InputState>,
    base_url: Entity<InputState>,
    api_key: Entity<InputState>,
    model: Entity<InputState>,
}

impl ProfileInputs {
    fn new(window: &mut Window, cx: &mut Context<ChatApp>, p: &ProviderProfile) -> Self {
        let make = |value: String,
                    placeholder: &'static str,
                    masked: bool,
                    window: &mut Window,
                    cx: &mut Context<ChatApp>| {
            cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(placeholder)
                    .masked(masked)
                    .default_value(value)
            })
        };
        Self {
            name: make(p.name.clone(), "如 DeepSeek（可留空）", false, window, cx),
            base_url: make(p.base_url.clone(), "http://127.0.0.1:8045/v1", false, window, cx),
            api_key: make(p.api_key.clone(), "没有可留空", true, window, cx),
            model: make(p.model.clone(), "如 deepseek-chat", false, window, cx),
        }
    }
}

/// 设置页的编辑中状态（保存前不影响正在用的设置）。
pub struct SettingsForm {
    kernel: KernelChoice,
    /// 编辑中的档案列表（保存才生效）
    profiles: Vec<ProviderProfile>,
    /// 正在编辑第几套
    active: usize,
    /// 每套档案的输入框
    inputs: Vec<ProfileInputs>,
    acp_command: Entity<InputState>,
    /// (是否错误, 文本)
    message: Option<(bool, SharedString)>,
    /// 已保存的「总是允许」规则：(项目目录, 规则)；删除即时生效，不等「保存」
    rules: Vec<(String, Vec<String>)>,
}

impl SettingsForm {
    pub fn new(window: &mut Window, cx: &mut Context<ChatApp>) -> Self {
        let acp_command = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("留空 = 内置 mock agent")
        });
        Self {
            kernel: KernelChoice::default(),
            profiles: Vec::new(),
            active: 0,
            inputs: Vec::new(),
            acp_command,
            message: None,
            rules: Vec::new(),
        }
    }

    /// 正在编辑的档案（没有时给空档案占位）。
    fn current(&self) -> Option<(&ProviderProfile, &ProfileInputs)> {
        let p = self.profiles.get(self.active)?;
        let i = self.inputs.get(self.active)?;
        Some((p, i))
    }
}

fn value(input: &Entity<InputState>, cx: &App) -> String {
    input.read(cx).value().trim().to_string()
}

impl ChatApp {
    pub(crate) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let s = self.settings.clone();
        self.settings_form.kernel = s.kernel();
        self.settings_form.message = None;
        self.settings_form.rules = PermissionRules::load(default_rules_path()).list();
        // 档案：一份都没有时给一套空的可直接填
        let mut profiles = s.builtin.profiles.clone();
        if profiles.is_empty() {
            profiles.push(ProviderProfile::default());
        }
        self.settings_form.active = s.builtin.active.min(profiles.len() - 1);
        self.settings_form.inputs = profiles
            .iter()
            .map(|p| ProfileInputs::new(window, cx, p))
            .collect();
        self.settings_form.profiles = profiles;
        self.settings_form.acp_command.update(cx, |state, cx| {
            state.set_value(s.acp.command.clone(), window, cx)
        });
        self.settings_open = true;
        cx.notify();
    }

    /// 表单当前的值 → 设置（档案里的模型列表沿用已保存的，除非表单里重新获取过）。
    /// 视图模式、外观这类即时生效的偏好：改内存里的设置，并只把这一项写回盘上的设置文件
    /// （内存里的设置合并过环境变量，不能整份写回；文件坏了就不碰，免得用默认值盖掉）。
    pub(crate) fn save_ui_pref(&mut self, edit: impl Fn(&mut Settings)) {
        edit(&mut self.settings);
        if let Some(path) = &self.settings_path {
            let (mut on_disk, broken) = Settings::load(Some(path));
            if broken.is_none() {
                edit(&mut on_disk);
                if let Err(err) = on_disk.save(path) {
                    self.sessions[self.active].note(format!("设置没能保存：{err}"));
                }
            }
        }
    }

    /// 按设置（或系统外观）切浅色 / 深色：我们自己的色板和 gpui-kit 组件（输入框、提示、滚动条）一起切。
    pub(crate) fn apply_appearance(&mut self, window: &Window, cx: &mut Context<Self>) {
        let dark = match self.settings.appearance {
            Appearance::System => {
                matches!(window.appearance(), WindowAppearance::Dark | WindowAppearance::VibrantDark)
            }
            Appearance::Light => false,
            Appearance::Dark => true,
        };
        crate::theme::set_dark(dark);
        let mode = if dark { ThemeMode::Dark } else { ThemeMode::Light };
        if Theme::global(cx).mode != mode {
            Theme::change(mode, None, cx);
        }
        cx.notify();
    }

    pub(crate) fn set_appearance(&mut self, a: Appearance, window: &Window, cx: &mut Context<Self>) {
        self.save_ui_pref(move |s| s.appearance = a);
        self.apply_appearance(window, cx);
    }

    /// 设置页顶部的外观切换（即时生效，不走「保存并应用」）。
    fn render_appearance(&self, cx: &mut Context<Self>) -> Div {
        let current = self.settings.appearance;
        let mut segs = div().h_flex().gap_0p5().p_0p5().rounded_lg().bg(rgb(SAND));
        for (i, a) in Appearance::ALL.into_iter().enumerate() {
            segs = segs.child(
                div()
                    .id(("appearance", i))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, window, cx| this.set_appearance(a, window, cx)))
                    .child(Self::seg(a.label(), a == current)),
            );
        }
        div()
            .v_flex()
            .gap_2()
            .child(div().text_size(TEXT_BODY).font_weight(FontWeight::MEDIUM).child("外观"))
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_3()
                    .child(segs)
                    .child(
                        div()
                            .text_size(TEXT_SMALL)
                            .text_color(rgb(INK_SOFT))
                            .child("立即生效，不用点保存"),
                    ),
            )
    }

    fn form_settings(&self, cx: &App) -> Settings {
        let f = &self.settings_form;
        let mut s = self.settings.clone();
        s.kernel = Some(f.kernel);
        let mut profiles = f.profiles.clone();
        for (p, inputs) in profiles.iter_mut().zip(&f.inputs) {
            p.name = value(&inputs.name, cx);
            p.base_url = value(&inputs.base_url, cx);
            p.api_key = value(&inputs.api_key, cx);
            p.model = value(&inputs.model, cx);
        }
        s.builtin.profiles = profiles;
        s.builtin.active = f.active.min(s.builtin.profiles.len().saturating_sub(1));
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
            for p in &mut b.profiles {
                p.models.clear();
            }
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

    // ---------- 档案编辑（表单内） ----------

    /// 表单里切到第 `i` 套档案。
    fn select_form_profile(&mut self, i: usize, cx: &mut Context<Self>) {
        if i < self.settings_form.profiles.len() {
            self.settings_form.active = i;
            cx.notify();
        }
    }

    /// 表单里删掉第 `i` 套档案。
    fn remove_form_profile(&mut self, i: usize, cx: &mut Context<Self>) {
        if i >= self.settings_form.profiles.len() {
            return;
        }
        self.settings_form.profiles.remove(i);
        self.settings_form.inputs.remove(i);
        let len = self.settings_form.profiles.len();
        let f = &mut self.settings_form;
        if len == 0 {
            f.active = 0;
        } else {
            if i < f.active {
                f.active -= 1;
            }
            f.active = f.active.min(len - 1);
        }
        cx.notify();
    }

    /// 表单里追加一套档案（预设 `preset = None` 时是空白自定义）。
    fn add_form_profile(&mut self, preset: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let p = preset.map_or_else(ProviderProfile::default, |i| {
            let mut p = preset_profiles()[i].clone();
            p.api_key = String::new();
            p.models = Vec::new();
            p
        });
        self.settings_form.inputs.push(ProfileInputs::new(window, cx, &p));
        self.settings_form.profiles.push(p);
        self.settings_form.active = self.settings_form.profiles.len() - 1;
        cx.notify();
    }

    /// 表单里切当前档案的协议。
    fn set_form_protocol(&mut self, protocol: Protocol, cx: &mut Context<Self>) {
        if let Some(p) = self.settings_form.profiles.get_mut(self.settings_form.active) {
            p.protocol = protocol;
            cx.notify();
        }
    }

    /// 直接切换生效中的档案（输入框旁的菜单）：写盘并重启自研内核。
    pub(crate) fn switch_profile(&mut self, i: usize, cx: &mut Context<Self>) {
        if i >= self.settings.builtin.profiles.len() || i == self.settings.builtin.active {
            return;
        }
        self.update_settings(|s| s.builtin.select(i));
        self.restart_kernel(KernelChoice::Builtin, cx);
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

    /// 后台获取模型列表。`from_form` = 用设置页里正在编辑的档案；否则用生效中的设置。
    pub(crate) fn fetch_models(&mut self, from_form: bool, cx: &mut Context<Self>) {
        if self.fetching_models {
            return;
        }
        let config = if from_form {
            let f = &self.settings_form;
            let Some(inputs) = f.inputs.get(f.active) else {
                return;
            };
            ProviderConfig {
                protocol: f
                    .profiles
                    .get(f.active)
                    .map_or(Protocol::OpenAiCompat, |p| p.protocol),
                base_url: value(&inputs.base_url, cx),
                api_key: Some(value(&inputs.api_key, cx)).filter(|k| !k.is_empty()),
                model: String::new(),
                ..Default::default()
            }
        } else {
            self.settings.builtin.provider()
        };
        if config.base_url.is_empty() {
            self.settings_form.message = Some((true, "先填端点地址".into()));
            cx.notify();
            return;
        }
        let form_active = if from_form { Some(self.settings_form.active) } else { None };
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
                        if let Some(i) = form_active {
                            if let Some(p) = app.settings_form.profiles.get_mut(i) {
                                p.models = models.clone();
                            }
                            app.settings_form.message =
                                Some((false, format!("获取到 {n} 个模型").into()));
                        }
                        // 和生效中的端点一致时，顺便更新下拉用的列表
                        if base_url == app.settings.builtin.base_url().trim() {
                            app.update_settings(|s| s.builtin.current_mut().models = models.clone());
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
                    .text_size(TEXT_BODY)
                    .font_weight(FontWeight::MEDIUM)
                    .child("已保存的权限规则"),
            )
            .child(
                div()
                    .text_size(TEXT_SMALL)
                    .text_color(rgb(INK_SOFT))
                    .child("自研内核里在权限卡片上选「总是允许」时记下的，按项目生效；删除后立即生效。"),
            );
        let rules = &self.settings_form.rules;
        if rules.is_empty() {
            return section.child(
                div()
                    .py_2()
                    .text_size(TEXT_SMALL)
                    .text_color(rgb(INK_SOFT))
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
                                .text_size(TEXT_SMALL)
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
                                .text_size(TEXT_SMALL)
                                .text_color(rgb(INK))
                                .child(rule_label(rule)),
                        )
                        .child(
                            div()
                                .id(("rule-remove", n))
                                .p_1()
                                .rounded_md()
                                .cursor_pointer()
                                .hover(|d| d.bg(rgb(HOVER)))
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
        let i = self.settings_form.active;
        let Some(inputs) = self.settings_form.inputs.get(i) else {
            return;
        };
        inputs.base_url.update(cx, |s, cx| s.set_value(base_url.clone(), window, cx));
        inputs.api_key.update(cx, |s, cx| s.set_value(key, window, cx));
        self.settings_form.message = Some((
            false,
            format!("已填入 Antigravity Tools 本地网关 {base_url}（需要它的反代服务在运行）").into(),
        ));
        self.fetch_models(true, cx);
    }

    /// 在模型下拉里选了一个：当前会话立即切换（还没建立的会话等 Started），并记为默认。
    pub(crate) fn choose_model(&mut self, model: String, cx: &mut Context<Self>) {
        self.update_settings(|s| s.builtin.current_mut().model = model.clone());
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

    /// 模型下拉打开时：自研内核且还没有模型列表就顺手获取一次。
    pub(crate) fn fetch_models_for_menu(&mut self, cx: &mut Context<Self>) {
        let builtin = self.session().kernel == KernelChoice::Builtin;
        if builtin && self.settings.builtin.models().is_empty() {
            self.fetch_models(false, cx);
        }
    }

    fn field(label: &'static str, hint: Option<&'static str>, input: impl IntoElement) -> Div {
        div()
            .v_flex()
            .gap_1p5()
            .child(
                div()
                    .text_size(TEXT_BODY)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(rgb(INK))
                    .child(label),
            )
            .child(input)
            .when_some(hint, |d, h| {
                d.child(div().text_size(TEXT_SMALL).text_color(rgb(INK_SOFT)).child(h))
            })
    }

    fn button(id: &'static str, label: impl Into<SharedString>, primary: bool) -> Stateful<Div> {
        div()
            .id(id)
            .px_3p5()
            .py_1p5()
            .rounded_md()
            .cursor_pointer()
            .text_size(TEXT_BODY)
            .when(primary, |d| {
                d.bg(rgb(CORAL_DEEP))
                    .text_color(rgb(WHITE))
                    .hover(|d| d.bg(rgb(CORAL_DEEP_HOVER)))
            })
            .when(!primary, |d| {
                d.border_1()
                    .border_color(rgb(LINE))
                    .bg(rgb(CARD))
                    .text_color(rgb(INK))
                    .hover(|d| d.bg(rgb(HOVER)))
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
                .hover(|d| d.bg(rgb(HOVER)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.settings_form.kernel = kind;
                    this.settings_form.message = None;
                    cx.notify();
                }))
                .child(
                    div()
                        .h_flex()
                        .gap_1p5()
                        .child(div().text_size(TEXT_BODY).text_color(rgb(INK)).child(title))
                        .when(active, |d| {
                            d.child(Icon::new(IconAll::Check).size_4().text_color(rgb(CORAL)))
                        }),
                )
                .child(div().text_size(TEXT_SMALL).text_color(rgb(INK_SOFT)).child(desc))
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
                            .text_size(TEXT_BODY)
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
                // 配置档案切换条：点名字换档案，× 删除；后面跟「＋ 添加」
                let mut profile_row = div().h_flex().flex_wrap().gap_1p5();
                for (i, p) in f.profiles.iter().enumerate() {
                    let active = i == f.active;
                    let label: SharedString = if p.name.trim().is_empty() {
                        format!("档案 {}", i + 1).into()
                    } else {
                        p.name.clone().into()
                    };
                    profile_row = profile_row.child(
                        div()
                            .id(("profile-chip", i))
                            .h_flex()
                            .items_center()
                            .gap_1()
                            .px_2()
                            .py_0p5()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(if active { CORAL } else { LINE }))
                            .bg(rgb(if active { SAND } else { CARD }))
                            .cursor_pointer()
                            .hover(|d| d.bg(rgb(HOVER)))
                            .text_size(TEXT_SMALL)
                            .text_color(rgb(INK))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.select_form_profile(i, cx);
                            }))
                            .child(label)
                            .child(
                                div()
                                    .id(("profile-remove", i))
                                    .p(px(1.))
                                    .rounded_sm()
                                    .cursor_pointer()
                                    .hover(|d| d.bg(rgb(SELECTED)))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.remove_form_profile(i, cx);
                                    }))
                                    .child(
                                        Icon::new(IconAll::X)
                                            .size_3()
                                            .text_color(rgb(INK_SOFT)),
                                    ),
                            ),
                    );
                }
                profile_row = profile_row.child(
                    div()
                        .id("profile-add")
                        .h_flex()
                        .items_center()
                        .gap_1()
                        .px_2()
                        .py_0p5()
                        .rounded_md()
                        .border_1()
                        .border_color(rgb(LINE))
                        .cursor_pointer()
                        .hover(|d| d.bg(rgb(HOVER)))
                        .text_size(TEXT_SMALL)
                        .text_color(rgb(INK_SOFT))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.add_form_profile(None, window, cx);
                        }))
                        .child(Icon::new(IconAll::Plus).size_3())
                        .child("空白档案"),
                );
                // 预设一键添加
                let mut presets = div().h_flex().flex_wrap().gap_1p5();
                for (i, p) in preset_profiles().into_iter().enumerate() {
                    presets = presets.child(
                        div()
                            .id(("preset-add", i))
                            .h_flex()
                            .items_center()
                            .gap_1()
                            .px_2()
                            .py_0p5()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(LINE))
                            .cursor_pointer()
                            .hover(|d| d.bg(rgb(HOVER)))
                            .text_size(TEXT_SMALL)
                            .text_color(rgb(INK))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.add_form_profile(Some(i), window, cx);
                            }))
                            .child(Icon::new(IconAll::Plus).size_3().text_color(rgb(INK_SOFT)))
                            .child(p.name.clone()),
                    );
                }

                let mut builtin_body = div().v_flex().gap_4().child(
                    Self::field("配置档案", Some("可以存多套 provider 配置，输入框旁的菜单可快速切换"), profile_row),
                );
                if !f.profiles.is_empty() {
                    if let Some((p, inputs)) = f.current() {
                    // 协议二选一
                    let proto = |id: &'static str, label: &'static str, want: Protocol| {
                        let active = p.protocol == want;
                        div()
                            .id(id)
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(if active { CORAL } else { LINE }))
                            .bg(rgb(if active { SAND } else { CARD }))
                            .cursor_pointer()
                            .hover(|d| d.bg(rgb(HOVER)))
                            .text_size(TEXT_SMALL)
                            .text_color(rgb(INK))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_form_protocol(want, cx);
                            }))
                            .child(label)
                    };
                    let fetch_label = if self.fetching_models { "获取中…" } else { "获取模型列表" };
                    let mut model_row = div()
                        .h_flex()
                        .gap_2()
                        .child(div().flex_1().child(Self::text_input(&inputs.model)))
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
                    let current_model = value(&inputs.model, cx);
                    let mut chips = div().flex().flex_wrap().gap_1p5();
                    for (i, m) in p.models.iter().enumerate() {
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
                                .hover(|d| d.bg(rgb(HOVER)))
                                .text_size(TEXT_SMALL)
                                .font_family(MONO)
                                .text_color(rgb(INK))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    let pick = pick.clone();
                                    let i = this.settings_form.active;
                                    if let Some(inputs) = this.settings_form.inputs.get(i) {
                                        inputs.model.update(cx, |s, cx| s.set_value(pick, window, cx));
                                    }
                                    cx.notify();
                                }))
                                .child(m.clone()),
                        );
                    }
                    builtin_body = builtin_body
                        .child(
                            Self::field(
                                "常用服务预设",
                                Some("点击追加一套已填好端点的档案，再补上自己的 key"),
                                presets,
                            ),
                        )
                        .child(Self::field("名称", None, Self::text_input(&inputs.name)))
                        .child(Self::field(
                            "协议",
                            Some("绝大多数服务选 OpenAI 兼容；Anthropic 官方端点用原生协议"),
                            div()
                                .h_flex()
                                .gap_1p5()
                                .child(proto("protocol-openai", "OpenAI 兼容", Protocol::OpenAiCompat))
                                .child(proto("protocol-anthropic", "Anthropic 原生", Protocol::Anthropic)),
                        ))
                        .child(Self::field(
                            "端点地址",
                            Some("base URL，不含 /chat/completions 或 /messages 后缀"),
                            Self::text_input(&inputs.base_url),
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
                                .child(
                                    Input::new(&inputs.api_key)
                                        .appearance(false)
                                        .bordered(false)
                                        .mask_toggle(),
                                ),
                        ))
                        .child(Self::field(
                            "模型",
                            Some("新会话的默认模型；会话里也可以在输入框旁切换"),
                            model_row,
                        ))
                        .when(!p.models.is_empty(), |d| d.child(chips));
                    }
                }
                body = body.child(builtin_body);
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
                    .text_size(TEXT_SMALL)
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
                    .text_size(TEXT_META)
                    .text_color(rgb(INK_SOFT))
                    .child(format!(
                        "保存在 {path}（API Key 为明文）。启动时环境变量优先于这里的设置。只重启配置变了的内核，它上面进行中的对话会中断，下次发消息时自动续接。"
                    )),
            );

        div()
            .id("settings-scroll")
            // 焦点锚：设置页里点空白处也留在 ChatApp 键上下文里（Esc 关设置页）
            .track_focus(&self.settings_focus)
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .child(
                div()
                    .v_flex()
                    .max_w(FORM_W)
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
                                    .text_size(TEXT_TITLE)
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(rgb(INK))
                                    .child("设置"),
                            )
                            .child(
                                div()
                                    .id("settings-close")
                                    .p_1()
                                    .rounded_md()
                                    .cursor_pointer()
                                    .hover(|d| d.bg(rgb(HOVER)))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.settings_open = false;
                                        cx.notify();
                                    }))
                                    .child(Icon::new(IconAll::X).size_4().text_color(rgb(INK_SOFT))),
                            ),
                    )
                    .child(self.render_appearance(cx))
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

//! 设置页「项目」：这个仓库自己的配置，写在项目根的 `.claudesktop/settings.json`
//! （格式和读取规则见 `claudesktop_core::project`）。
//!
//! 这是一层**轻覆盖**：全局设置永远在，项目文件只写和全局不一样的几项（模型、权限规则、
//! 停用的 MCP 服务器、只用哪些插件、说明文件）。和设置页其余分类一样**即时生效**：开关一拨就写，
//! 输入框在离开（失焦）或按回车时写。文件解析不了时照常打开这一页，把原因列在顶部，
//! 但不写回（会把手写的内容整个盖掉）——改好文件点「重新读取」。
//!
//! 生效时机：读发生在自研内核**开会话**的时候，所以保存后新开的会话按它来，正在聊的会话不变；
//! 项目里的权限规则会在开会话时并进 `permissions.json`（集合语义，删掉项目规则不会删权限文件里的）。

use super::ChatApp;
use super::settings_kit::{form_field, note, section, seg_control};
use crate::theme::rgb;
use crate::theme::*;
use claudesktop_core::project::{self, ProjectConfig};
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::input::{InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::path::PathBuf;

/// 设置页「项目」分类的下标
pub(super) const PROJECT_TAB: usize = 1;

/// 插件那一栏：跟全局 / 只在这里列出的
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PluginScope {
    Global,
    Project,
}

/// 「项目」页的状态：项目根、表单里正在填的值、保存按钮要不要亮。
pub(super) struct ProjectForm {
    /// 项目根（配置文件就写在它下面）
    root: PathBuf,
    /// `.claudesktop/settings.json` 的完整路径
    path: PathBuf,
    model: Entity<InputState>,
    /// 权限规则，一行一条
    rules: Entity<TextareaState>,
    instructions: Entity<InputState>,
    /// 建 worktree 时从主目录复制过去的文件 / glob，一行一条
    worktree_copy: Entity<TextareaState>,
    /// 建 worktree 之后跑的初始化命令（如 `npm install`）
    worktree_setup: Entity<InputState>,
    /// 表单里这个项目停用的 MCP 服务器
    mcp_disabled: Vec<String>,
    /// 表单里这个项目启用的插件；None = 跟全局
    plugins: Option<Vec<String>>,
    /// 文件里的配置（和表单不一样才写；`broken` 时不写）
    saved: ProjectConfig,
    /// 读文件时的问题（文件坏了、格式不对）
    warnings: Vec<String>,
    /// 上次写文件的结果：(是不是错误, 文本)；接着改就清掉
    message: Option<(bool, gpui::SharedString)>,
    /// 全局 mcp.json 里的服务器名（这一页列开关用）
    servers: Vec<String>,
    /// 全局启用的插件名（项目只能从这里面挑）
    plugin_names: Vec<String>,
}

impl ProjectForm {
    pub(super) fn new(window: &mut Window, cx: &mut Context<ChatApp>) -> Self {
        let model = cx.new(|cx| InputState::new(window, cx).placeholder("留空 = 用全局的默认模型"));
        let rules = cx.new(|cx| TextareaState::new(window, cx).auto_grow(2, 8));
        let instructions =
            cx.new(|cx| InputState::new(window, cx).placeholder("默认读 AGENTS.md / CLAUDE.md"));
        let worktree_copy = cx.new(|cx| TextareaState::new(window, cx).auto_grow(2, 6));
        let worktree_setup = cx.new(|cx| InputState::new(window, cx).placeholder("如 npm install"));
        // 打字时清掉上次的结果；离开输入框（或单行框里按回车）时写文件
        for input in [&model, &instructions, &worktree_setup] {
            cx.subscribe(input, |this: &mut ChatApp, _, event: &InputEvent, cx| match event {
                InputEvent::Change => this.project_edited(cx),
                InputEvent::Blur | InputEvent::PressEnter { .. } => this.apply_project(cx),
                InputEvent::Focus => {}
            })
            .detach();
        }
        cx.subscribe(&rules, |this: &mut ChatApp, _, event: &InputEvent, cx| match event {
            InputEvent::Change => this.project_edited(cx),
            InputEvent::Blur => this.apply_project(cx),
            InputEvent::PressEnter { .. } | InputEvent::Focus => {}
        })
        .detach();
        cx.subscribe(&worktree_copy, |this: &mut ChatApp, _, event: &InputEvent, cx| match event {
            InputEvent::Change => this.project_edited(cx),
            InputEvent::Blur => this.apply_project(cx),
            InputEvent::PressEnter { .. } | InputEvent::Focus => {}
        })
        .detach();
        Self {
            root: PathBuf::new(),
            path: PathBuf::new(),
            model,
            rules,
            instructions,
            worktree_copy,
            worktree_setup,
            mcp_disabled: Vec::new(),
            plugins: None,
            saved: ProjectConfig::default(),
            warnings: Vec::new(),
            message: None,
            servers: Vec::new(),
            plugin_names: Vec::new(),
        }
    }

    /// 插件那一栏选的是哪种
    fn scope(&self) -> PluginScope {
        if self.plugins.is_some() { PluginScope::Project } else { PluginScope::Global }
    }
}

/// 比「有没有改动」用：顺序不影响语义，警告和「坏了」的标记本来也不进文件。
fn normalize(mut config: ProjectConfig) -> ProjectConfig {
    config.warnings.clear();
    config.broken = false;
    config.mcp_disabled.sort();
    if let Some(plugins) = &mut config.plugins_enabled {
        plugins.sort();
    }
    config.worktree = config.worktree.normalized();
    config
}

impl ChatApp {
    /// 重读当前会话的项目配置（打开设置页、切到「项目」时）。不负责重画。
    pub(super) fn reload_project_view(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let cwd = self.sessions[self.active].cwd.clone();
        let root = project::root_or(&cwd).to_path_buf();
        let path = project::path_in(&root);
        // 文件坏了也照常开这一页：`load` 给一份默认配置 + 一句能照着改的原因
        let config = project::load(&root).unwrap_or_default();
        let servers = claudesktop_builtin::default_mcp_config()
            .map(|p| claudesktop_builtin::mcp::load_config(&p).unwrap_or_default())
            .unwrap_or_default()
            .into_iter()
            .map(|s| s.name)
            .collect();
        // 项目只能从全局已启用的插件里挑（项目文件只做收窄）
        let plugin_names = self.enabled_plugins().into_iter().map(|p| p.name).collect();
        let model_text = config.model.clone().unwrap_or_default();
        let rules_text = config.permissions.join("\n");
        let instructions_text =
            config.instructions.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
        let copy_text = config.worktree.copy.join("\n");
        let setup_text = config.worktree.setup.clone().unwrap_or_default();
        let form = &mut self.project_form;
        form.root = root;
        form.path = path;
        form.model.update(cx, |s, cx| s.set_value(model_text, window, cx));
        form.rules.update(cx, |s, cx| s.set_value(rules_text, window, cx));
        form.instructions.update(cx, |s, cx| s.set_value(instructions_text, window, cx));
        form.worktree_copy.update(cx, |s, cx| s.set_value(copy_text, window, cx));
        form.worktree_setup.update(cx, |s, cx| s.set_value(setup_text, window, cx));
        form.mcp_disabled = config.mcp_disabled.clone();
        form.plugins = config.plugins_enabled.clone();
        form.warnings = config.warnings.clone();
        form.message = None;
        form.saved = config;
        form.servers = servers;
        form.plugin_names = plugin_names;
    }

    /// 表单当前的值 → 项目配置。
    fn project_config(&self, cx: &App) -> ProjectConfig {
        let form = &self.project_form;
        let model = form.model.read(cx).value().trim().to_string();
        let instructions = form.instructions.read(cx).value().trim().to_string();
        ProjectConfig {
            model: (!model.is_empty()).then_some(model),
            permissions: form
                .rules
                .read(cx)
                .value()
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect(),
            mcp_disabled: form.mcp_disabled.clone(),
            plugins_enabled: form.plugins.clone(),
            instructions: (!instructions.is_empty()).then(|| PathBuf::from(instructions)),
            worktree: claudesktop_core::project::WorktreeConfig {
                copy: form
                    .worktree_copy
                    .read(cx)
                    .value()
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .map(str::to_string)
                    .collect(),
                setup: Some(form.worktree_setup.read(cx).value().trim().to_string())
                    .filter(|s| !s.is_empty()),
            },
            warnings: Vec::new(),
            broken: false,
        }
    }

    /// 表单里又改了：上次写文件的结果不再代表现在的样子，清掉
    fn project_edited(&mut self, cx: &mut Context<Self>) {
        if self.project_form.message.take().is_some() {
            cx.notify();
        }
    }

    /// 表单和文件不一样就写回 `.claudesktop/settings.json`（一个字段都没填就把文件删掉，
    /// 不留一个空对象在仓库里）。文件解析不了时不写，提示先改好文件。
    pub(super) fn apply_project(&mut self, cx: &mut Context<Self>) {
        let config = self.project_config(cx);
        let form = &mut self.project_form;
        if form.root.as_os_str().is_empty()
            || normalize(config.clone()) == normalize(form.saved.clone())
        {
            return;
        }
        if form.saved.broken {
            form.message = Some((
                true,
                "配置文件格式有问题，这里的改动没有写进去：先改好文件，再点「重新读取」".into(),
            ));
            cx.notify();
            return;
        }
        match project::save(&form.root, &config) {
            Ok(path) => {
                form.saved = config;
                form.message = Some((
                    false,
                    format!("已写入 {}；新开的会话按它来，正在聊的会话不受影响", path.display())
                        .into(),
                ));
            }
            Err(err) => form.message = Some((true, err.into())),
        }
        cx.notify();
    }

    /// 项目配置文件在资源管理器里露出来（还没建过就打开项目根的 `.claudesktop`）。
    fn reveal_project_file(&self, cx: &mut Context<Self>) {
        let path = &self.project_form.path;
        let target = if path.exists() {
            path.clone()
        } else {
            path.parent().map(PathBuf::from).unwrap_or_else(|| path.clone())
        };
        cx.reveal_path(&target);
    }

    fn toggle_project_mcp(&mut self, name: String, on: bool, cx: &mut Context<Self>) {
        let list = &mut self.project_form.mcp_disabled;
        list.retain(|n| *n != name);
        if !on {
            list.push(name);
        }
        self.apply_project(cx);
        cx.notify();
    }

    fn toggle_project_plugin(&mut self, name: String, on: bool, cx: &mut Context<Self>) {
        let Some(list) = &mut self.project_form.plugins else {
            return;
        };
        list.retain(|n| *n != name);
        if on {
            list.push(name);
        }
        self.apply_project(cx);
        cx.notify();
    }

    pub(super) fn render_project(&self, cx: &mut Context<Self>) -> Div {
        let form = &self.project_form;
        let actions = div()
            .h_flex()
            .gap_1p5()
            .child(Self::text_button("project-reload", "重新读取", false).small().on_click(
                cx.listener(|this, _, window, cx| {
                    this.reload_project_view(window, cx);
                    cx.notify();
                }),
            ))
            .child(
                Self::text_button("project-reveal", "打开文件位置", false)
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| this.reveal_project_file(cx))),
            )
            .into_any_element();

        // 项目根：这一页的配置管哪个目录
        let root_line = div()
            .v_flex()
            .gap_1()
            .child(note(IconAll::FolderTree, format!("项目根：{}", form.root.display())))
            .child(note(IconAll::Braces, format!("配置文件：{}", form.path.display())));

        // 模型：留空就跟全局
        let global_model = self.settings.builtin.current().model.clone();
        let model_hint = if global_model.is_empty() {
            "新会话默认用的模型；留空 = 跟全局".to_string()
        } else {
            format!("新会话默认用的模型；留空 = 跟全局（现在是 {global_model}）")
        };
        let model = div()
            .v_flex()
            .gap_1p5()
            .child(Self::text_input(&form.model))
            .child(div().text_size(TEXT_SMALL).text_color(rgb(INK_SOFT)).child(model_hint));

        let rules = div()
            .v_flex()
            .gap_1p5()
            .child(
                div()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(LINE))
                    .bg(rgb(CARD))
                    .px_1()
                    .font_family(MONO)
                    .text_size(TEXT_SMALL)
                    .child(Textarea::new(&form.rules).appearance(false).bordered(false)),
            )
            .child(note(
                IconAll::Dot,
                "一行一条，如 edit、shell:git status、fetch:example.com；前加 deny: 为拒绝",
            ))
            .child(note(IconAll::Dot, "新会话生效；删这里不影响已记下的规则"));

        // MCP：只列全局 mcp.json 里的服务器 + 项目里已经停用的（配置里写了但 mcp.json 没有的）
        let mut server_names = form.servers.clone();
        for name in &form.mcp_disabled {
            if !server_names.contains(name) {
                server_names.push(name.clone());
            }
        }
        let mcp = if server_names.is_empty() {
            note(IconAll::Info, "全局还没有 MCP 服务器；在「MCP」页添加之后，这里可以按项目停用")
        } else {
            let mut panel = div().v_flex().gap_0p5().p_1().rounded_xl().bg(rgb(SAND));
            for (i, name) in server_names.into_iter().enumerate() {
                let on = !form.mcp_disabled.contains(&name);
                let target = name.clone();
                let missing = !form.servers.contains(&name);
                panel = panel.child(
                    div()
                        .h_flex()
                        .items_center()
                        .gap_3()
                        .px_3()
                        .py_2()
                        .child(
                            div()
                                .v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(
                                    div()
                                        .text_size(TEXT_BODY)
                                        .text_color(rgb(INK))
                                        .child(name.clone()),
                                )
                                .when(missing, |d| {
                                    d.child(
                                        div()
                                            .text_size(TEXT_SMALL)
                                            .text_color(rgb(INK_SOFT))
                                            .child("全局的 mcp.json 里已经没有这个服务器了"),
                                    )
                                }),
                        )
                        .child(
                            Switch::new(("project-mcp", i))
                                .checked(on)
                                .color(rgb(CORAL))
                                .on_change(cx.listener(move |this, checked: &bool, _, cx| {
                                    this.toggle_project_mcp(target.clone(), *checked, cx)
                                })),
                        ),
                );
            }
            panel
        };

        // 插件：跟全局，或只在这里列出全局已启用的那几个
        let scope = form.scope();
        let mut plugins = div().v_flex().gap_3().child(seg_control(
            "project-plugins",
            [(PluginScope::Global, "跟全局"), (PluginScope::Project, "只在这里选")],
            scope,
            |this, scope, _, cx| {
                let form = &mut this.project_form;
                match scope {
                    // 打开时先全选上：什么都没变，取消勾选才生效
                    PluginScope::Project => {
                        form.plugins = Some(form.plugin_names.clone());
                    }
                    PluginScope::Global => form.plugins = None,
                }
                this.apply_project(cx);
                cx.notify();
            },
            cx,
        ));
        if form.plugin_names.is_empty() {
            plugins = plugins.child(note(
                IconAll::Info,
                "全局还没有启用任何插件；在「插件」页启用之后，这里可以按项目挑",
            ));
        } else if scope == PluginScope::Project {
            let plugins_form = form.plugins.clone().unwrap_or_default();
            let mut panel = div().v_flex().gap_0p5().p_1().rounded_xl().bg(rgb(SAND));
            for (i, name) in form.plugin_names.iter().enumerate() {
                let on = plugins_form.contains(name);
                let target = name.clone();
                panel = panel.child(
                    div()
                        .h_flex()
                        .items_center()
                        .gap_3()
                        .px_3()
                        .py_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(TEXT_BODY)
                                .text_color(rgb(INK))
                                .child(name.clone()),
                        )
                        .child(
                            Switch::new(("project-plugin", i))
                                .checked(on)
                                .color(rgb(CORAL))
                                .on_change(cx.listener(move |this, checked: &bool, _, cx| {
                                    this.toggle_project_plugin(target.clone(), *checked, cx)
                                })),
                        ),
                );
            }
            plugins = plugins.child(panel);
        }

        // 说明文件：相对项目根
        let instructions = div()
            .v_flex()
            .gap_1p5()
            .child(Self::text_input(&form.instructions))
            .child(note(IconAll::Dot, "相对项目根，在 AGENTS.md / CLAUDE.md 之后追加"));

        // worktree：建 worktree 之后复制哪些文件、跑什么命令（都没配时什么都不做）
        let worktree = div()
            .v_flex()
            .gap_1p5()
            .child(
                div()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(LINE))
                    .bg(rgb(CARD))
                    .px_1()
                    .font_family(MONO)
                    .text_size(TEXT_SMALL)
                    .child(Textarea::new(&form.worktree_copy).appearance(false).bordered(false)),
            )
            .child(Self::text_input(&form.worktree_setup))
            .child(note(
                IconAll::Dot,
                "上面一行一条：建 worktree 时从主目录复制过去的文件（如 .env*），相对项目根",
            ))
            .child(note(IconAll::Dot, "下面这一行是建好之后在里面跑的初始化命令（如 npm install）"));

        // 底部：上次写文件的结果（即时生效，没有保存按钮）
        let status: AnyElement = match &form.message {
            Some((error, text)) => div()
                .text_color(rgb(if *error { RED } else { GREEN }))
                .child(text.clone())
                .into_any_element(),
            None => div().text_color(rgb(INK_SOFT)).child("改完即生效").into_any_element(),
        };
        let footer = div().text_size(TEXT_SMALL).line_height(LEADING).child(status);

        let warnings = form.warnings.clone();
        section(
            "这个项目",
            "只写和全局不一样的，其余跟全局",
            Some(actions),
            div()
                .v_flex()
                .gap_4()
                .child(root_line)
                .children(warnings.into_iter().map(|w| {
                    div().text_size(TEXT_SMALL).line_height(LEADING).text_color(rgb(RED)).child(w)
                }))
                .child(
                    div()
                        .v_flex()
                        .gap_5()
                        .child(form_field("模型", None, model))
                        .child(form_field("权限规则", None, rules))
                        .child(form_field("停用的 MCP", Some("只在这个项目里不连".into()), mcp))
                        .child(form_field("插件", Some("从全局已启用的插件里挑".into()), plugins))
                        .child(form_field("说明文件", None, instructions))
                        .child(form_field(
                            "worktree",
                            Some("建 worktree 时复制哪些文件、跑什么命令".into()),
                            worktree,
                        )),
                )
                .child(footer),
        )
    }
}

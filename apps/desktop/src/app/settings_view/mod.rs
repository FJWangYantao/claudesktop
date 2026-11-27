//! 设置页（内核 / provider 档案 / 模型）与输入框旁的模型下拉。

use super::ChatApp;
use super::menu::{
    FONT_PREVIEW, MenuChip, MenuList, code_theme_menu_item, font_menu_item, menu_item,
    menu_popover, menu_text_item, theme_menu_item,
};
use super::motion::{self, Reveal};
use super::settings_kit::*;
use super::widgets::{MeasuredWidth, Surface};
use crate::model::{FailureKind, ViewMode};
use crate::settings::*;
use crate::theme::rgb;
use crate::theme::*;
use claudesktop_builtin::{
    PermissionRules, Protocol, ProviderConfig, ThinkingLevel, ThinkingSpec, default_rules_path,
};
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::base::PopoverState;
use gpui_kit::component::button::{ButtonVariant, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::{Scrollbar, ScrollbarMode};
use gpui_kit::component::slider::Slider;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::theme::{Theme, ThemeMode};
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::collections::HashSet;

/// 设置页里 magpie 网关的状态（探测 / 下载 / 启动各是一个后台任务，见「设计说明-本机agent.md」第十四节）。
#[derive(Default)]
pub(super) struct MagpieForm {
    /// 探过了没（后台探活回来置 true；`false` 时状态行显示「正在检测…」）
    probed: bool,
    probing: bool,
    /// 网关在跑时的版本号（谁起的都一样：用户自己开的 magpie 也直接复用）
    running: Option<String>,
    /// 正在下载（已收字节 / 总字节）
    downloading: Option<(u64, u64)>,
    /// 正在启动（`serve` 起来后探活确认，最多等十几秒）
    starting: bool,
    /// 下载 / 启动失败的一句话（能照着做的）
    error: Option<String>,
}

/// 本机 agent 检测的状态：首次进「内核」时检测，之后缓存到手动刷新。
#[derive(Default)]
enum AgentDetect {
    /// 还没测
    #[default]
    Idle,
    /// 在测
    Running,
    /// 测完了（没装的不在列表里，界面按预设表补一行）
    Done(Vec<claudesktop_core::agents::Detected>),
}

/// 设置页左侧的分类（下标即 `SettingsForm::tab`）。
const SETTINGS_TABS: [(IconAll, &str); 10] = [
    (IconAll::Settings, "通用"),
    (IconAll::FolderTree, "项目"),
    (IconAll::Cpu, "内核"),
    (IconAll::Server, "服务商"),
    (IconAll::Bot, "模型"),
    (IconAll::ShieldCheck, "权限"),
    (IconAll::Plug, "MCP"),
    (IconAll::Blocks, "插件"),
    // 用量：单独一页，不在导航里列（见 `render_settings`）
    (IconAll::ChartColumn, "用量"),
    (IconAll::Brain, "记忆"),
];

/// 设置页「内核」分类的下标（首页配置引导、失败卡片的「打开设置」直接打开它）
pub(super) const KERNEL_TAB: usize = 2;

/// 设置页「服务商」分类的下标（来源清单 + 手填端点）
pub(super) const PROVIDER_TAB: usize = 3;

/// 设置页「模型」分类的下标（默认模型、思考强度、模型清单）
pub(super) const MODEL_TAB: usize = 4;

/// 设置页「MCP」分类的下标（右侧概览区 MCP 卡片直接打开它）
pub(crate) const MCP_TAB: usize = 6;

/// 设置页「插件」分类的下标（侧栏的「插件」入口直接打开它）
pub(crate) const PLUGINS_TAB: usize = 7;

mod debug;
mod form;
mod general;
mod kernel;
mod lifecycle;
mod model;
mod provider;
mod rules;
mod web_search;

/// 一套档案在表单里的输入框（与 `SettingsForm::profiles` 一一对应）。
pub struct ProfileInputs {
    name: Entity<InputState>,
    base_url: Entity<InputState>,
    api_key: Entity<InputState>,
}

impl ProfileInputs {
    fn new(window: &mut Window, cx: &mut Context<ChatApp>, p: &ProviderProfile) -> Self {
        let make = |value: String,
                    placeholder: &'static str,
                    masked: bool,
                    window: &mut Window,
                    cx: &mut Context<ChatApp>| {
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(placeholder)
                    .masked(masked)
                    .default_value(value)
            });
            cx.subscribe(&input, |this: &mut ChatApp, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = input.read(cx).value().to_string();
                    for (profile, fields) in
                        this.settings_form.profiles.iter_mut().zip(&this.settings_form.inputs)
                    {
                        if input == fields.name {
                            profile.name = text.clone();
                        } else if input == fields.base_url {
                            profile.base_url = text.clone();
                        } else if input == fields.api_key {
                            profile.api_key = text.clone();
                        }
                    }
                    this.settings_form.message = None;
                    cx.notify();
                }
            })
            .detach();
            input
        };
        Self {
            name: make(p.name.clone(), "如 DeepSeek（可留空）", false, window, cx),
            base_url: make(p.base_url.clone(), "如 https://api.deepseek.com/v1", false, window, cx),
            api_key: make(p.api_key.clone(), "没有可留空", true, window, cx),
        }
    }
}

/// 设置页的编辑中状态（保存前不影响正在用的设置）。
pub struct SettingsForm {
    kernel: KernelChoice,
    /// 编辑中的档案列表（保存才生效）
    profiles: Vec<ProviderProfile>,
    /// 全局默认模型所属的来源，随模型选择一起更新
    active: usize,
    /// 「服务商」页正在查看 / 编辑哪套（不跟默认模型所属来源走）
    view: usize,
    editing_profile: bool,
    /// 每套档案的输入框
    inputs: Vec<ProfileInputs>,
    /// 设置页当前所在的 tab（0 通用 / 1 项目 / 2 内核 / 3 服务商 / 4 模型 / 5 权限 / 6 MCP / 7 插件 / 8 用量页 / 9 记忆）
    pub(super) tab: usize,
    /// 编辑中的 agent 档案表（保存才生效；预设的启用 / 网关在这里改）
    agents: Vec<AgentProfile>,
    /// 本机 agent 的检测结果（同一次应用启动中保留，手动刷新才重测）
    agent_detect: AgentDetect,
    /// 首页选了本机 agent、检测还没跑完时，跑完启用选中的那一家
    enable_on_detect: bool,
    /// 自定义 agent 的启动命令（空 = 没有自定义 agent）
    custom_command: Entity<InputState>,
    /// 一轮最多几步（自研内核的工具循环上限）
    max_steps: Entity<InputState>,
    /// 用量网关开关（保存才生效，会重启自研内核）
    usage_gateway: bool,
    gateway_bodies: bool,
    /// 点了「清空日志」，等再点一次确认
    confirm_clear_gateway: bool,
    /// 模型单价表（一行一个模型，格式见 `pricing.rs`）
    prices: Entity<TextareaState>,
    /// 「自动」模式：分类器模型（跟着会话走时不用）
    auto_model: Entity<InputState>,
    /// 「自动」模式：信任边界（自然语言）
    auto_environment: Entity<TextareaState>,
    /// 「自动」模式开着没有（保存才生效）
    auto_enabled: bool,
    /// 分类器跟随当前模型
    auto_follow: bool,
    /// 输入补全开着没有（保存才生效）
    suggest_enabled: bool,
    /// 一轮结束后猜下一句开着没有（保存才生效，和上面那个各管各的）
    next_suggestion: bool,
    /// 输入补全用的模型；空 = 跟会话的
    suggest_model: Entity<InputState>,
    subagent_model: Entity<InputState>,
    /// 一轮结束后提炼候选记忆开着没有（保存才生效，会重启自研内核）
    memory_suggest: bool,
    /// 聊天用的模型；空 = 跟档案的默认模型
    chat_model: Entity<InputState>,
    /// 新建的聊天默认开着「联网」
    chat_web: bool,
    /// 「聊天说明」文件（每轮重读，改了下一轮就生效）
    chat_instructions: Entity<InputState>,
    /// Brave Search API key（`web_search` 用）
    brave_key: Entity<InputState>,
    /// `web_search` 的后端选择（存在内核那边的数据目录 `search.json`，所以不在 `Settings` 里）
    search: claudesktop_builtin::websearch::SearchConfig,
    /// DeepSeek 搜索单独填的 key（空 = 用「服务商」页里 DeepSeek 那份的 key）
    deepseek_search_key: Entity<InputState>,
    /// (是否错误, 文本)
    message: Option<(bool, SharedString)>,
    /// 已保存的「总是允许」规则：(项目目录, 规则)；删除即时生效，不等「保存」
    rules: Vec<(String, Vec<String>)>,
    /// 展开着的说明 / 示例（`ChatApp::disclosure` 的 key），关设置页也记着
    pub(super) open_help: HashSet<&'static str>,
    /// 内容列的定宽（见 `MeasuredWidth`）
    pub(super) content_w: MeasuredWidth,
    /// 上次「获取列表」的结果，显示在模型那一栏下面：(失败了吗, 给人看的话, 服务商的原文)
    fetch_result: Option<(bool, SharedString, Option<SharedString>)>,
    model_search: Entity<InputState>,
    model_menu_search: Entity<InputState>,
    model_menu_query: String,
    model_source: Option<usize>,
    hovered_action: Option<SharedString>,
    /// 模型清单自己的滚动位置；外层滚轮不会抢走它
    pub(super) model_scroll: ScrollHandle,
    /// magpie 网关的探测 / 下载 / 启动状态
    magpie: MagpieForm,
    /// 打开设置表单时对照的已保存设置：未保存时保留草稿；若外部改过设置则重新载入
    base_settings: Option<Settings>,
}

impl SettingsForm {
    pub fn new(window: &mut Window, cx: &mut Context<ChatApp>) -> Self {
        let custom_command = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("如 npx -y @agentclientprotocol/claude-agent-acp")
        });
        let max_steps = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(claudesktop_builtin::DEFAULT_MAX_STEPS.to_string())
        });
        let prices = cx.new(|cx| TextareaState::new(window, cx).auto_grow(3, 12));
        let auto_model =
            cx.new(|cx| InputState::new(window, cx).placeholder("如 deepseek-chat / glm-4-flash"));
        let auto_environment = cx.new(|cx| TextareaState::new(window, cx).auto_grow(2, 10));
        let suggest_model = cx.new(|cx| {
            InputState::new(window, cx).placeholder("留空用会话的模型，如 deepseek-chat")
        });
        let subagent_model = cx.new(|cx| {
            InputState::new(window, cx).placeholder("留空用会话的模型，如 deepseek-chat")
        });
        let chat_model = cx.new(|cx| {
            InputState::new(window, cx).placeholder("留空用上面默认模型，如 deepseek-chat")
        });
        let chat_instructions =
            cx.new(|cx| InputState::new(window, cx).placeholder("说明文件的路径"));
        let brave_key =
            cx.new(|cx| InputState::new(window, cx).placeholder("没有也能搜（DuckDuckGo 兜底）"));
        let deepseek_search_key = cx.new(|cx| {
            InputState::new(window, cx).placeholder("留空就用「服务商」页里 DeepSeek 那份的 key")
        });
        let model_search = cx.new(|cx| InputState::new(window, cx).placeholder("搜索或输入模型名"));
        let model_menu_search = cx.new(|cx| InputState::new(window, cx).placeholder("搜索模型"));
        for search in [&model_search, &model_menu_search] {
            cx.subscribe(search, |this: &mut ChatApp, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    if input == this.settings_form.model_menu_search {
                        this.settings_form.model_menu_query = input.read(cx).value().to_string();
                    }
                    this.model_wheel = None;
                    this.settings_form.model_scroll.set_offset(point(px(0.), px(0.)));
                    cx.notify();
                }
            })
            .detach();
        }
        Self {
            kernel: KernelChoice::Builtin,
            profiles: Vec::new(),
            active: 0,
            view: 0,
            editing_profile: false,
            inputs: Vec::new(),
            tab: 0,
            agents: Vec::new(),
            agent_detect: AgentDetect::Idle,
            enable_on_detect: false,
            custom_command,
            max_steps,
            usage_gateway: true,
            gateway_bodies: true,
            confirm_clear_gateway: false,
            prices,
            auto_model,
            auto_environment,
            auto_enabled: false,
            auto_follow: true,
            suggest_enabled: false,
            next_suggestion: false,
            suggest_model,
            subagent_model,
            memory_suggest: false,
            chat_model,
            chat_web: true,
            chat_instructions,
            brave_key,
            search: Default::default(),
            deepseek_search_key,
            message: None,
            rules: Vec::new(),
            open_help: HashSet::new(),
            content_w: MeasuredWidth::default(),
            fetch_result: None,
            model_search,
            model_menu_search,
            model_menu_query: String::new(),
            model_source: None,
            hovered_action: None,
            model_scroll: ScrollHandle::new(),
            magpie: MagpieForm::default(),
            base_settings: None,
        }
    }

    /// 正在查看 / 编辑的档案（没有时给空档案占位）。
    fn current(&self) -> Option<(&ProviderProfile, &ProfileInputs)> {
        let p = self.profiles.get(self.view)?;
        let i = self.inputs.get(self.view)?;
        Some((p, i))
    }
}

fn value(input: &Entity<InputState>, cx: &App) -> String {
    input.read(cx).value().trim().to_string()
}

/// 设置页的「一轮最多几步」输入 → 实际值：空着或写了非数字就用默认值，
/// 超出范围夹回允许区间（和读设置文件时的夹法一致）。
fn parse_max_steps(text: &str) -> usize {
    let text = text.trim();
    if text.is_empty() {
        return claudesktop_builtin::DEFAULT_MAX_STEPS;
    }
    text.parse::<usize>()
        .unwrap_or(claudesktop_builtin::DEFAULT_MAX_STEPS)
        .clamp(claudesktop_builtin::MIN_MAX_STEPS, claudesktop_builtin::MAX_MAX_STEPS)
}

// 设置页的「思考强度」通过下拉档位直接写进默认模型所属档案，不用输入框解析；
// 老的「思考预算」在 `Settings::normalize` 里折算成档位了（见 `render_thinking_row`）。

impl ChatApp {
    pub(super) fn settings_navigation_only(&self) -> bool {
        self.settings_open && self.settings_form.tab != super::usage_view::USAGE_TAB
    }

    // ---------- 档案编辑（表单内） ----------

    /// 用量网关：开关（随「保存」生效），说明下面带「打开日志目录」「清空日志」（再点一次确认）。
    fn gateway_row(&self, cx: &mut Context<Self>) -> Div {
        let f = &self.settings_form;
        let confirm = f.confirm_clear_gateway;
        let actions = div()
            .h_flex()
            .items_center()
            .gap_1()
            .ml(rems(-0.5))
            .child(
                Self::quiet_button("gateway-open", IconAll::FolderOpen, Surface::Paper, cx)
                    .xsmall()
                    .label("打开日志目录")
                    .px_2()
                    .on_click(cx.listener(|this, _, _, cx| this.open_gateway_dir(cx))),
            )
            .child(if confirm {
                Self::text_button("gateway-clear", "确定清空？", false)
                    .xsmall()
                    .with_variant(ButtonVariant::Danger)
                    .on_click(cx.listener(|this, _, _, cx| this.clear_gateway_logs(cx)))
            } else {
                Self::quiet_button("gateway-clear", IconAll::Trash, Surface::Paper, cx)
                    .xsmall()
                    .label("清空日志")
                    .px_2()
                    .on_click(cx.listener(|this, _, _, cx| this.clear_gateway_logs(cx)))
            });
        setting_row_with(
            "用量网关",
            actions,
            Switch::new("usage-gateway")
                .small()
                .checked(f.usage_gateway)
                .color(rgb(CORAL))
                .on_change(cx.listener(|this, checked: &bool, _, cx| {
                    this.settings_form.usage_gateway = *checked;
                    cx.notify();
                })),
        )
    }

    /// 网关存不存请求原文（随「保存」生效；网关关着时不可点）。
    fn bodies_row(&self, cx: &mut Context<Self>) -> Div {
        let f = &self.settings_form;
        setting_row(
            "保存请求原文",
            Switch::new("gateway-bodies")
                .small()
                .checked(f.gateway_bodies && f.usage_gateway)
                .disabled(!f.usage_gateway)
                .color(rgb(CORAL))
                .on_change(cx.listener(|this, checked: &bool, _, cx| {
                    this.settings_form.gateway_bodies = *checked;
                    cx.notify();
                })),
        )
    }

    /// 模型单价：一行说明 +「编辑」；展开后是多行文本框、写法说明和「恢复默认」。
    fn prices_row(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        const KEY: &str = "prices";
        let open = self.settings_form.open_help.contains(KEY);
        let row = setting_row(
            "模型单价",
            Self::text_button("prices-edit", if open { "收起" } else { "编辑" }, false)
                .small()
                .on_click(cx.listener(|this, _, _, cx| {
                    let help = &mut this.settings_form.open_help;
                    if !help.remove(KEY) {
                        help.insert(KEY);
                    }
                    cx.notify();
                })),
        );
        let progress = motion::reveal_progress("prices-editor", open, window, cx);
        if progress <= 0. {
            return row;
        }
        let editor = div()
            .rounded_lg()
            .border_1()
            .border_color(rgb(LINE))
            .bg(rgb(CARD))
            .px_1()
            .font_family(MONO)
            .text_size(TEXT_SMALL)
            .child(Textarea::new(&self.settings_form.prices).appearance(false).bordered(false));
        let format = div()
            .v_flex()
            .gap_2()
            .child(example_block(PRICES_EXAMPLE))
            .child(note(
                IconAll::Dot,
                "一行一个模型：名称、输入价、输出价、缓存命中价，按每百万 token 计，数字前写 $ 或 ¥",
            ))
            .child(note(IconAll::Dot, "名称按包含匹配，几条都对得上时取最长的那条；# 开头是注释"))
            .child(note(
                IconAll::Dot,
                "分峰谷价的模型行尾写 x2（前面填谷价），再加一行 @peak 写峰时段：星期（1-5 = 周一到周五）、时段、时区",
            ));
        let editor = div().v_flex().gap_2().pb_4().child(editor).child(
            div()
                .h_flex()
                .items_start()
                .gap_2()
                .child(div().flex_1().min_w_0().child(self.disclosure(
                    "prices-format",
                    "写法说明",
                    format,
                    window,
                    cx,
                )))
                .child(Self::text_button("prices-reset", "恢复默认", false).small().on_click(
                    cx.listener(|this, _, window, cx| {
                        let text = crate::pricing::to_text(&crate::pricing::Table::defaults());
                        this.settings_form
                            .prices
                            .update(cx, |state, cx| state.set_value(text, window, cx));
                        cx.notify();
                    }),
                )),
        );
        div().v_flex().child(row).child(Reveal::new("prices-editor-body", progress, editor))
    }

    pub(super) fn open_gateway_dir(&mut self, cx: &mut Context<Self>) {
        let Some(dir) = claudesktop_builtin::gateway::default_dir() else {
            return;
        };
        let _ = std::fs::create_dir_all(&dir);
        cx.open_with_system(&dir);
    }

    fn clear_gateway_logs(&mut self, cx: &mut Context<Self>) {
        if !self.settings_form.confirm_clear_gateway {
            self.settings_form.confirm_clear_gateway = true;
            cx.notify();
            return;
        }
        self.settings_form.confirm_clear_gateway = false;
        let Some(dir) = claudesktop_builtin::gateway::default_dir() else {
            return;
        };
        let text = match claudesktop_builtin::gateway::clear_logs(&dir) {
            Ok(n) => (false, format!("已清空用量网关日志（{n} 个文件）")),
            Err(err) => (true, format!("清空失败：{err}")),
        };
        self.settings_form.message = Some((text.0, text.1.into()));
        // 首页「请求」页签下次重读
        self.requests_view = Default::default();
        cx.notify();
    }

    pub(super) fn text_input(state: &Entity<InputState>) -> Div {
        Self::text_input_with(Input::new(state).appearance(false).bordered(false))
    }

    /// 同 [`Self::text_input`]，带显示 / 隐藏切换（key 一类）。
    pub(super) fn secret_input(state: &Entity<InputState>) -> Div {
        Self::text_input_with(Input::new(state).appearance(false).bordered(false).mask_toggle())
    }

    fn text_input_with(input: Input) -> Div {
        div().rounded_lg().border_1().border_color(rgb(LINE)).bg(rgb(CARD)).px_1().child(input)
    }

    /// 内核 / 服务商 / 模型三个需要「保存」的页签各自有没有未保存草稿。
    fn dirty_tabs(&self, cx: &App) -> (bool, bool, bool) {
        let prices_err =
            crate::pricing::parse_text(&self.settings_form.prices.read(cx).value()).is_err();
        let normalize = |mut s: Settings| {
            s.kernel = Some(s.kernel_for(s.kernel.as_ref().unwrap_or(&KernelChoice::Builtin)));
            // 打开设置页时补上的那套空档案不算改动
            let blank = ProviderProfile::default();
            s.builtin.profiles.retain(|p| *p != blank);
            s.builtin.active = s.builtin.active.min(s.builtin.profiles.len().saturating_sub(1));
            s
        };
        let draft = normalize(self.form_settings(cx));
        let saved = normalize(self.settings.clone());
        let mut kernel = prices_err
            || draft.kernel != saved.kernel
            || draft.acp != saved.acp
            || draft.max_steps != saved.max_steps
            || draft.auto_mode != saved.auto_mode
            || draft.suggestions != saved.suggestions
            || draft.next_suggestion != saved.next_suggestion
            || draft.memory_suggest != saved.memory_suggest
            || draft.suggest_model != saved.suggest_model
            || draft.subagent_model != saved.subagent_model
            || draft.chat != saved.chat
            || draft.usage_gateway != saved.usage_gateway
            || draft.gateway_bodies != saved.gateway_bodies
            || draft.model_prices != saved.model_prices
            || draft.price_peaks != saved.price_peaks;
        let same_provider_fields = |a: &ProviderProfile, b: &ProviderProfile| {
            a.name == b.name
                && a.protocol == b.protocol
                && a.base_url == b.base_url
                && a.api_key == b.api_key
                && a.params.temperature == b.params.temperature
                && a.params.max_tokens == b.params.max_tokens
                && a.params.extra == b.params.extra
                && a.params.server_tools == b.params.server_tools
                && a.magpie == b.magpie
        };
        let same_model_fields = |a: &ProviderProfile, b: &ProviderProfile| {
            a.model == b.model
                && a.params.thinking == b.params.thinking
                && a.menu_hidden == b.menu_hidden
                && a.models == b.models
                && a.context_windows == b.context_windows
        };
        let provider = draft.builtin.profiles.len() != saved.builtin.profiles.len()
            || draft
                .builtin
                .profiles
                .iter()
                .zip(&saved.builtin.profiles)
                .any(|(a, b)| !same_provider_fields(a, b));
        let model = draft.builtin.active != saved.builtin.active
            || (draft.builtin.profiles.len() == saved.builtin.profiles.len()
                && draft
                    .builtin
                    .profiles
                    .iter()
                    .zip(&saved.builtin.profiles)
                    .any(|(a, b)| !same_model_fields(a, b)));
        if !kernel && !provider && !model && draft != saved {
            kernel = true;
        }
        (kernel, provider, model)
    }

    /// 表单和生效中的设置有没有不同（页头据此提示、决定「保存」能不能点）。
    fn form_dirty(&self, cx: &App) -> bool {
        let (kernel, provider, model) = self.dirty_tabs(cx);
        kernel || provider || model
    }

    fn hover_settings_action(&mut self, id: &SharedString, hovered: bool, cx: &mut Context<Self>) {
        if hovered {
            if self.settings_form.hovered_action.as_ref() != Some(id) {
                self.settings_form.hovered_action = Some(id.clone());
                cx.notify();
            }
        } else if self.settings_form.hovered_action.as_ref() == Some(id) {
            self.settings_form.hovered_action = None;
            cx.notify();
        }
    }

    fn select_settings_tab(&mut self, tab: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings_form.tab == tab {
            return;
        }
        if self.settings_form.tab == super::project_view::PROJECT_TAB {
            self.apply_project(cx);
        }
        self.settings_form.tab = tab;
        self.settings_wheel = None;
        self.model_wheel = None;
        self.settings_scroll.set_offset(point(px(0.), px(0.)));
        self.settings_form.model_scroll.set_offset(point(px(0.), px(0.)));
        self.settings_form.message = None;
        self.settings_form.hovered_action = None;
        if tab == KERNEL_TAB && matches!(self.settings_form.agent_detect, AgentDetect::Idle) {
            self.detect_agents(cx);
        }
        if tab == super::memory_view::MEMORY_TAB {
            self.reload_memory_view();
        }
        if tab == super::project_view::PROJECT_TAB {
            self.reload_project_view(window, cx);
        }
        cx.notify();
    }

    pub(super) fn render_settings(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let f = &self.settings_form;
        // 用量是单独一页（侧栏「用量」打开），不带分类导航
        if f.tab == super::usage_view::USAGE_TAB {
            return self.render_usage_page(cx);
        }
        let compact_navigation =
            window.viewport_size().width < rems(37.).to_pixels(window.rem_size());
        let dirty_tabs = self.dirty_tabs(cx);
        let dirty = dirty_tabs.0 || dirty_tabs.1 || dirty_tabs.2;
        // 左侧分类导航
        let mut nav = div()
            .id("settings-navigation")
            .v_flex()
            .w(SETTINGS_NAV_W)
            .h_full()
            .flex_none()
            .px_2()
            .py_3()
            .gap_0p5()
            .bg(rgb(SAND))
            .overflow_y_scroll()
            .child(
                Self::quiet_button("settings-back", IconAll::ArrowLeft, Surface::Sand, cx)
                    .small()
                    .label("返回对话")
                    .mb_4()
                    .on_click(cx.listener(|this, _, _, cx| this.close_settings(cx))),
            );
        for (i, (icon, label)) in SETTINGS_TABS.into_iter().enumerate() {
            if i == super::usage_view::USAGE_TAB {
                continue;
            }
            let active = i == f.tab;
            let tab_dirty = match i {
                KERNEL_TAB => dirty_tabs.0,
                PROVIDER_TAB => dirty_tabs.1,
                MODEL_TAB => dirty_tabs.2,
                _ => false,
            };
            nav = nav.child(
                div()
                    .id(("settings-tab", i))
                    .role(Role::Tab)
                    .aria_selected(active)
                    .h_flex()
                    .flex_none()
                    .items_center()
                    .gap_2p5()
                    .px_3()
                    .py_1p5()
                    .rounded_lg()
                    .cursor_pointer()
                    .text_size(TEXT_BODY)
                    .text_color(rgb(INK))
                    .when(active, |d| d.bg(rgb(SELECTED)))
                    .when(!active, |d| d.hover(|d| d.bg(rgb(HOVER))))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select_settings_tab(i, window, cx);
                    }))
                    .child(Icon::new(icon).size_4().text_color(rgb(INK_SOFT)))
                    .child(div().flex_1().min_w_0().child(label))
                    .when(tab_dirty, |d| {
                        d.child(div().size(px(6.)).flex_shrink_0().rounded_full().bg(rgb(CORAL)))
                    }),
            );
        }
        let save_bar_tab = matches!(f.tab, KERNEL_TAB | PROVIDER_TAB | MODEL_TAB);
        // 内容列定宽，滚动时不用每帧把整页来回排好几遍（见 `MeasuredWidth`）
        let content_w = f.content_w.get();
        let width_probe = f.content_w.probe();
        let tab_content: Div = match f.tab {
            super::project_view::PROJECT_TAB => self.render_project(cx),
            KERNEL_TAB => self.render_kernel_tab(window, cx),
            PROVIDER_TAB => self.render_provider_tab(window, cx),
            MODEL_TAB => self.render_model_tab(window, cx),
            5 => self.render_rules(cx),
            6 => self.render_mcp(window, cx),
            PLUGINS_TAB => self.render_plugins(cx),
            super::memory_view::MEMORY_TAB => self.render_memory(window, cx),
            _ => self.render_general(cx),
        };
        let app = cx.entity().downgrade();
        let line = rems(1.25).to_pixels(window.rem_size());
        let wheel = canvas(
            |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
            move |_, hitbox, window, _| {
                window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
                    if !phase.capture() || !hitbox.should_handle_scroll(window) {
                        return;
                    }
                    let dy = event.delta.pixel_delta(line).y;
                    if dy == px(0.) {
                        return;
                    }
                    let precise = event.delta.precise()
                        || std::env::var_os("CLAUDESKTOP_WHEEL").is_some_and(|v| v == "direct");
                    app.update(cx, |this, cx| {
                        this.settings_wheel_by(-dy, precise, event.position, cx)
                    })
                    .ok();
                    cx.stop_propagation();
                });
            },
        )
        .absolute()
        .inset_0();

        let title = SETTINGS_TABS.get(f.tab).map_or("通用", |(_, title)| *title);
        let header = div()
            .h_flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .min_h(rems(3.))
            .pb_3()
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .flex_1()
                    .min_w(rems(4.))
                    .when(compact_navigation, |d| {
                        d.child(
                            Self::quiet_button(
                                "settings-back-compact",
                                IconAll::ArrowLeft,
                                Surface::Paper,
                                cx,
                            )
                            .small()
                            .tooltip("返回对话")
                            .on_click(cx.listener(|this, _, _, cx| this.close_settings(cx))),
                        )
                        .child(menu_popover(
                            "settings-category-menu",
                            Anchor::BottomLeft,
                            rems(12.),
                            &cx.entity(),
                            &self.menu_focus,
                            MenuChip::new(SETTINGS_TABS[f.tab].0, title).filled(),
                            move |app, handle, popover| {
                                settings_tab_menu_items(dirty_tabs, app, handle, popover)
                            },
                        ))
                    })
                    .when(!compact_navigation, |d| {
                        d.child(
                            div()
                                .whitespace_nowrap()
                                .text_size(TEXT_HEADING)
                                .font_weight(FontWeight::MEDIUM)
                                .child(title),
                        )
                    }),
            )
            .when(f.tab == KERNEL_TAB, |d| {
                let refreshing = self.kernel_status_busy();
                d.child(
                    Self::quiet_button(
                        "kernel-status-refresh",
                        IconAll::RefreshCw,
                        Surface::Paper,
                        cx,
                    )
                    .small()
                    .label(if refreshing { "刷新中" } else { "刷新" })
                    .disabled(refreshing)
                    .on_click(cx.listener(|this, _, _, cx| this.refresh_kernel_status(cx))),
                )
            })
            .when(f.tab == PROVIDER_TAB, |d| {
                d.child(menu_popover(
                    "profile-add-menu",
                    Anchor::TopRight,
                    rems(20.),
                    &cx.entity(),
                    &self.menu_focus,
                    MenuChip::new(IconAll::Plus, "添加").filled(),
                    add_profile_menu_items,
                ))
            })
            .when(save_bar_tab || dirty, |d| {
                d.child(self.render_save_actions(compact_navigation, dirty, cx))
            });
        let model_page = f.tab == MODEL_TAB;
        let content = div()
            .v_flex()
            .px_6()
            .pt_1()
            .pb_4()
            .when(model_page, |d| d.h_full().min_h(rems(18.)))
            .when_some(content_w, |d, w| d.w(w))
            .child(tab_content);
        div()
            .id("settings")
            .track_focus(&self.settings_focus)
            .flex_1()
            .min_h_0()
            .h_flex()
            .when(!compact_navigation, |d| d.child(nav))
            .child(
                div()
                    .v_flex()
                    .flex_1()
                    .min_w_0()
                    .max_w(FORM_W)
                    .h_full()
                    .mx_auto()
                    .pt_3()
                    .child(div().px_6().child(header))
                    .when_some(f.message.as_ref(), |d, (error, text)| {
                        d.child(
                            div()
                                .px_6()
                                .pb_3()
                                .text_size(TEXT_SMALL)
                                .text_color(rgb(if *error { RED } else { GREEN }))
                                .child(text.clone()),
                        )
                    })
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_h_0()
                            .child(
                                div()
                                    .id("settings-scroll")
                                    .track_scroll(&self.settings_scroll)
                                    .h_full()
                                    .overflow_y_scroll()
                                    .child(content),
                            )
                            .child(wheel)
                            .child(width_probe)
                            .child(
                                Scrollbar::vertical(&self.settings_scroll)
                                    .mode(ScrollbarMode::Always),
                            ),
                    ),
            )
    }
}

/// 为什么去取模型列表，决定结果告诉谁。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fetch {
    /// 设置页的「获取列表」：结果显示在模型那一栏下面
    Form,
    /// 输入框旁的模型菜单：失败了弹提示
    Menu,
    /// 后台顺手探上下文窗口：失败不吭声
    Probe,
    /// 保存后测一下连不连得上：成功、失败都弹提示
    Verify,
}

/// 思考强度能选的项（设置页那一排和模型弹层底栏共用）：「默认」= 不发字段，排在「关」前面；
/// 然后是这个模型的档位；认不出的模型只有「默认 / 兜底」（兜底见 `FALLBACK_LEVELS`）。
pub(super) fn thinking_options(spec: &ThinkingSpec) -> Vec<(Option<ThinkingLevel>, &'static str)> {
    let mut options = vec![(None, "默认")];
    if spec.is_known() {
        options.extend(spec.choices().iter().map(|level| (Some(*level), level.label())));
    } else {
        options.push((Some(ThinkingLevel::Default), ThinkingLevel::Default.label()));
    }
    options
}

/// 高亮哪一项：None 就是「默认」；Some(x) 落到这个模型实际生效的那一档。
/// 认不出的模型只有「兜底」会发字段，存着别的档（从别的模型带过来的）实际什么都不发，算「默认」。
pub(super) fn active_thinking(
    spec: &ThinkingSpec,
    current: Option<ThinkingLevel>,
) -> Option<ThinkingLevel> {
    if !spec.is_known() {
        return current.filter(|c| *c == ThinkingLevel::Default);
    }
    current.and_then(|c| spec.resolve(c)).or(current)
}

/// 「自动」模式的说明（拦什么、放什么、判不出来怎么办）。
fn auto_mode_docs() -> Div {
    let line = |icon, text: &'static str| note(icon, text);
    div()
        .v_flex()
        .gap_2()
        .child(line(IconAll::ShieldBan, "拦：往信任边界外发数据、下载并执行（curl | sh）、force push / reset --hard 丢掉没提交的改动、删它自己没建的文件、生产部署和迁移、改共享基础设施和权限、关掉安全检查、装持久化（SSH key / 计划任务）"))
        .child(line(IconAll::CircleCheck, "放：只读操作、工作目录内的编辑、构建和测试、项目自己声明的依赖安装、当前仓库的 add / commit / 分支操作"))
        .child(line(IconAll::Info, "只判命令、联网、项目外写入；工作目录内的编辑和只读操作直接放行"))
        .child(line(IconAll::Zap, "分类器只看用户说过的话和要执行的动作：助手的解释和工具结果都剥掉，防止助手给自己找理由，也防止网页 / 文件里的注入影响它"))
        .child(line(IconAll::ShieldAlert, "硬熔断：rm -rf / 、删家目录或工作目录、写 .env / shell 启动脚本 / 包管理器配置，这些一律不执行，也不送分类器"))
        .child(line(IconAll::Info, "拦下时理由回给 agent：它要么换个安全做法，要么把打算做的事告诉你、等你回答。连着被拦 3 次或累计 20 次，自动模式会停下来等你"))
        .child(line(
            IconAll::ShieldX,
            "判不出来（端点没回话、回复看不懂）一律按拒绝处理；每次判定多一次请求，算进用量",
        ))
}

/// 单价表写法说明里的示例。
const PRICES_EXAMPLE: &str = "\
# 名称            输入    输出    缓存命中
deepseek-chat     ¥2      ¥8      ¥0.2     x2
claude-sonnet-4   $3      $15     $0.3
@peak 1-5 09:00-12:00 14:00-18:00 +08:00";

/// 端点地址里的主机名（档案列表里当说明）；没写协议头也行。
fn host_of(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.split('/').next().unwrap_or(rest)
}

/// magpie 下载 / 查询失败转成能照着做的话（GitHub 直连慢是最常见原因，本机实测约 70 KB/s）。
fn magpie_net_hint(err: &anyhow::Error) -> String {
    let text = format!("{err:#}");
    if text.contains("github") || text.contains("timed out") || text.contains("超时") {
        format!("{text}；GitHub 直连很慢时给系统设代理环境变量（如 HTTPS_PROXY）后重试")
    } else {
        text
    }
}

fn profile_title(p: &ProviderProfile, name: &str, base_url: &str) -> SharedString {
    if p.magpie {
        "magpie 网关".into()
    } else if !name.trim().is_empty() {
        name.into()
    } else {
        let host = host_of(base_url);
        if host.is_empty() { "自定义服务商".into() } else { host.into() }
    }
}

fn model_options<'a>(
    profiles: &'a [ProviderProfile],
    source: Option<usize>,
    query: &str,
) -> Vec<(usize, &'a str)> {
    let query = query.trim().to_lowercase();
    let mut options = Vec::new();
    for (i, profile) in profiles.iter().enumerate() {
        if source.is_some_and(|source| source != i) {
            continue;
        }
        for model in profile.model_choices() {
            if query.is_empty() || model.to_lowercase().contains(&query) {
                options.push((i, model));
            }
        }
    }
    options
}

fn default_model_menu_items(
    app: &ChatApp,
    handle: &Entity<ChatApp>,
    popover: &Entity<PopoverState>,
) -> MenuList {
    let f = &app.settings_form;
    let options = model_options(&f.profiles, None, &f.model_menu_query);
    let mut menu = MenuList::new("default-model-list", handle, popover)
        .without_shortcuts()
        .scroll_with_bar(rems(20.))
        .header(ChatApp::text_input(&f.model_menu_search));
    for (row, &(i, model)) in options.iter().enumerate() {
        let p = &f.profiles[i];
        if row == 0 || options[row - 1].0 != i {
            menu = menu.child(
                div()
                    .px_2p5()
                    .pt_2()
                    .pb_0p5()
                    .text_size(TEXT_META)
                    .text_color(rgb(INK_SOFT))
                    .child(profile_title(p, &p.name, &p.base_url)),
            );
        }
        let picked = model.to_string();
        menu = menu.pick_in(
            menu_text_item(
                ("default-model", row),
                picked.clone(),
                i == f.active && model == p.model.trim(),
            ),
            move |this, window, cx| this.select_form_model(i, picked.clone(), window, cx),
        );
    }
    if options.is_empty() {
        menu = menu.child(
            div().px_2p5().py_2().text_size(TEXT_SMALL).text_color(rgb(INK_SOFT)).child(
                if f.model_menu_query.trim().is_empty() {
                    "先获取模型列表，或在清单中输入模型名"
                } else {
                    "没有匹配的模型"
                },
            ),
        );
    }
    menu
}

fn profile_menu_items(
    app: &ChatApp,
    handle: &Entity<ChatApp>,
    popover: &Entity<PopoverState>,
) -> MenuList {
    let f = &app.settings_form;
    let mut menu = MenuList::new("model-provider-list", handle, popover).without_shortcuts().pick(
        menu_text_item("all-model-sources", "全部来源", f.model_source.is_none()),
        |this, cx| this.select_model_source(None, cx),
    );
    let mut order: Vec<usize> = (0..f.profiles.len()).collect();
    order.sort_by_key(|&i| (!f.profiles[i].magpie, i));
    for i in order {
        let p = &f.profiles[i];
        menu = menu.child(div().mx_2p5().h(px(1.)).bg(rgb(LINE)).flex_shrink_0()).pick(
            menu_text_item(
                ("model-provider", i),
                profile_title(p, &p.name, &p.base_url),
                f.model_source == Some(i),
            ),
            move |this, cx| this.select_model_source(Some(i), cx),
        );
    }
    menu
}

fn settings_thinking_menu_items(
    app: &ChatApp,
    handle: &Entity<ChatApp>,
    popover: &Entity<PopoverState>,
) -> MenuList {
    let f = &app.settings_form;
    let mut menu = MenuList::new("settings-thinking-list", handle, popover).without_shortcuts();
    if let Some(p) = f.profiles.get(f.active) {
        let spec = claudesktop_builtin::thinking_spec(p.protocol, &p.model);
        let current = active_thinking(&spec, p.params.thinking);
        for (i, (level, label)) in thinking_options(&spec).into_iter().enumerate() {
            menu = menu.pick(
                menu_text_item(("settings-thinking", i), label, current == level),
                move |this, cx| {
                    this.set_form_thinking(level, cx);
                },
            );
        }
    }
    menu
}

fn kernel_logo(id: &str) -> Icon {
    kernel_icon(id)
}

pub(super) fn kernel_icon(id: &str) -> Icon {
    crate::assets::kernel_logo_path(id)
        .map(|path| Icon::empty().path(path))
        .unwrap_or_else(|| Icon::new(IconAll::SquareTerminal))
}

pub(super) fn provider_icon(base_url: &str) -> Icon {
    crate::assets::provider_logo_for_url(base_url)
        .and_then(crate::assets::provider_logo_path)
        .map(|path| Icon::empty().path(path))
        .unwrap_or_else(|| Icon::new(IconAll::Server))
}

fn settings_tab_menu_items(
    dirty_tabs: (bool, bool, bool),
    app: &ChatApp,
    handle: &Entity<ChatApp>,
    popover: &Entity<PopoverState>,
) -> MenuList {
    let mut menu = MenuList::new("settings-category-list", handle, popover).without_shortcuts();
    for (i, (_, title)) in SETTINGS_TABS.into_iter().enumerate() {
        if i == super::usage_view::USAGE_TAB {
            continue;
        }
        let tab_dirty = match i {
            KERNEL_TAB => dirty_tabs.0,
            PROVIDER_TAB => dirty_tabs.1,
            MODEL_TAB => dirty_tabs.2,
            _ => false,
        };
        let label: SharedString =
            if tab_dirty { format!("{title} · 未保存").into() } else { title.into() };
        menu = menu.pick_in(
            menu_text_item(("settings-category", i), label, app.settings_form.tab == i),
            move |this, window, cx| this.select_settings_tab(i, window, cx),
        );
    }
    menu
}

fn protocol_menu_items(
    app: &ChatApp,
    handle: &Entity<ChatApp>,
    popover: &Entity<PopoverState>,
) -> MenuList {
    let current = app.settings_form.current().map(|(p, _)| p.protocol);
    let mut menu = MenuList::new("profile-protocol-list", handle, popover).without_shortcuts();
    for (i, protocol) in
        [Protocol::OpenAiCompat, Protocol::Anthropic, Protocol::Responses].into_iter().enumerate()
    {
        menu = menu.pick(
            menu_text_item(("profile-protocol", i), protocol.label(), current == Some(protocol)),
            move |this, cx| this.set_form_protocol(protocol, cx),
        );
    }
    menu
}

/// 「添加」菜单：常用服务预设 + 自定义。
fn add_profile_menu_items(
    _: &ChatApp,
    handle: &Entity<ChatApp>,
    popover: &Entity<PopoverState>,
) -> MenuList {
    let mut menu = MenuList::new("profile-add-list", handle, popover);
    for (i, p) in preset_profiles().into_iter().enumerate() {
        menu = menu.pick_in(
            menu_item(
                ("preset", i),
                provider_icon(&p.base_url),
                p.name.clone(),
                host_of(&p.base_url).to_string(),
                false,
            ),
            move |this, window, cx| this.add_form_profile(Some(i), window, cx),
        );
    }
    menu.pick_in(
        menu_item(
            "preset-custom",
            IconAll::Plus,
            "自定义",
            "其他兼容的服务，自己填端点地址",
            false,
        ),
        |this, window, cx| this.add_form_profile(None, window, cx),
    )
}

#[cfg(test)]
mod model_management_tests {
    use super::*;

    #[core::prelude::v1::test]
    fn management_keeps_hidden_and_manual_models_separate_by_source() {
        let profiles = vec![
            ProviderProfile {
                model: "manual".into(),
                models: vec!["shared".into(), "GPT-5".into()],
                menu_hidden: vec!["shared".into()],
                ..Default::default()
            },
            ProviderProfile {
                model: "shared".into(),
                models: vec!["shared".into()],
                ..Default::default()
            },
        ];
        assert_eq!(model_options(&profiles, None, "shared"), vec![(0, "shared"), (1, "shared")]);
        assert_eq!(model_options(&profiles, Some(0), "manual"), vec![(0, "manual")]);
        assert_eq!(model_options(&profiles, Some(1), "manual"), Vec::<(usize, &str)>::new());
        assert_eq!(model_options(&profiles, None, " gPt "), vec![(0, "GPT-5")]);
        assert_eq!(model_options(&profiles, None, "").len(), 4);
    }

    #[core::prelude::v1::test]
    fn thinking_bar_uses_each_models_presets() {
        let toggle = claudesktop_builtin::thinking_spec(Protocol::OpenAiCompat, "qwen3-max");
        assert_eq!(
            thinking_options(&toggle),
            vec![
                (None, "默认"),
                (Some(ThinkingLevel::Off), "关"),
                (Some(ThinkingLevel::High), "高"),
            ]
        );

        let multi = claudesktop_builtin::thinking_spec(Protocol::OpenAiCompat, "glm-5.3");
        assert_eq!(
            thinking_options(&multi),
            vec![
                (None, "默认"),
                (Some(ThinkingLevel::Low), "低"),
                (Some(ThinkingLevel::High), "高"),
                (Some(ThinkingLevel::Max), "最高"),
            ]
        );

        let unknown = claudesktop_builtin::thinking_spec(Protocol::OpenAiCompat, "new-model");
        assert_eq!(
            thinking_options(&unknown),
            vec![(None, "默认"), (Some(ThinkingLevel::Default), "兜底")]
        );
    }

    #[core::prelude::v1::test]
    fn thinking_bar_highlights_the_level_effective_on_the_new_model() {
        let source = claudesktop_builtin::thinking_spec(Protocol::OpenAiCompat, "gpt-5.2");
        assert_eq!(
            active_thinking(&source, Some(ThinkingLevel::XHigh)),
            Some(ThinkingLevel::XHigh)
        );

        let toggle = claudesktop_builtin::thinking_spec(Protocol::OpenAiCompat, "qwen3-max");
        assert_eq!(active_thinking(&toggle, Some(ThinkingLevel::XHigh)), Some(ThinkingLevel::High));

        let no_off = claudesktop_builtin::thinking_spec(Protocol::OpenAiCompat, "glm-5.3");
        assert_eq!(active_thinking(&no_off, Some(ThinkingLevel::Off)), Some(ThinkingLevel::Low));

        let unknown = claudesktop_builtin::thinking_spec(Protocol::OpenAiCompat, "new-model");
        assert_eq!(active_thinking(&unknown, Some(ThinkingLevel::High)), None);
        assert_eq!(
            active_thinking(&unknown, Some(ThinkingLevel::Default)),
            Some(ThinkingLevel::Default)
        );
    }
}

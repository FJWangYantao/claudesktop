//! 设置页（内核 / provider 档案 / 模型）与输入框旁的模型下拉。

use super::ChatApp;
use super::menu::{
    FONT_PREVIEW, MenuChip, MenuList, font_menu_item, menu_item, menu_popover, menu_text_item,
    theme_menu_item,
};
use super::motion::{self, Reveal};
use super::settings_kit::*;
use super::widgets::{MeasuredWidth, Surface};
use crate::model::FailureKind;
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

/// 设置页「插件」分类的下标（侧栏的「插件」入口直接打开它）
pub(super) const PLUGINS_TAB: usize = 7;

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
    /// 首页引导选了「本机 agent」时检测还没跑完：跑完再顺手启用（见 `start_setup`）
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

    pub(crate) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // 记住上次看的 tab（打开时不动它），要指定 tab 的走 `open_settings_at`；
        // 停在用量页时回到进用量页之前的分类（用量不算设置的分类）
        let mut tab = self.settings_form.tab;
        if tab == super::usage_view::USAGE_TAB {
            tab = self.usage_page.settings_tab;
        }
        self.open_settings_at(tab, window, cx);
    }

    /// 打开设置页并切到第 `tab` 个分类（下标见 `SETTINGS_TABS`）。
    /// 失败卡片的「打开设置」用它直接落到服务商配置。
    pub(crate) fn open_settings_at(
        &mut self,
        tab: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 字体下拉要系统的字体清单，先热一次缓存（首次枚举要上百毫秒）
        crate::theme::installed_fonts(cx);
        // 滑杆位置跟上当前设置（可能在别处被改过）
        let scale = self.settings.font_scale * 100.;
        self.font_slider.update(cx, |state, cx| state.set_value(scale, window, cx));
        let s = self.settings.clone();
        self.settings_form.kernel = s.kernel();
        self.settings_form.message = None;
        self.settings_form.fetch_result = None;
        self.settings_form.usage_gateway = s.usage_gateway;
        self.settings_form.gateway_bodies = s.gateway_bodies;
        self.settings_form.confirm_clear_gateway = false;
        let prices = crate::pricing::to_text(&crate::pricing::Table {
            prices: s.prices(),
            peaks: s.price_peaks(),
        });
        self.settings_form.prices.update(cx, |state, cx| state.set_value(prices, window, cx));
        self.settings_form.rules = PermissionRules::load(default_rules_path()).list();
        // 「自动」模式的分类器模型和信任边界
        let auto = s.auto_mode.clone();
        self.settings_form.auto_enabled = auto.enabled;
        self.settings_form.auto_follow = auto.use_session_model;
        self.settings_form
            .auto_model
            .update(cx, |state, cx| state.set_value(auto.model.clone(), window, cx));
        self.settings_form
            .auto_environment
            .update(cx, |state, cx| state.set_value(auto.environment.clone(), window, cx));
        // 输入补全
        self.settings_form.suggest_enabled = s.suggestions;
        self.settings_form.next_suggestion = s.next_suggestion;
        self.settings_form.memory_suggest = s.memory_suggest;
        self.settings_form
            .suggest_model
            .update(cx, |state, cx| state.set_value(s.suggest_model.clone(), window, cx));
        self.settings_form
            .subagent_model
            .update(cx, |state, cx| state.set_value(s.subagent_model.clone(), window, cx));
        // 聊天空间的一组（模型 / 默认联网 / 聊天说明文件）
        self.settings_form.chat_web = s.chat.web;
        self.settings_form
            .chat_model
            .update(cx, |state, cx| state.set_value(s.chat.model.clone(), window, cx));
        self.settings_form
            .chat_instructions
            .update(cx, |state, cx| state.set_value(s.chat.instructions.clone(), window, cx));
        // web_search 的 Brave key（环境变量优先；文件里没有时输入框留空）
        let brave = std::env::var(claudesktop_builtin::websearch::BRAVE_KEY_ENV)
            .ok()
            .or_else(claudesktop_builtin::websearch::brave_key)
            .unwrap_or_default();
        self.settings_form.brave_key.update(cx, |state, cx| state.set_value(brave, window, cx));
        // web_search 的后端选择 + DeepSeek 搜索单独填的 key
        self.settings_form.search = claudesktop_builtin::websearch::load();
        let deepseek = claudesktop_builtin::websearch::own_deepseek_key().unwrap_or_default();
        self.settings_form
            .deepseek_search_key
            .update(cx, |state, cx| state.set_value(deepseek, window, cx));
        // 步数上限：默认值留空时显示实际生效的数（占位符就是默认值）
        self.settings_form.max_steps.update(cx, |state, cx| {
            state.set_value(
                if s.max_steps == claudesktop_builtin::DEFAULT_MAX_STEPS {
                    String::new()
                } else {
                    s.max_steps.to_string()
                },
                window,
                cx,
            )
        });
        // 档案：一份都没有时给一套空的可直接填
        let mut profiles = s.builtin.profiles.clone();
        if profiles.is_empty() {
            profiles.push(ProviderProfile::default());
        }
        self.settings_form.active = s.builtin.active.min(profiles.len() - 1);
        self.settings_form.view = self.settings_form.active;
        self.settings_form.editing_profile = false;
        self.settings_form.model_source = None;
        self.settings_form.model_menu_query.clear();
        self.settings_form
            .model_menu_search
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.settings_form.inputs =
            profiles.iter().map(|p| ProfileInputs::new(window, cx, p)).collect();
        self.settings_form.profiles = profiles;
        // agent 档案表 + 自定义 agent 的命令
        self.settings_form.agents = s.acp.agents.clone();
        let custom = s
            .acp
            .agents
            .iter()
            .find(|a| a.preset().is_none())
            .and_then(|a| a.command.clone())
            .unwrap_or_default();
        self.settings_form
            .custom_command
            .update(cx, |state, cx| state.set_value(custom, window, cx));
        self.settings_form.tab = tab;
        self.settings_wheel = None;
        self.model_wheel = None;
        self.settings_scroll.set_offset(point(px(0.), px(0.)));
        self.settings_form.model_scroll.set_offset(point(px(0.), px(0.)));
        self.settings_open = true;
        // 设置页开着时输入框不在画面上：光标后面那句灰字先撤掉（关掉设置页后接着打字会重猜）
        self.suggestion.clear();
        // 「记忆」一栏按需读两份记忆文件（内核可能刚记下一条）
        if tab == super::memory_view::MEMORY_TAB {
            self.reload_memory_view();
        }
        // 「项目」一栏重读项目根的那个配置文件（可能是手改过的）
        if tab == super::project_view::PROJECT_TAB {
            self.reload_project_view(window, cx);
        }
        if tab == KERNEL_TAB && matches!(self.settings_form.agent_detect, AgentDetect::Idle) {
            self.detect_agents(cx);
        }
        cx.notify();
    }

    fn kernel_status_busy(&self) -> bool {
        matches!(self.settings_form.agent_detect, AgentDetect::Running)
            || self.settings_form.magpie.probing
            || self.settings_form.magpie.starting
            || self.settings_form.magpie.downloading.is_some()
    }

    fn refresh_kernel_status(&mut self, cx: &mut Context<Self>) {
        if self.kernel_status_busy() {
            return;
        }
        self.detect_agents(cx);
        self.probe_magpie(cx);
        cx.notify();
    }

    /// 后台探一遍本机装了哪些 agent（PATH + 常见安装目录，每家起一次子进程问版本）。
    fn detect_agents(&mut self, cx: &mut Context<Self>) {
        if matches!(self.settings_form.agent_detect, AgentDetect::Running) {
            return;
        }
        self.settings_form.agent_detect = AgentDetect::Running;
        cx.spawn(async move |this, cx| {
            let found = cx
                .background_executor()
                .spawn(async move { claudesktop_core::agents::discover() })
                .await;
            this.update(cx, |this, cx| {
                this.settings_form.agent_detect = AgentDetect::Done(found);
                if std::mem::take(&mut this.settings_form.enable_on_detect) {
                    this.enable_detected_agent();
                }
                cx.notify();
            })
        })
        .detach();
    }

    /// 手动刷新网关状态，检测期间保留上次结果。
    fn probe_magpie(&mut self, cx: &mut Context<Self>) {
        let magpie = &mut self.settings_form.magpie;
        if magpie.probing || magpie.starting || magpie.downloading.is_some() {
            return;
        }
        magpie.probing = true;
        let port = claudesktop_builtin::magpie::gateway_port();
        cx.spawn(async move |this, cx| {
            let found = cx
                .background_executor()
                .spawn(async move { claudesktop_builtin::magpie::probe(port) })
                .await;
            let _ = this.update(cx, |this, cx| {
                let magpie = &mut this.settings_form.magpie;
                magpie.probing = false;
                magpie.probed = true;
                if found.is_some() {
                    magpie.error = None;
                }
                magpie.running = found;
                cx.notify();
            });
        })
        .detach();
    }

    /// 下载 magpie CLI（约 75 MB，后台线程）并启动网关。已下载过就直接启动。
    fn setup_magpie(&mut self, cx: &mut Context<Self>) {
        let data_dir = claudesktop_core::default_data_dir()
            .unwrap_or_else(|_| std::env::temp_dir().join("claudesktop-magpie"));
        let exe = claudesktop_builtin::magpie::installed_path(&data_dir);
        if exe.is_file() {
            self.start_magpie(&exe, cx);
            return;
        }
        self.settings_form.magpie.downloading = Some((0, 0));
        self.settings_form.magpie.error = None;
        // 下载线程拿走一份，下载完回来还要拿同一路径去启动
        let exe_for_start = exe.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let release = claudesktop_builtin::magpie::latest_release()?;
                    claudesktop_builtin::magpie::download(&release, &exe, &mut |_, _| {}, &|| {
                        false
                    })?;
                    Ok(())
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.settings_form.magpie.downloading = None;
                match result {
                    Ok(()) => this.start_magpie(&exe_for_start, cx),
                    Err(err) => {
                        this.settings_form.magpie.error = Some(magpie_net_hint(&err));
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    /// 启动 sidecar（已在跑的网关它会让位复用），起来后探活确认。
    fn start_magpie(&mut self, exe: &std::path::Path, cx: &mut Context<Self>) {
        self.settings_form.magpie.starting = true;
        self.settings_form.magpie.error = None;
        let exe = exe.to_path_buf();
        cx.spawn(async move |this, cx| {
            let port = claudesktop_builtin::magpie::gateway_port();
            let spawned = cx
                .background_executor()
                .spawn(async move { claudesktop_builtin::magpie::spawn(&exe) })
                .await;
            let child = match spawned {
                Ok(child) => child,
                Err(err) => {
                    let _ = this.update(cx, |this, cx| {
                        this.settings_form.magpie.starting = false;
                        this.settings_form.magpie.probed = true;
                        this.settings_form.magpie.error = Some(format!("magpie 没能启动：{err}"));
                        cx.notify();
                    });
                    return;
                }
            };
            // 挂到 ChatApp 上：应用退出时连带收掉（其他平台的进程守卫是空操作）
            let _ = this.update(cx, |this, _| {
                this.magpie_child = Some(child);
            });
            // serve 起监听要几秒：探到 /api/hello 应答为止，最多等 15 秒
            let mut ok = None;
            for _ in 0..15 {
                cx.background_executor().timer(std::time::Duration::from_secs(1)).await;
                if let Some(version) = cx
                    .background_executor()
                    .spawn(async move { claudesktop_builtin::magpie::probe(port) })
                    .await
                {
                    ok = Some(version);
                    break;
                }
            }
            let _ = this.update(cx, |this, cx| {
                let magpie = &mut this.settings_form.magpie;
                magpie.starting = false;
                magpie.probed = true;
                match ok {
                    Some(version) => {
                        magpie.running = Some(version);
                        this.toast(
                            Notification::success("magpie 网关已就绪，点「添加为档案」使用"),
                            cx,
                        );
                    }
                    None => {
                        magpie.error =
                            Some("网关没有应答：试试在终端里手动跑 `magpie serve` 看报错".into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 启动引导（见「设计说明-本机agent.md」第十五节）：探到 magpie 网关就自动建档案
    /// （没有的话；之前什么都没配好就选它）并拉 `/v1/models` 填模型下拉；
    /// 网关没跑、当前档案是 magpie 而 sidecar 已下载——顺手起 serve。
    pub(crate) fn magpie_bootstrap(&mut self, cx: &mut Context<Self>) {
        if self.settings_form.magpie.probed || self.settings_form.magpie.probing {
            return;
        }
        self.settings_form.magpie.probing = true;
        let port = claudesktop_builtin::magpie::gateway_port();
        cx.spawn(async move |this, cx| {
            let running = cx
                .background_executor()
                .spawn(async move { claudesktop_builtin::magpie::probe(port) })
                .await;
            match running {
                Some(version) => {
                    // 网关已在跑（用户自己开的也算）：确保 magpie 档案在、模型列表满上
                    let _ = this.update(cx, |this, cx| {
                        this.settings_form.magpie.probing = false;
                        this.settings_form.magpie.probed = true;
                        this.settings_form.magpie.running = Some(version);
                        this.update_settings(|s| {
                            let base = format!("http://127.0.0.1:{port}/v1");
                            if s.builtin.profiles.iter().any(|p| p.base_url == base) {
                                return;
                            }
                            let unconfigured =
                                s.builtin.profiles.iter().all(|p| !p.is_configured());
                            s.builtin.profiles.push(crate::settings::magpie_profile(port));
                            if unconfigured {
                                s.builtin.active = s.builtin.profiles.len() - 1;
                            }
                        });
                        cx.notify();
                    });
                    let base = format!("http://127.0.0.1:{port}/v1");
                    let list = cx
                        .background_executor()
                        .spawn(async move { claudesktop_builtin::magpie::models(port) })
                        .await;
                    if let Ok(list) = list
                        && !list.is_empty()
                    {
                        let _ = this.update(cx, |this, cx| {
                            this.update_settings(|s| {
                                if let Some(p) =
                                    s.builtin.profiles.iter_mut().find(|p| p.base_url == base)
                                {
                                    p.set_models(&list);
                                }
                            });
                            cx.notify();
                        });
                    }
                }
                None => {
                    let _ = this.update(cx, |this, cx| {
                        let magpie = &mut this.settings_form.magpie;
                        magpie.probing = false;
                        magpie.probed = true;
                        magpie.running = None;
                        this.auto_start_magpie_if_active(cx);
                        cx.notify();
                    });
                }
            }
        })
        .detach();
    }

    /// 尽量自动：当前自研内核档案是 magpie、网关没在跑、sidecar 已下载——顺手起 serve。
    pub(crate) fn auto_start_magpie_if_active(&mut self, cx: &mut Context<Self>) {
        let magpie = &self.settings_form.magpie;
        if !self.settings.builtin.current().magpie
            || !magpie.probed
            || magpie.probing
            || magpie.running.is_some()
            || magpie.starting
            || magpie.downloading.is_some()
        {
            return;
        }
        if let Ok(dir) = claudesktop_core::default_data_dir() {
            let exe = claudesktop_builtin::magpie::installed_path(&dir);
            if exe.is_file() {
                self.start_magpie(&exe, cx);
            }
        }
    }

    /// 表单当前的值 → 设置（档案里的模型列表沿用已保存的，除非表单里重新获取过）。
    /// 视图模式、外观这类即时生效的偏好：改内存里的设置，并只把这一项写回盘上的设置文件
    /// （内存里的设置合并过环境变量，不能整份写回；文件坏了就不碰，免得用默认值盖掉）。
    pub(crate) fn save_ui_pref(&mut self, edit: impl Fn(&mut Settings), cx: &mut Context<Self>) {
        /// 保存失败提示的 id：拖字号滑杆时每一下都会存，同一条提示原地替换，不堆叠
        struct SavePrefFailed;

        edit(&mut self.settings);
        if let Some(path) = &self.settings_path {
            let (mut on_disk, broken) = Settings::load(Some(path));
            if broken.is_none() {
                edit(&mut on_disk);
                if let Err(err) = on_disk.save(path) {
                    self.toast(
                        Notification::error(format!("设置没能保存：{err}")).id::<SavePrefFailed>(),
                        cx,
                    );
                }
            }
        }
    }

    /// 把「看不了图片」的模型名单交给已经起来的内核（没起来的启动时从设置带上）。
    pub(crate) fn push_no_vision_models(&self) {
        let models = self.settings.no_vision_models.clone();
        for kernel in self.kernels.iter().filter_map(|(_, k)| k.kernel.as_ref()) {
            kernel.set_no_vision_models(models.clone());
        }
    }

    /// 按设置（或系统外观）切浅色 / 深色：我们自己的色板和 gpui-kit 组件（输入框、提示、滚动条）一起切。
    pub(crate) fn apply_appearance(&mut self, window: &Window, cx: &mut Context<Self>) {
        let dark = match self.settings.appearance {
            Appearance::System => {
                matches!(
                    window.appearance(),
                    WindowAppearance::Dark | WindowAppearance::VibrantDark
                )
            }
            Appearance::Light => false,
            Appearance::Dark => true,
        };
        crate::theme::set_dark(dark);
        let mode = if dark { ThemeMode::Dark } else { ThemeMode::Light };
        if Theme::global(cx).mode != mode {
            Theme::change(mode, None, cx);
        }
        // 外观之后再套字号 / 字体：`Theme::change` 会载入主题文件里的字号，盖掉我们设的值
        self.apply_font(cx);
        cx.notify();
    }

    /// 把字号缩放、字体族和色板装进组件主题。Root 每帧读 `Theme::font_size` 写进窗口的 rem，
    /// 所以这里改完，下一帧整套界面（字号 / 间距 / 圆角 / 图标）等比缩放。
    pub(crate) fn apply_font(&mut self, cx: &mut Context<Self>) {
        // 色板先设：后面 `Theme::update` 里的 `apply_to_theme` 按它写组件配色
        crate::theme::set_palette(self.settings.theme.index());
        crate::theme::set_font_scale(self.settings.font_scale);
        // 没装的字体族会被忽略（GPUI 排版找不到字体族时会 panic）
        if let Some(name) = self.settings.font_family_or_default()
            && !crate::theme::installed_fonts(cx).iter().any(|f| f == name.as_ref())
        {
            self.settings.font_family = String::new();
        }
        crate::theme::set_font_family(cx, self.settings.font_family_or_default());
        Theme::update(cx, crate::theme::apply_to_theme);
        cx.notify();
    }

    /// 设置页换配色：立即生效，并只把这一项写回文件。
    pub(crate) fn set_theme(&mut self, theme: ThemeChoice, cx: &mut Context<Self>) {
        self.save_ui_pref(move |s| s.theme = theme, cx);
        self.apply_font(cx);
    }

    /// 设置页调字号（拖动和松手都走这里）：立即生效，并只把这一项写回文件。
    pub(crate) fn set_font_scale(&mut self, scale: f32, cx: &mut Context<Self>) {
        self.save_ui_pref(move |s| s.font_scale = scale, cx);
        self.apply_font(cx);
    }

    /// 设置页换界面字体族；None = 回到内嵌的 Inter。
    pub(crate) fn set_font_family(&mut self, family: Option<SharedString>, cx: &mut Context<Self>) {
        let value = family.map(|f| f.to_string()).unwrap_or_default();
        self.save_ui_pref(move |s| s.font_family = value.clone(), cx);
        self.apply_font(cx);
    }

    pub(crate) fn set_appearance(
        &mut self,
        a: Appearance,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        self.save_ui_pref(move |s| s.appearance = a, cx);
        self.apply_appearance(window, cx);
    }

    /// 设置页切「动画」：立即生效并写回。
    pub(crate) fn set_motion_pref(&mut self, pref: MotionPref, cx: &mut Context<Self>) {
        self.save_ui_pref(move |s| s.motion = pref, cx);
        self.apply_motion_pref(cx);
        cx.notify();
    }

    /// 侧栏底部要不要列出内核状态（立即生效，不走「保存」）。
    pub(crate) fn set_sidebar_kernel_status(&mut self, on: bool, cx: &mut Context<Self>) {
        self.save_ui_pref(move |s| s.sidebar_kernel_status = on, cx);
        cx.notify();
    }

    /// 「通用」分类：外观、字号、字体、侧栏、通知（都即时生效，不走「保存」），以及设置文件在哪。
    fn render_general(&self, cx: &mut Context<Self>) -> Div {
        let motion = self.settings.motion;
        let motion_control = seg_control(
            "motion",
            MotionPref::ALL.map(|m| (m, m.label())),
            motion,
            |this, m, _, cx| this.set_motion_pref(m, cx),
            cx,
        );
        let motion_row = setting_row("动画", motion_control);
        let appearance = setting_group([
            setting_row(
                "明暗",
                seg_control(
                    "appearance",
                    Appearance::ALL.map(|a| (a, a.label())),
                    self.settings.appearance,
                    |this, a, window, cx| this.set_appearance(a, window, cx),
                    cx,
                ),
            ),
            self.theme_row(cx),
            motion_row,
            self.auto_load_images_row(cx),
            self.font_scale_row(),
            self.font_family_row(cx),
        ]);
        let reminders = setting_group([
            setting_row(
                "系统通知",
                Switch::new("notifications")
                    .checked(self.settings.notifications)
                    .color(rgb(CORAL))
                    .on_change(cx.listener(|this, checked, _, cx| {
                        let on = *checked;
                        this.save_ui_pref(move |s| s.notifications = on, cx);
                        cx.notify();
                    })),
            ),
            setting_row(
                "侧栏显示内核状态",
                Switch::new("sidebar-kernel-status")
                    .checked(self.settings.sidebar_kernel_status)
                    .color(rgb(CORAL))
                    .on_change(cx.listener(|this, checked, _, cx| {
                        this.set_sidebar_kernel_status(*checked, cx)
                    })),
            ),
        ]);
        div()
            .v_flex()
            .gap_5()
            .child(section("外观", None, appearance))
            .child(section("提醒", None, reminders))
            .child(section("数据", None, setting_group([self.settings_file_row(cx)])))
    }

    /// 设置文件在哪：路径 + 「打开文件夹」。
    fn settings_file_row(&self, cx: &mut Context<Self>) -> Div {
        let path = self.settings_path.clone();
        let location = match &path {
            Some(p) => div().font_family(MONO).text_size(TEXT_META).child(p.display().to_string()),
            None => div().text_color(rgb(RED)).child("数据目录不可用，设置不会保存"),
        };
        setting_row_with(
            "设置文件",
            location,
            div().when_some(path, |d, path| {
                d.child(
                    Self::text_button("settings-reveal", "打开文件夹", false)
                        .small()
                        .on_click(cx.listener(move |_, _, _, cx| cx.reveal_path(&path))),
                )
            }),
        )
    }

    /// 「自动加载回复里的网络图片」：默认关，关着时远程图片先摆占位卡等用户点「加载」。
    fn auto_load_images_row(&self, cx: &mut Context<Self>) -> Div {
        setting_row(
            "自动加载回复里的网络图片",
            Switch::new("auto-load-images")
                .checked(self.settings.auto_load_images)
                .color(rgb(CORAL))
                .on_change(cx.listener(|this, checked, _, cx| {
                    let on = *checked;
                    crate::app::remote_image::set_auto_load(on);
                    this.save_ui_pref(move |s| s.auto_load_images = on, cx);
                    cx.notify();
                })),
        )
    }

    /// 字号滑杆：右边带当前百分比。事件订阅在 `ChatApp::new`（`SliderState` 自己 emit），
    /// 拖动与松手都即时生效。
    fn font_scale_row(&self) -> Div {
        let percent = (self.settings.font_scale * 100.).round();
        let slider = div()
            .h_flex()
            .items_center()
            .gap_3()
            .w(rems(15.))
            .child(div().flex_1().min_w_0().child(Slider::new(&self.font_slider)))
            .child(
                // 宽度用 rem：字号放大后「140%」也不会折成两行
                div()
                    .w(rems(3.))
                    .flex_shrink_0()
                    .whitespace_nowrap()
                    .text_right()
                    .text_size(TEXT_SMALL)
                    .text_color(rgb(INK))
                    .font_features(tabular_nums())
                    .child(format!("{percent:.0}%")),
            );
        setting_row("界面字号", slider)
    }

    /// 配色下拉：九套色板，每项下面一行色块预览。换配色即时生效。
    fn theme_row(&self, cx: &mut Context<Self>) -> Div {
        let app = cx.entity();
        setting_row(
            "配色",
            menu_popover(
                "theme-menu",
                Anchor::BottomRight,
                rems(17.5),
                &app,
                &self.menu_focus,
                MenuChip::new(IconAll::Palette, self.settings.theme.label()),
                theme_menu_items,
            ),
        )
    }

    /// 字体族下拉：内嵌的 Inter + 系统已装字体（下拉里可搜索）。
    fn font_family_row(&self, cx: &mut Context<Self>) -> Div {
        let app = cx.entity();
        let label: SharedString =
            self.settings.font_family_or_default().unwrap_or_else(|| "Inter（内嵌）".into());
        setting_row(
            "界面字体",
            menu_popover(
                "font-family-menu",
                Anchor::BottomRight,
                rems(17.5),
                &app,
                &self.menu_focus,
                MenuChip::new(IconAll::ALargeSmall, label),
                font_family_menu_items,
            ),
        )
    }

    fn form_settings(&self, cx: &App) -> Settings {
        let f = &self.settings_form;
        let mut s = self.settings.clone();
        let mut profiles = f.profiles.clone();
        for (p, inputs) in profiles.iter_mut().zip(&f.inputs) {
            p.name = value(&inputs.name, cx);
            p.base_url = value(&inputs.base_url, cx);
            p.api_key = value(&inputs.api_key, cx);
            // 思考强度（`p.params.thinking`）由档位开关直接写在表单的档案里，这里不用再收
            // 服务端工具（`p.params.server_tools`）同样由多选开关直接写
        }
        s.builtin.profiles = profiles;
        s.builtin.active = f.active.min(s.builtin.profiles.len().saturating_sub(1));
        // agent 档案表：预设行的开关直接写在表单里；自定义命令有值就保留 / 建一条档案
        let mut agents = f.agents.clone();
        let custom = value(&f.custom_command, cx);
        let custom = custom.trim();
        match agents.iter().position(|a| a.preset().is_none()) {
            Some(at) => {
                if custom.is_empty() {
                    agents.remove(at);
                } else {
                    agents[at].command = Some(custom.to_string());
                }
            }
            None if !custom.is_empty() => {
                let n = agents
                    .iter()
                    .filter_map(|a| {
                        a.id.strip_prefix("custom-").and_then(|v| v.parse::<usize>().ok())
                    })
                    .max()
                    .map_or(1, |n| n + 1);
                agents.push(AgentProfile::custom(n, custom));
            }
            None => {}
        }
        s.acp.agents = agents;
        s.kernel = None;
        s.max_steps = parse_max_steps(&value(&f.max_steps, cx));
        s.auto_mode.enabled = f.auto_enabled;
        s.auto_mode.use_session_model = f.auto_follow;
        s.auto_mode.model = value(&f.auto_model, cx);
        s.auto_mode.environment = f.auto_environment.read(cx).value().to_string();
        s.suggestions = f.suggest_enabled;
        s.next_suggestion = f.next_suggestion;
        s.memory_suggest = f.memory_suggest;
        s.suggest_model = value(&f.suggest_model, cx);
        s.subagent_model = value(&f.subagent_model, cx);
        // 聊天空间：模型（空 = 跟档案）、默认联网、聊天说明文件
        s.chat.model = value(&f.chat_model, cx);
        s.chat.web = f.chat_web;
        s.chat.instructions = value(&f.chat_instructions, cx);
        s.usage_gateway = f.usage_gateway;
        s.gateway_bodies = f.gateway_bodies;
        // 单价表写错了留着原来的（`save_settings` 先报错，不会走到这里）；和内置的一样就不存
        if let Ok(table) = crate::pricing::parse_text(&f.prices.read(cx).value()) {
            let defaults = crate::pricing::Table::defaults();
            s.model_prices = (table.prices != defaults.prices).then_some(table.prices);
            s.price_peaks = (table.peaks != defaults.peaks).then_some(table.peaks);
        }
        s
    }

    /// 失败卡片上的「打开设置」：落到服务商配置，页头写着上一轮为什么失败、该改哪一项。
    pub(crate) fn open_settings_for_failure(
        &mut self,
        kind: FailureKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tab = if matches!(kind, FailureKind::Model) { MODEL_TAB } else { PROVIDER_TAB };
        self.open_settings_at(tab, window, cx);
        let why = match kind {
            FailureKind::Auth => "上一轮被服务商拒绝：检查下面的 API Key，改好后保存",
            FailureKind::Endpoint => "上一轮找不到接口：检查下面的端点地址，改好后保存",
            FailureKind::Model => {
                "上一轮找不到模型：点「获取列表」挑一个能用的；当前会话的模型在输入框右下角换"
            }
            FailureKind::Network => "上一轮连不上服务：检查下面的端点地址，以及网络和代理",
            _ => return,
        };
        self.settings_form.message = Some((true, why.into()));
    }

    /// 首页配置引导选了一种接法：打开「内核」并选好那种内核；
    /// 还没启用任何 agent 且本机装了 Claude Code 时顺手把它启用上。
    pub(crate) fn start_setup(
        &mut self,
        kind: KernelChoice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_settings_at(KERNEL_TAB, window, cx);
        self.settings_form.kernel = kind.clone();
        if matches!(kind, KernelChoice::Acp(_)) {
            // 设置页刚打开时检测多半还在跑：跑完再启用
            match self.settings_form.agent_detect {
                AgentDetect::Done(_) => self.enable_detected_agent(),
                _ => self.settings_form.enable_on_detect = true,
            }
        }
        cx.notify();
    }

    /// 还没启用任何 agent、表单选的是「本机 agent」时，把检测到的第一家能用的启用上（只改表单）。
    fn enable_detected_agent(&mut self) {
        let f = &mut self.settings_form;
        if !matches!(f.kernel, KernelChoice::Acp(_)) || !f.agents.is_empty() {
            return;
        }
        let AgentDetect::Done(list) = &f.agent_detect else { return };
        let Some(hit) = list.iter().find(|d| d.is_ready()) else { return };
        let preset = hit.preset;
        f.agents.push(AgentProfile::from_preset(preset));
        f.message = Some((false, format!("已启用本机的{}，点「保存」生效", preset.name).into()));
    }

    fn save_settings(&mut self, cx: &mut Context<Self>) {
        if let Err(err) = crate::pricing::parse_text(&self.settings_form.prices.read(cx).value()) {
            self.settings_form.message = Some((true, err.into()));
            cx.notify();
            return;
        }
        let new = self.form_settings(cx);
        // 选了「本机 agent」却一家都没启用：先让用户勾一家（或填一条自定义命令）
        if matches!(new.kernel(), KernelChoice::Acp(_)) && new.acp.first().is_none() {
            self.settings_form.message = Some((
                true,
                "选了「本机 agent」但一家都没启用：在上面勾一家，或填一条自定义命令".into(),
            ));
            cx.notify();
            return;
        }
        if new.kernel() == KernelChoice::Builtin {
            if let Err(err) = new.builtin_config(self.cwd.clone()) {
                self.settings_form.message = Some((true, err.to_string().into()));
                cx.notify();
                return;
            }
            // 开着「自动」模式就必须配好：跟会话模型走要当前档案配好，自己指定模型要填模型名
            if !new.auto_mode.is_configured(new.builtin.current()) {
                let why = if new.builtin.current().is_configured() {
                    "「自动」模式开着：要么选「跟随当前模型」，要么在上面填分类器用哪个模型"
                } else {
                    "「自动」模式开着：先在「服务商」页配好端点，再到「模型」页选模型"
                };
                self.settings_form.message = Some((true, why.into()));
                cx.notify();
                return;
            }
        }
        if let Some(path) = &self.settings_path
            && let Err(err) = new.save(path)
        {
            self.settings_form.message = Some((true, format!("保存失败：{err}").into()));
            cx.notify();
            return;
        }
        // 只重启配置变了的内核（模型列表变了不算）
        let builtin_key = |s: &Settings| {
            let mut b = s.builtin.clone();
            for p in &mut b.profiles {
                p.models.clear();
                p.context_windows.clear();
            }
            b
        };
        let restart_builtin = builtin_key(&new) != builtin_key(&self.settings)
            || new.max_steps != self.settings.max_steps
            || new.usage_gateway != self.settings.usage_gateway
            // 一轮结束的候选记忆开关变了：内核要重装才知道
            || new.memory_suggest != self.settings.memory_suggest
            // 子任务模型：内核开子会话时读
            || new.subagent_model != self.settings.subagent_model
            // 聊天空间的一组（模型 / 默认联网 / 说明文件）也要重装才知道
            || new.chat != self.settings.chat
            // 自动模式的开关 / 分类器模型变了：模式列表和分类器都要重装
            || new.auto_mode != self.settings.auto_mode;
        // 只重启启动命令 / 环境变量变了的档案：改一家的网关开关不该把另一家跑着的一轮杀掉
        //（`last_model`、显示名这些不影响进程的不算）
        let launch = |s: &Settings, id: &str| {
            s.acp.get(id).map(|p| {
                let config = s.acp_config_for(self.acp_base.clone(), Some(p));
                (config.command, config.env)
            })
        };
        let changed_acp: Vec<String> = new
            .acp
            .agents
            .iter()
            .filter(|a| launch(&new, &a.id) != launch(&self.settings, &a.id))
            .map(|a| a.id.clone())
            .collect();
        let restart_acp = new.acp != self.settings.acp;
        // 根据当前模型配置和启用的 agent 自动选择新会话的内核。
        let default = new.kernel();
        let default_changed = default != self.settings.kernel();
        // 存不存原文对已经跑着的网关立刻生效，不用重启内核
        claudesktop_builtin::request_bodies::set_enabled(new.gateway_bodies);
        self.settings = new;
        self.settings_open = false;
        if restart_builtin {
            self.restart_kernel(&KernelChoice::Builtin, cx);
        }
        // 用量网关总开关变了也会改 agent 的环境变量，所以不只看档案表
        for id in changed_acp {
            self.restart_kernel(&KernelChoice::Acp(id), cx);
        }
        if restart_acp {
            // 停用 / 删掉的档案：进程直接关（它们的会话已经全部脱离）
            let mut removed: Vec<String> = self
                .kernels
                .iter()
                .filter_map(|(k, _)| k.strip_prefix("acp:"))
                .filter(|id| self.settings.acp.get(id).is_none())
                .map(|id| id.to_string())
                .collect();
            removed.sort();
            removed.dedup();
            for id in removed {
                self.shutdown_slot(&KernelChoice::Acp(id).key());
            }
        }
        // 自研内核的配置变了：顺手取一次模型列表，连不上马上告诉用户（不挡保存）
        if restart_builtin && default == KernelChoice::Builtin {
            self.start_fetch_models(Fetch::Verify, cx);
        }
        if default_changed {
            // 默认内核换了：还没对话的会话跟着换，有对话的留在原来的内核上
            for idx in 0..self.sessions.len() {
                self.rebind_session(idx, default.clone());
            }
        }
        // 首次配置自研内核：把等着的会话接上；外部 agent 仍等首次发送。
        if default == KernelChoice::Builtin
            && !self.slot(&default.key()).is_some_and(|s| s.started)
            && self.sessions.iter().any(|s| s.kernel == default && !s.has_user_message())
        {
            self.ensure_kernel(&default);
            self.request_pending_sessions();
        }
        // 刚把 magpie 档案存成当前档案（或换了它的模型）：网关没在跑就顺手起 serve
        if default == KernelChoice::Builtin {
            self.auto_start_magpie_if_active(cx);
        }
        // 端点填了、模型列表还是空的档案：后台补拉一次，「模型」页直接有东西可看
        self.fetch_missing_models(cx);
        cx.notify();
    }

    /// 保存后给「端点已配置、模型列表还是空」的档案后台补拉一次模型列表。
    /// 成功的写进设置和表单（来源行 / 清单立刻有数）；失败不打扰。magpie 的列表
    /// 由 `magpie_bootstrap` 喂，不在这里拉。
    fn fetch_missing_models(&mut self, cx: &mut Context<Self>) {
        let configs: Vec<ProviderConfig> = self
            .settings
            .builtin
            .profiles
            .iter()
            .filter(|p| !p.magpie && !p.base_url.trim().is_empty() && p.models.is_empty())
            .map(|p| {
                let mut config = p.provider_config();
                // 和「获取列表」一样经用量网关（日志里记成 models）
                config.gateway = self.settings.gateway();
                config
            })
            .collect();
        if configs.is_empty() {
            return;
        }
        self.fetching_models = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let mut fetched: Vec<(String, Vec<claudesktop_builtin::ModelInfo>)> = Vec::new();
            for config in configs {
                let base = config.base_url.clone();
                let list = cx
                    .background_executor()
                    .spawn(async move { claudesktop_builtin::fetch_models(config) })
                    .await;
                if let Ok(models) = list {
                    fetched.push((base, models));
                }
            }
            let _ = this.update(cx, |this, cx| {
                this.fetching_models = false;
                if fetched.is_empty() {
                    return;
                }
                this.update_settings(|s| {
                    for (base, models) in &fetched {
                        if let Some(p) =
                            s.builtin.profiles.iter_mut().find(|p| p.base_url.trim() == base.trim())
                        {
                            p.set_models(models);
                        }
                    }
                });
                // 表单里那份同步（设置页开着时来源行 / 清单立即有数）
                for (base, models) in &fetched {
                    if let Some(p) = this
                        .settings_form
                        .profiles
                        .iter_mut()
                        .find(|p| p.base_url.trim() == base.trim())
                    {
                        p.set_models(models);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    // ---------- 档案编辑（表单内） ----------

    /// 查看服务商 Profile，不改全局默认模型。
    fn select_form_profile(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        if i < self.settings_form.profiles.len() {
            self.settings_form.view = i;
            self.settings_form.editing_profile = false;
            self.settings_form.fetch_result = None;
            self.settings_wheel = None;
            self.model_wheel = None;
            self.settings_form.model_scroll.set_offset(point(px(0.), px(0.)));
            // 搜索词是上一个来源的，别带过来
            self.settings_form
                .model_search
                .update(cx, |s, cx| s.set_value(String::new(), window, cx));
            cx.notify();
        }
    }

    pub(crate) fn select_form_model(
        &mut self,
        profile: usize,
        model: String,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let model = model.trim().to_string();
        if model.is_empty() || profile >= self.settings_form.profiles.len() {
            return;
        }
        let f = &mut self.settings_form;
        f.profiles[profile].model = model;
        f.active = profile;
        f.message = None;
        cx.notify();
    }

    fn set_form_thinking(&mut self, level: Option<ThinkingLevel>, cx: &mut Context<Self>) {
        let active = self.settings_form.active;
        if let Some(profile) = self.settings_form.profiles.get_mut(active) {
            profile.params.thinking = level;
        }
        self.settings_form.message = None;
        cx.notify();
    }

    #[cfg(feature = "headless-screenshot")]
    pub(crate) fn debug_settings_preview(
        &mut self,
        preview: &str,
        scale: Option<f32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(scale) = scale {
            self.settings.font_scale = (scale / 100.).clamp(FONT_SCALE_MIN, FONT_SCALE_MAX);
            self.apply_font(cx);
        }
        if let Some(i) = preview.strip_prefix("edit:").and_then(|i| i.parse().ok()) {
            self.select_form_profile(i, window, cx);
            self.settings_form.editing_profile = true;
        } else if let Some(query) = preview.strip_prefix("search:") {
            self.settings_form
                .model_search
                .update(cx, |state, cx| state.set_value(query, window, cx));
        } else if let Some(query) = preview.strip_prefix("menu:") {
            self.settings_form
                .model_menu_search
                .update(cx, |state, cx| state.set_value(query, window, cx));
            self.settings_form.model_menu_query = query.into();
        } else if preview == "refresh" {
            if let Ok(endpoint) = std::env::var("CLAUDESKTOP_TEST_PROVIDER_URL") {
                assert!(endpoint.starts_with("http://127.0.0.1:"));
                let profiles: Vec<_> = ["a", "b"]
                    .into_iter()
                    .map(|source| ProviderProfile {
                        name: format!("验证来源 {source}"),
                        base_url: format!("{endpoint}/{source}/v1"),
                        model: "shared".into(),
                        ..Default::default()
                    })
                    .collect();
                self.settings_form.inputs =
                    profiles.iter().map(|p| ProfileInputs::new(window, cx, p)).collect();
                self.settings_form.profiles = profiles;
                self.settings_form.active = 0;
                self.settings_form.view = 0;
                self.settings_form.model_source = None;
                self.settings.usage_gateway = false;
                self.refresh_model_sources(cx);
            }
        } else if preview == "dark" || preview == "light" {
            self.settings.appearance =
                if preview == "dark" { Appearance::Dark } else { Appearance::Light };
            self.apply_appearance(window, cx);
        } else if preview == "kernel-status" {
            self.open_settings_at(KERNEL_TAB, window, cx);
        } else if preview == "advanced" {
            self.settings_form.open_help.insert("kernel-advanced");
        } else if preview == "auto" {
            self.settings_form.auto_enabled = true;
            self.settings_form.auto_follow = false;
        } else if preview == "check" {
            let sidebar = self.sidebar_open;
            let profiles = vec![
                ProviderProfile {
                    name: "验证来源 A".into(),
                    base_url: "https://first.invalid/v1".into(),
                    model: "gpt-5".into(),
                    models: vec!["gpt-5".into()],
                    ..Default::default()
                },
                ProviderProfile {
                    name: "验证来源 B".into(),
                    base_url: "https://second.invalid/v1".into(),
                    model: "gpt-5".into(),
                    models: vec!["gpt-5".into()],
                    ..Default::default()
                },
            ];
            self.settings_form.inputs =
                profiles.iter().map(|p| ProfileInputs::new(window, cx, p)).collect();
            self.settings_form.profiles = profiles;
            self.settings_form.active = 0;
            self.settings_form.view = 0;
            self.select_model_source(None, cx);
            self.select_form_model(1, "gpt-5".into(), window, cx);
            assert_eq!(self.settings_form.active, 1);
            assert_eq!(self.settings_form.model_source, None);
            self.select_model_source(Some(0), cx);
            assert_eq!(self.settings_form.active, 1);
            self.set_form_thinking(Some(ThinkingLevel::Low), cx);
            assert_eq!(self.settings_form.profiles[0].params.thinking, None);
            assert_eq!(self.settings_form.profiles[1].params.thinking, Some(ThinkingLevel::Low));
            let draft = self.form_settings(cx);
            assert_eq!(draft.builtin.current().base_url, "https://second.invalid/v1");
            self.remove_form_profile(0, cx);
            assert_eq!(self.settings_form.active, 0);
            assert_eq!(self.settings_form.model_source, None);
            assert_eq!(self.settings_form.profiles[0].name, "验证来源 B");
            self.select_form_model(0, "manual-model".into(), window, cx);
            assert!(
                model_options(&self.settings_form.profiles, None, "manual")
                    .iter()
                    .any(|(_, model)| *model == "manual-model")
            );
            assert!(self.form_dirty(cx));
            self.open_settings_at(MODEL_TAB, window, cx);
            assert!(!self.form_dirty(cx));
            assert_eq!(self.sidebar_open, sidebar);
            assert!(self.settings_navigation_only());
            eprintln!(
                "settings checks passed: source filter, default routing, thinking ownership, removal, manual model, discard, sidebar state"
            );
        }
        cx.notify();
    }

    #[cfg(feature = "headless-screenshot")]
    pub(crate) fn debug_refresh_finished(&self, verify: bool) -> bool {
        if self.fetching_models {
            return false;
        }
        if verify {
            let profiles = &self.settings_form.profiles;
            assert!(profiles[0].models.iter().any(|m| m == "only-a"));
            assert!(!profiles[0].models.iter().any(|m| m == "only-b"));
            assert!(profiles[1].models.iter().any(|m| m == "only-b"));
            assert!(!profiles[1].models.iter().any(|m| m == "only-a"));
            assert!(self.settings_form.fetch_result.as_ref().is_some_and(|(error, _, _)| !*error));
            eprintln!(
                "provider refresh checks passed: two real HTTP calls, shared IDs kept separate"
            );
        }
        true
    }

    #[cfg(feature = "headless-screenshot")]
    pub(crate) fn debug_kernel_status_ready(&self) -> bool {
        !self.kernel_status_busy()
            && self.settings_form.magpie.probed
            && matches!(self.settings_form.agent_detect, AgentDetect::Done(_))
    }

    #[cfg(feature = "headless-screenshot")]
    pub(crate) fn debug_check_kernel_status(
        &mut self,
        refreshed: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        assert!(self.debug_kernel_status_ready());
        if refreshed {
            assert_eq!(value(&self.settings_form.max_steps, cx), "77");
            self.open_settings_at(KERNEL_TAB, window, cx);
            assert!(self.debug_kernel_status_ready());
            eprintln!(
                "kernel status checks passed: tab switches and reopen reuse cached results; manual refresh completes and keeps drafts"
            );
            return;
        }
        let running = self.settings_form.magpie.running.clone();
        let AgentDetect::Done(agents) = &self.settings_form.agent_detect else { unreachable!() };
        let agents = agents.clone();
        for tab in [PROVIDER_TAB, MODEL_TAB, KERNEL_TAB, 0, KERNEL_TAB] {
            self.select_settings_tab(tab, window, cx);
            assert!(self.debug_kernel_status_ready());
            assert_eq!(self.settings_form.magpie.running, running);
            let AgentDetect::Done(found) = &self.settings_form.agent_detect else { unreachable!() };
            assert_eq!(found, &agents);
        }
        self.settings_open = false;
        self.open_settings_at(KERNEL_TAB, window, cx);
        self.magpie_bootstrap(cx);
        assert!(self.debug_kernel_status_ready());
        assert_eq!(self.settings_form.magpie.running, running);
        self.settings_form.max_steps.update(cx, |input, cx| input.set_value("77", window, cx));
        self.refresh_kernel_status(cx);
        assert!(matches!(self.settings_form.agent_detect, AgentDetect::Running));
        assert!(self.settings_form.magpie.probing);
        self.refresh_kernel_status(cx);
        assert!(self.kernel_status_busy());
    }

    pub(crate) fn ask_remove_form_profile(
        &mut self,
        i: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let f = &self.settings_form;
        let (Some(profile), Some(inputs)) = (f.profiles.get(i), f.inputs.get(i)) else {
            return;
        };
        if profile.magpie || window.has_active_dialog(cx) {
            return;
        }
        let name = profile_title(profile, &value(&inputs.name, cx), &value(&inputs.base_url, cx));
        let target = inputs.name.clone();
        let app = cx.entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let app = app.clone();
            let target = target.clone();
            alert
                .title("确认删除？")
                .description(format!("将移除「{name}」及其连接配置，保存后生效。"))
                .show_cancel(true)
                .ok_text("删除")
                .ok_variant(ButtonVariant::Danger)
                .cancel_text("取消")
                .on_ok(move |_, _, cx| {
                    app.update(cx, |this, cx| {
                        let index = this
                            .settings_form
                            .inputs
                            .iter()
                            .position(|inputs| inputs.name == target);
                        if let Some(index) = index {
                            this.remove_form_profile(index, cx);
                        }
                    });
                    true
                })
        });
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
        f.editing_profile = false;
        if len == 0 {
            f.active = 0;
            f.view = 0;
        } else {
            if i < f.active {
                f.active -= 1;
            }
            if i < f.view {
                f.view -= 1;
            }
            f.active = f.active.min(len - 1);
            f.view = f.view.min(len - 1);
        }
        f.model_source = f.model_source.and_then(|source| {
            if source == i { None } else { Some(if source > i { source - 1 } else { source }) }
        });
        self.model_wheel = None;
        f.model_scroll.set_offset(point(px(0.), px(0.)));
        cx.notify();
    }

    /// 表单里追加一套档案（预设 `preset = None` 时是空白自定义）。
    /// 选中的那套还什么都没填（首次配置时补的空档案）就直接换掉它，不留一套空的。
    fn add_form_profile(
        &mut self,
        preset: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let p = preset.map_or_else(ProviderProfile::default, |i| {
            let mut p = preset_profiles()[i].clone();
            p.api_key = String::new();
            p.set_models(&[]);
            p
        });
        let inputs = ProfileInputs::new(window, cx, &p);
        let f = &mut self.settings_form;
        let blank = f.current().is_some_and(|(p, i)| {
            p.model.trim().is_empty()
                && [&i.name, &i.base_url, &i.api_key]
                    .iter()
                    .all(|input| value(input, cx).is_empty())
        });
        if blank {
            f.profiles[f.view] = p;
            f.inputs[f.view] = inputs;
        } else {
            f.inputs.push(inputs);
            f.profiles.push(p);
            // 新来源只加入清单；选了它的模型才会改变全局默认
            f.view = f.profiles.len() - 1;
        }
        f.editing_profile = true;
        f.message = None;
        f.fetch_result = None;
        self.model_wheel = None;
        f.model_scroll.set_offset(point(px(0.), px(0.)));
        cx.notify();
    }

    /// 表单里切正在查看那套档案的协议。
    fn set_form_protocol(&mut self, protocol: Protocol, cx: &mut Context<Self>) {
        if let Some(p) = self.settings_form.profiles.get_mut(self.settings_form.view) {
            p.protocol = protocol;
            cx.notify();
        }
    }

    /// 改一处设置并只把这一处写回文件：生效中的设置可能含有环境变量给的值
    /// （如 key），没点「保存」就不该落盘。
    pub(super) fn update_settings(&mut self, change: impl Fn(&mut Settings)) {
        change(&mut self.settings);
        if let Some(path) = &self.settings_path {
            // 盘上的文件读不出来（手改坏了、新版本的格式）时别用「默认值 + 这次改动」盖掉它
            let (mut saved, broken) = Settings::load(Some(path));
            if broken.is_none() {
                change(&mut saved);
                let _ = saved.save(path);
            }
        }
    }

    /// 后台获取模型列表。`from_form` = 用设置页里正在编辑的档案；否则用生效中的设置。
    pub(crate) fn fetch_models(&mut self, from_form: bool, cx: &mut Context<Self>) {
        self.start_fetch_models(if from_form { Fetch::Form } else { Fetch::Menu }, cx);
    }

    /// 一轮结束或切到一个会话时：自研内核报告过上下文用量、但还不知道当前模型的窗口大小，
    /// 就悄悄取一次模型列表（窗口大小随列表缓存在档案里）。每次启动最多试一次，失败不提示。
    pub(crate) fn probe_context_window(&mut self, idx: usize, cx: &mut Context<Self>) {
        let s = &self.sessions[idx];
        if self.context_probed
            || s.kernel != KernelChoice::Builtin
            || s.context_tokens == 0
            || self.settings.builtin.context_window(&s.model_label).is_some()
        {
            return;
        }
        self.context_probed = true;
        self.start_fetch_models(Fetch::Probe, cx);
    }

    fn start_fetch_models(&mut self, why: Fetch, cx: &mut Context<Self>) {
        if self.fetching_models {
            return;
        }
        let from_form = why == Fetch::Form;
        let mut config = if from_form {
            let f = &self.settings_form;
            let Some(inputs) = f.inputs.get(f.view) else {
                return;
            };
            ProviderConfig {
                protocol: f.profiles.get(f.view).map_or(Protocol::OpenAiCompat, |p| p.protocol),
                base_url: value(&inputs.base_url, cx),
                api_key: Some(value(&inputs.api_key, cx)).filter(|k| !k.is_empty()),
                model: String::new(),
                ..Default::default()
            }
        } else {
            self.settings.builtin.provider()
        };
        // 列模型的请求也经用量网关（日志里记成 models）
        config.gateway = self.settings.gateway();
        if config.base_url.is_empty() {
            // 后台探测时自研内核可能已经没配了（只剩历史会话），不往设置页塞错误提示
            if from_form {
                self.settings_form.fetch_result = Some((true, "先填端点地址".into(), None));
                cx.notify();
            }
            return;
        }
        let form_active = if from_form { Some(self.settings_form.view) } else { None };
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
                                p.set_models(&models);
                            }
                            app.settings_form.fetch_result =
                                Some((false, format!("连上了，获取到 {n} 个模型").into(), None));
                        }
                        if why == Fetch::Verify {
                            let profile = app.settings.builtin.current();
                            let (name, model) = (profile.display_name(), profile.model.trim());
                            // 列表里没有填的模型：多半是模型名写错了（列表为空的服务不算）
                            let note = if models.is_empty() || models.iter().any(|m| m.id == model)
                            {
                                Notification::success(format!("已连上 {name}，可以开始对话了"))
                            } else {
                                Notification::warning(format!(
                                    "已连上 {name}，但它的模型列表里没有「{model}」。检查模型名，或点「获取列表」挑一个"
                                ))
                            };
                            app.toast(note, cx);
                        }
                        // 和生效中的端点一致时，顺便更新下拉用的列表
                        if base_url == app.settings.builtin.base_url().trim() {
                            app.update_settings(|s| s.builtin.current_mut().set_models(&models));
                            // 运行中的自研内核也要知道窗口大小（自动压缩用）
                            if let Some(kernel) =
                                &app.kernel_of(&KernelChoice::Builtin)
                            {
                                let windows = &app.settings.builtin.current().context_windows;
                                kernel.set_context_windows(windows.clone().into_iter().collect());
                            }
                        }
                    }
                    Err(err) => {
                        let raw = err.to_string();
                        let text = crate::model::fetch_failure_text(&raw, &base_url);
                        match why {
                            Fetch::Form => {
                                app.settings_form.fetch_result =
                                    Some((true, text.into(), Some(raw.into())))
                            }
                            Fetch::Menu => app.toast(
                                Notification::error(format!("获取模型失败：{text}（{raw}）")),
                                cx,
                            ),
                            // 保存时顺手测的：已经存了，只是提醒一下
                            Fetch::Verify => app.toast(
                                Notification::warning(format!(
                                    "已保存，但连接测试没通过：{text}（{raw}）"
                                )),
                                cx,
                            ),
                            Fetch::Probe => {}
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
        let rules = &self.settings_form.rules;
        let body = if rules.is_empty() {
            div().v_flex().gap_3().child(how_to(
                "还没有记下的规则",
                &[
                    "运行命令、改文件或联网前，对话里会弹出权限卡片",
                    "选「总是允许」或「总是拒绝」，同类操作以后不再问",
                    "记下的规则列在这里，点 × 删掉就会重新问",
                ],
            ))
        } else {
            let mut list = div().v_flex().gap_3();
            let mut n = 0usize;
            for (project, items) in rules {
                let mut card = div()
                    .v_flex()
                    .rounded_xl()
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
                                    .text_color(rgb(INK_SOFT))
                                    .child(project.clone()),
                            ),
                    );
                for rule in items {
                    let (p, r) = (project.clone(), rule.clone());
                    card = card.child(
                        div()
                            .h_flex()
                            .gap_2()
                            .pl_3()
                            .pr_1p5()
                            .py_1()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(TEXT_BODY)
                                    .text_color(rgb(INK))
                                    .child(rule_label(rule)),
                            )
                            .child(
                                Self::quiet_button(
                                    ("rule-remove", n),
                                    IconAll::X,
                                    Surface::Paper,
                                    cx,
                                )
                                .small()
                                .tooltip("删除这条规则")
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.remove_rule(p.clone(), r.clone(), cx)
                                    },
                                )),
                            ),
                    );
                    n += 1;
                }
                list = list.child(card);
            }
            list
        };
        let body = body.when_some(self.settings_form.message.clone(), |d, (error, text)| {
            d.child(
                div()
                    .text_size(TEXT_SMALL)
                    .text_color(rgb(if error { RED } else { GREEN }))
                    .child(text),
            )
        });
        section("总是允许的操作", None, body).child(self.render_web_search(cx))
    }

    /// 「网页搜索」：先用的后端 + 允许哪些后端 + 各自的 key。
    ///
    /// 选择存在内核那边的数据目录 `search.json`（搜索是即时读取的，不经 `Kernel::set_*`），
    /// 所以这里不走页头「保存」：改了就写盘，下一轮搜索立刻用新顺序。默认顺序和加 DeepSeek
    /// 之前一致（Brave → DuckDuckGo），DeepSeek 是按次计费的新花费，要用户自己选。
    fn render_web_search(&self, cx: &mut Context<Self>) -> Div {
        use claudesktop_builtin::websearch::SearchBackend;
        let f = &self.settings_form;
        let cfg = &f.search;
        // 状态一行：按「配没配齐」算，不联网验（见设计说明：不做「测试」按钮）
        let own_key = !f.deepseek_search_key.read(cx).value().trim().is_empty();
        let deepseek_key = own_key || self.deepseek_search_key_from_profiles().is_some();
        let brave_ready = claudesktop_builtin::websearch::brave_key().is_some();
        let order = cfg.order();
        let status: SharedString = if order.is_empty() {
            "所有后端都不可用，web_search 会直接报错".into()
        } else {
            let names: Vec<&str> = order.iter().map(|b| b.label()).collect();
            let mut line = format!("当前：{}", names.join(" → "));
            if order.contains(&SearchBackend::DeepSeek) && !deepseek_key {
                line.push_str("（DeepSeek 没配 key，会跳过它）");
            }
            if order.contains(&SearchBackend::Brave) && !brave_ready {
                line.push_str("（Brave 没配 key，会跳过它）");
            }
            line.into()
        };
        let prefer = seg_control(
            "search-prefer",
            SearchBackend::ALL.map(|b| (b, b.label())),
            cfg.prefer,
            |this, value, _, cx| {
                let mut cfg = this.settings_form.search.clone();
                cfg.prefer = value;
                // 选了先用它就是要它：对应的「也允许」一起打开，开关和实际顺序对得上
                match value {
                    SearchBackend::DeepSeek => cfg.allow_deepseek = true,
                    SearchBackend::Brave => cfg.allow_brave = true,
                    SearchBackend::DuckDuckGo => cfg.allow_duckduckgo = true,
                }
                this.save_search(cfg, "搜索后端已更新", cx);
            },
            cx,
        );
        let deepseek_toggle = Switch::new("search-allow-deepseek")
            .checked(order.contains(&SearchBackend::DeepSeek))
            .color(rgb(CORAL))
            .on_change(cx.listener(|this, checked: &bool, _, cx| {
                let mut cfg = this.settings_form.search.clone();
                cfg.allow_deepseek = *checked;
                // 「先用」选着 DeepSeek 时它也算允许：关掉就把先用的退回默认
                if !*checked && cfg.prefer == SearchBackend::DeepSeek {
                    cfg.prefer = SearchBackend::Brave;
                }
                this.save_search(cfg, "搜索后端已更新", cx);
            }));
        let brave_toggle = Switch::new("search-allow-brave")
            .checked(cfg.allow_brave)
            .color(rgb(CORAL))
            .on_change(cx.listener(|this, checked: &bool, _, cx| {
                let mut cfg = this.settings_form.search.clone();
                cfg.allow_brave = *checked;
                this.save_search(cfg, "搜索后端已更新", cx);
            }));
        let ddg_toggle = Switch::new("search-allow-ddg")
            .checked(cfg.allow_duckduckgo)
            .color(rgb(CORAL))
            .on_change(cx.listener(|this, checked: &bool, _, cx| {
                let mut cfg = this.settings_form.search.clone();
                cfg.allow_duckduckgo = *checked;
                this.save_search(cfg, "搜索后端已更新", cx);
            }));
        let body = setting_group([
            setting_row("先用哪个", prefer),
            setting_row("也允许 DeepSeek", deepseek_toggle),
            setting_row("也允许 Brave", brave_toggle),
            setting_row("也允许 DuckDuckGo", ddg_toggle),
            setting_row(
                "Brave API key",
                div()
                    .h_flex()
                    .gap_2()
                    .child(div().w(rems(20.)).child(Self::secret_input(&f.brave_key)))
                    .child(Self::text_button("brave-key-save", "保存", false).on_click(
                        cx.listener(|this, _, window, cx| {
                            let key = this.settings_form.brave_key.read(cx).value().to_string();
                            let res = claudesktop_builtin::websearch::set_brave_key(Some(&key));
                            this.settings_form.message = Some(match res {
                                Ok(()) => (false, "Brave key 已保存".into()),
                                Err(e) => (true, format!("保存失败：{e}").into()),
                            });
                            this.settings_form
                                .brave_key
                                .update(cx, |state, cx| state.set_value(key, window, cx));
                            cx.notify();
                        }),
                    )),
            ),
            setting_row(
                "DeepSeek key",
                div()
                    .h_flex()
                    .gap_2()
                    .child(div().w(rems(20.)).child(Self::secret_input(&f.deepseek_search_key)))
                    .child(Self::text_button("deepseek-key-save", "保存", false).on_click(
                        cx.listener(|this, _, window, cx| {
                            let key =
                                this.settings_form.deepseek_search_key.read(cx).value().to_string();
                            let res =
                                claudesktop_builtin::websearch::set_own_deepseek_key(Some(&key));
                            this.settings_form.message = Some(match res {
                                Ok(()) => (false, "DeepSeek 搜索 key 已保存".into()),
                                Err(e) => (true, format!("保存失败：{e}").into()),
                            });
                            this.settings_form
                                .deepseek_search_key
                                .update(cx, |state, cx| state.set_value(key, window, cx));
                            cx.notify();
                        }),
                    )),
            ),
        ]);
        section(
            "网页搜索",
            None,
            div()
                .v_flex()
                .gap_3()
                .child(
                    div()
                        .text_size(TEXT_SMALL)
                        .text_color(rgb(if order.is_empty() { RED } else { INK_SOFT }))
                        .child(status),
                )
                .child(body),
        )
    }

    /// 「服务商」页里 DeepSeek 那份档案的 key（搜索页只说有没有，不显示内容）。
    fn deepseek_search_key_from_profiles(&self) -> Option<String> {
        self.settings_form
            .profiles
            .iter()
            .filter(|p| crate::settings::is_deepseek_endpoint(p))
            .map(|p| p.api_key.trim().to_string())
            .find(|k| !k.is_empty())
    }

    /// 把搜索后端的选择写进数据目录（不用点页头「保存」）。
    fn save_search(
        &mut self,
        cfg: claudesktop_builtin::websearch::SearchConfig,
        ok: &str,
        cx: &mut Context<Self>,
    ) {
        self.settings_form.message = Some(match claudesktop_builtin::websearch::save(&cfg) {
            Ok(()) => {
                self.settings_form.search = cfg;
                (false, ok.to_string().into())
            }
            Err(e) => (true, format!("保存搜索设置失败：{e}").into()),
        });
        cx.notify();
    }

    /// 在模型下拉里选了一个：当前会话立即切换（还没建立的会话等 Started），并记为默认。
    /// 从指定供应商档案挑模型（输入框模型菜单跨来源挑）：不是当前在用的那套就先切过去
    /// （自研内核带着新端点重启，进行中的对话下次发消息自动续接），再记模型。
    pub(crate) fn choose_model_from(
        &mut self,
        profile: usize,
        model: String,
        cx: &mut Context<Self>,
    ) {
        let model = model.trim().to_string();
        if self.settings.builtin.profiles.get(profile).is_none() || model.is_empty() {
            return;
        }
        if profile != self.settings.builtin.active {
            self.update_settings(|s| {
                s.builtin.select_model(profile, &model);
            });
            self.restart_kernel(&KernelChoice::Builtin, cx);
        }
        self.choose_model(model, cx);
    }

    pub(crate) fn choose_model(&mut self, model: String, cx: &mut Context<Self>) {
        // 记住上次用的模型：自研内核记在当前 provider 档案，agent 记在它的档案上
        // （新会话 Started 后按它切过去，见 `Session::apply_event` 的 Started 分支）
        match self.session().kernel.clone() {
            KernelChoice::Builtin => {
                self.update_settings(|s| s.builtin.current_mut().model = model.clone());
            }
            KernelChoice::Acp(id) => {
                let model = model.clone();
                self.update_settings(move |s| s.acp.set_last_model(&id, &model));
            }
            KernelChoice::Demo => {}
        }
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

    /// 表单和生效中的设置有没有不同（页头据此提示、决定「保存」能不能点）。
    fn form_dirty(&self, cx: &App) -> bool {
        // 单价表写错了也算改动：点「保存」才看得到错在哪
        if crate::pricing::parse_text(&self.settings_form.prices.read(cx).value()).is_err() {
            return true;
        }
        let normalize = |mut s: Settings| {
            s.kernel = Some(s.kernel_for(s.kernel.as_ref().unwrap_or(&KernelChoice::Builtin)));
            // 打开设置页时补上的那套空档案不算改动
            let blank = ProviderProfile::default();
            s.builtin.profiles.retain(|p| *p != blank);
            s.builtin.active = s.builtin.active.min(s.builtin.profiles.len().saturating_sub(1));
            s
        };
        normalize(self.form_settings(cx)) != normalize(self.settings.clone())
    }

    /// 「内核与模型」拆页后的「内核」：本机内核清单 + 自研内核的选项（自动模式、运行参数）。
    fn render_kernel_tab(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
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

    fn render_kernel_list(&self, window: &Window, cx: &mut Context<Self>) -> Div {
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
                        .tooltip(move |window, cx| super::chat::text_tip(&name_tip, window, cx))
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
                        .tooltip(move |window, cx| super::chat::text_tip(&status_tip, window, cx))
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
    fn render_advanced(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
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
    fn render_auto_mode(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
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
    fn render_chat(&self, cx: &mut Context<Self>) -> Div {
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
    fn render_no_vision(&self, cx: &mut Context<Self>) -> Option<Div> {
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
    fn render_suggest(&self, cx: &mut Context<Self>) -> Div {
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
    fn render_memory_options(&self, cx: &mut Context<Self>) -> Div {
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

    /// 全局默认模型跨来源选择；来源下拉只筛选下面用于管理的模型清单。
    fn render_model_tab(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
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

    fn render_provider_tab(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
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
                    if p.magpie { IconAll::Cpu } else { IconAll::Server },
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

    fn render_model_list(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
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

    fn select_model_source(&mut self, source: Option<usize>, cx: &mut Context<Self>) {
        self.settings_form.model_source = source.filter(|&i| i < self.settings_form.profiles.len());
        self.settings_form.fetch_result = None;
        self.model_wheel = None;
        self.settings_form.model_scroll.set_offset(point(px(0.), px(0.)));
        cx.notify();
    }

    fn refresh_model_sources(&mut self, cx: &mut Context<Self>) {
        if self.fetching_models {
            return;
        }
        let f = &self.settings_form;
        let jobs: Vec<_> = f
            .profiles
            .iter()
            .enumerate()
            .filter(|(i, _)| f.model_source.is_none_or(|source| *i == source))
            .filter_map(|(i, p)| {
                let inputs = f.inputs.get(i)?;
                let mut config = p.provider_config();
                config.base_url = value(&inputs.base_url, cx);
                config.api_key = Some(value(&inputs.api_key, cx)).filter(|key| !key.is_empty());
                config.gateway = self.settings.gateway();
                Some((inputs.name.clone(), p.clone(), config))
            })
            .collect();
        if jobs.is_empty() {
            return;
        }
        self.fetching_models = true;
        self.settings_form.fetch_result = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let mut count = 0;
            let mut failures = Vec::new();
            for (target, original, config) in jobs {
                let request = config.clone();
                let magpie = original.magpie;
                let result = cx
                    .background_executor()
                    .spawn(async move {
                        if magpie {
                            claudesktop_builtin::magpie::models(
                                claudesktop_builtin::magpie::gateway_port(),
                            )
                            .map_err(|err| err.to_string())
                        } else if request.base_url.trim().is_empty() {
                            Err("先填端点地址".to_string())
                        } else {
                            claudesktop_builtin::fetch_models(request)
                                .map_err(|err| err.to_string())
                        }
                    })
                    .await;
                let _ = this.update(cx, |app, cx| {
                    let Some(i) =
                        app.settings_form.inputs.iter().position(|inputs| inputs.name == target)
                    else {
                        return;
                    };
                    let inputs = &app.settings_form.inputs[i];
                    if value(&inputs.base_url, cx) != config.base_url
                        || value(&inputs.api_key, cx)
                            != config.api_key.as_deref().unwrap_or_default()
                        || app.settings_form.profiles[i].protocol != config.protocol
                    {
                        return;
                    }
                    match result {
                        Ok(models) => {
                            count += models.len();
                            app.settings_form.profiles[i].set_models(&models);
                            let saved = app.settings.builtin.profiles.iter().position(|p| {
                                p.name == original.name
                                    && p.magpie == original.magpie
                                    && p.protocol == config.protocol
                                    && p.base_url.trim() == config.base_url.trim()
                                    && p.api_key.trim()
                                        == config.api_key.as_deref().unwrap_or_default()
                            });
                            if let Some(saved) = saved {
                                app.update_settings(|s| {
                                    s.builtin.profiles[saved].set_models(&models)
                                });
                                if saved == app.settings.builtin.active {
                                    if let Some(kernel) = app.kernel_of(&KernelChoice::Builtin) {
                                        kernel.set_context_windows(
                                            app.settings.builtin.profiles[saved]
                                                .context_windows
                                                .clone()
                                                .into_iter()
                                                .collect(),
                                        );
                                    }
                                }
                            }
                        }
                        Err(error) => failures.push(format!(
                            "{}：{}",
                            original.display_name(),
                            crate::model::fetch_failure_text(&error, &config.base_url)
                        )),
                    }
                    cx.notify();
                });
            }
            let _ = this.update(cx, |app, cx| {
                app.fetching_models = false;
                let failed = !failures.is_empty();
                let text = if failed {
                    failures.join("；")
                } else {
                    format!("已获取 {count} 个模型")
                };
                app.settings_form.fetch_result = Some((failed, text.into(), None));
                cx.notify();
            });
        })
        .detach();
    }

    fn render_profile_summary(&self, p: &ProviderProfile, inputs: &ProfileInputs, cx: &App) -> Div {
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
    fn render_profile_editor(
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

    /// 「思考强度」：紧凑下拉，只编辑全局默认模型所属的档案。
    ///
    /// 能选哪些档跟着选中的全局默认模型现算，
    /// 所以换模型之后这一排跟着变；存在档案里的档位如果新模型不支持，高亮和保存都按
    /// `ThinkingSpec::resolve` 降级后的那一档。
    ///
    /// 模型认不出时照样给这一排，但只有「默认 / 兜底」两项：兜底按 `FALLBACK_LEVELS`
    /// 发 effort，其余档位不发（见 `apply_thinking`）。以前整行不出现，用户看着设置页
    /// 以为没地方调（其实是被能力表挡掉了）。
    fn render_thinking_row(&self, p: &ProviderProfile, model: &str, cx: &mut Context<Self>) -> Div {
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
    fn render_server_tools(&self, p: &ProviderProfile, cx: &mut Context<Self>) -> Div {
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

    fn render_save_actions(&self, compact: bool, cx: &mut Context<Self>) -> Div {
        let dirty = self.form_dirty(cx);
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
                    .on_click(cx.listener(|this, _, _, cx| {
                        if this.settings_form.tab == super::project_view::PROJECT_TAB {
                            this.apply_project(cx);
                        }
                        this.settings_open = false;
                        cx.notify();
                    })),
            );
        for (i, (icon, label)) in SETTINGS_TABS.into_iter().enumerate() {
            if i == super::usage_view::USAGE_TAB {
                continue;
            }
            let active = i == f.tab;
            nav = nav.child(
                div()
                    .id(("settings-tab", i))
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
                    .child(label),
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
                            .on_click(cx.listener(|this, _, _, cx| {
                                if this.settings_form.tab == super::project_view::PROJECT_TAB {
                                    this.apply_project(cx);
                                }
                                this.settings_open = false;
                                cx.notify();
                            })),
                        )
                        .child(menu_popover(
                            "settings-category-menu",
                            Anchor::BottomLeft,
                            rems(12.),
                            &cx.entity(),
                            &self.menu_focus,
                            MenuChip::new(SETTINGS_TABS[f.tab].0, title).filled(),
                            settings_tab_menu_items,
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
            .when(save_bar_tab, |d| d.child(self.render_save_actions(compact_navigation, cx)));
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

/// 思考强度能选的项（设置页那一排和输入框的下拉共用）：「默认」= 不发字段，排在「关」前面；
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
    crate::assets::kernel_logo_path(id)
        .map(|path| Icon::empty().path(path))
        .unwrap_or_else(|| Icon::new(IconAll::SquareTerminal))
}

fn settings_tab_menu_items(
    app: &ChatApp,
    handle: &Entity<ChatApp>,
    popover: &Entity<PopoverState>,
) -> MenuList {
    let mut menu = MenuList::new("settings-category-list", handle, popover).without_shortcuts();
    for (i, (_, title)) in SETTINGS_TABS.into_iter().enumerate() {
        if i == super::usage_view::USAGE_TAB {
            continue;
        }
        menu = menu.pick_in(
            menu_text_item(("settings-category", i), title, app.settings_form.tab == i),
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
                IconAll::Server,
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

/// 规则 → 给人看的说明
fn rule_label(rule: &str) -> String {
    // 拒绝规则带一层前缀：剥掉后复用允许规则的文案，再改「总是拒绝」的说法
    let (deny, rule) = match rule.strip_prefix(claudesktop_builtin::DENY_PREFIX) {
        Some(rest) => (true, rest),
        None => (false, rule),
    };
    if let Some(prefix) = rule.strip_prefix("shell:") {
        let verb = if deny { "总是拒绝运行" } else { "运行" };
        format!("{verb}以「{prefix}」开头的命令")
    } else if let Some(host) = rule.strip_prefix("fetch:") {
        format!("{}网站 {host}", if deny { "总是拒绝访问" } else { "访问" })
    } else if rule == "search" {
        if deny { "总是拒绝搜索网页".into() } else { "搜索网页".into() }
    } else if let Some(tool) = rule.strip_prefix("mcp:") {
        format!(
            "{} MCP 工具 {}",
            if deny { "总是拒绝调用" } else { "调用" },
            tool.replacen(':', " 的 ", 1)
        )
    } else if rule == "edit" {
        if deny {
            "总是拒绝编辑项目内的文件".into()
        } else {
            "编辑项目内的文件".into()
        }
    } else {
        rule.to_string()
    }
}
/// 配色下拉的条目：名称 + 灰色说明，下一行是这套配色在当前明暗下的色块，当前项打勾。
fn theme_menu_items(
    app: &ChatApp,
    handle: &Entity<ChatApp>,
    popover: &Entity<PopoverState>,
) -> MenuList {
    let current = app.settings.theme;
    // 九套色板 × 三行（名称 / 说明 / 色块）比原来高，限高滚动，矮窗口里不至于顶出屏幕
    let mut menu = MenuList::new("theme-list", handle, popover).scroll(rems(26.));
    for t in ThemeChoice::ALL {
        menu = menu.pick(
            theme_menu_item(
                ("theme-item", t.index()),
                t.label(),
                t.desc(),
                t.index(),
                t == current,
            ),
            move |this, cx| this.set_theme(t, cx),
        );
    }
    menu
}

/// 界面字体下拉的条目：第一项是内嵌的 Inter，后面跟系统已装字体（可滚动，最多列一批）。
/// 每项用该字体本身排一行预览（`font_menu_item`），当前项打勾；
/// 列不完的靠「还有 N 款」提示，需要的话在设置文件里手写字体名。
fn font_family_menu_items(
    app: &ChatApp,
    handle: &Entity<ChatApp>,
    popover: &Entity<PopoverState>,
) -> MenuList {
    const MAX_SYSTEM_FONTS: usize = 60;
    let current = app.settings.font_family_or_default();
    let mut menu = MenuList::new("font-family-list", handle, popover).scroll(rems(22.5));

    // 内嵌的 Inter：字体族为空即表示用它
    menu = menu.pick(
        font_menu_item(
            "font-embedded",
            "Inter（内嵌）",
            DEFAULT_SANS,
            FONT_PREVIEW,
            current.is_none(),
        ),
        |this, cx| this.set_font_family(None, cx),
    );

    let fonts = app.system_fonts();
    for (i, name) in fonts.iter().take(MAX_SYSTEM_FONTS).enumerate() {
        let active = current.as_deref() == Some(name.as_str());
        let picked: SharedString = name.clone().into();
        menu = menu.pick(
            font_menu_item(("font-item", i), name.clone(), name.clone(), FONT_PREVIEW, active),
            move |this, cx| this.set_font_family(Some(picked.clone()), cx),
        );
    }
    if fonts.len() > MAX_SYSTEM_FONTS {
        menu = menu.child(
            div()
                .px_2p5()
                .py_1p5()
                .text_size(TEXT_SMALL)
                .text_color(rgb(INK_SOFT))
                .child(format!("还有 {} 款未列出", fonts.len() - MAX_SYSTEM_FONTS)),
        );
    }
    menu
}

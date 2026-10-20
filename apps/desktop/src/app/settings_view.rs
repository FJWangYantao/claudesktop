//! 设置页（内核 / provider 档案 / 模型）与输入框旁的模型下拉。

use super::ChatApp;
use super::keys::{ZOOM_IN_KEY, ZOOM_OUT_KEY};
use super::menu::{
    FONT_PREVIEW, MenuChip, MenuList, font_menu_item, menu_item, menu_popover, theme_menu_item,
};
use super::motion::{self, Reveal};
use super::settings_kit::*;
use super::widgets::{MeasuredWidth, Surface};
use crate::model::FailureKind;
use crate::settings::*;
use crate::theme::rgb;
use crate::theme::*;
use claudesktop_builtin::{PermissionRules, Protocol, ProviderConfig, default_rules_path};
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::base::PopoverState;
use gpui_kit::component::button::{ButtonVariant, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState, Textarea, TextareaState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::slider::Slider;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::theme::{Theme, ThemeMode};
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::collections::HashSet;

/// 设置页左侧的分类（下标即 `SettingsForm::tab`）。
const SETTINGS_TABS: [(IconAll, &str); 8] = [
    (IconAll::Settings, "通用"),
    (IconAll::FolderTree, "项目"),
    (IconAll::Cpu, "内核与模型"),
    (IconAll::ShieldCheck, "权限"),
    (IconAll::Plug, "MCP"),
    (IconAll::Blocks, "插件"),
    // 用量：单独一页，不在导航里列（见 `render_settings`）
    (IconAll::ChartColumn, "用量"),
    (IconAll::Brain, "记忆"),
];

/// 设置页「内核与模型」分类的下标（首页配置引导、失败卡片的「打开设置」直接打开它）
pub(super) const KERNEL_TAB: usize = 2;

/// 设置页「插件」分类的下标（侧栏的「插件」入口直接打开它）
pub(super) const PLUGINS_TAB: usize = 5;

/// 一套档案在表单里的输入框（与 `SettingsForm::profiles` 一一对应）。
pub struct ProfileInputs {
    name: Entity<InputState>,
    base_url: Entity<InputState>,
    api_key: Entity<InputState>,
    model: Entity<InputState>,
    /// 思考预算（`thinking.budget_tokens`）；空 = 不显式开启思考
    thinking_budget: Entity<InputState>,
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
        // 思考预算：1024 是官方下限；没设就留空（占位符顺便提醒最小值）
        let budget = p.params.thinking_budget.map(|b| b.to_string()).unwrap_or_default();
        Self {
            name: make(p.name.clone(), "如 DeepSeek（可留空）", false, window, cx),
            base_url: make(p.base_url.clone(), "如 https://api.deepseek.com/v1", false, window, cx),
            api_key: make(p.api_key.clone(), "没有可留空", true, window, cx),
            model: make(p.model.clone(), "如 deepseek-chat", false, window, cx),
            thinking_budget: make(budget, "空 = 不开启", false, window, cx),
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
    /// 设置页当前所在的 tab（0 通用 / 1 项目 / 2 内核与模型 / 3 权限 / 4 MCP / 5 插件 / 6 用量页 / 7 记忆）
    pub(super) tab: usize,
    acp_command: Entity<InputState>,
    /// 一轮最多几步（自研内核的工具循环上限）
    max_steps: Entity<InputState>,
    /// 用量网关开关（保存才生效，会重启自研内核）
    usage_gateway: bool,
    gateway_bodies: bool,
    /// ACP agent 的请求也经网关（保存才生效，会重启 ACP 内核）
    acp_gateway: bool,
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
    /// 一轮结束后提炼候选记忆开着没有（保存才生效，会重启自研内核）
    memory_suggest: bool,
    /// Brave Search API key（`web_search` 用；空 = DuckDuckGo 兜底）
    brave_key: Entity<InputState>,
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
}

impl SettingsForm {
    pub fn new(window: &mut Window, cx: &mut Context<ChatApp>) -> Self {
        let acp_command = cx.new(|cx| {
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
        let auto_environment = cx.new(|cx| TextareaState::new(window, cx).auto_grow(3, 10));
        let suggest_model = cx.new(|cx| {
            InputState::new(window, cx).placeholder("留空用会话的模型，如 deepseek-chat")
        });
        let brave_key =
            cx.new(|cx| InputState::new(window, cx).placeholder("没有也能搜（DuckDuckGo 兜底）"));
        Self {
            kernel: KernelChoice::default(),
            profiles: Vec::new(),
            active: 0,
            inputs: Vec::new(),
            tab: 0,
            acp_command,
            max_steps,
            usage_gateway: true,
            gateway_bodies: true,
            acp_gateway: false,
            confirm_clear_gateway: false,
            prices,
            auto_model,
            auto_environment,
            auto_enabled: false,
            auto_follow: true,
            suggest_enabled: false,
            next_suggestion: false,
            suggest_model,
            memory_suggest: false,
            brave_key,
            message: None,
            rules: Vec::new(),
            open_help: HashSet::new(),
            content_w: MeasuredWidth::default(),
            fetch_result: None,
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

/// 设置页的「思考预算」输入 → `thinking.budget_tokens`：空着或写了非正数 = 不开启；
/// 小于官方下限（1024）时抬到下限（请求里也会按这个下限卡一次）。
fn parse_thinking_budget(text: &str) -> Option<u64> {
    let text = text.trim();
    let n = text.parse::<u64>().ok().filter(|n| *n > 0)?;
    Some(n.max(claudesktop_builtin::RequestParams::MIN_THINKING_BUDGET))
}

impl ChatApp {
    pub(crate) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // 记住上次看的 tab（打开时不动它），要指定 tab 的走 `open_settings_at`；
        // 停在用量页时回到进用量页之前的分类（用量不算设置的分类）
        let mut tab = self.settings_form.tab;
        if tab == super::usage_view::USAGE_TAB {
            tab = self.usage_page.settings_tab;
        }
        self.open_settings_at(tab, window, cx);
    }

    /// 打开设置页并切到第 `tab` 个分类（0 通用 / 1 项目 / 2 内核与模型 / 3 权限 / 4 MCP / 5 插件 / 6 用量页 / 7 记忆）。
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
        self.settings_form.acp_gateway = s.acp.gateway;
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
        // web_search 的 Brave key（环境变量优先；文件里没有时输入框留空）
        let brave = std::env::var(claudesktop_builtin::websearch::BRAVE_KEY_ENV)
            .ok()
            .or_else(claudesktop_builtin::websearch::brave_key)
            .unwrap_or_default();
        self.settings_form.brave_key.update(cx, |state, cx| state.set_value(brave, window, cx));
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
        self.settings_form.inputs =
            profiles.iter().map(|p| ProfileInputs::new(window, cx, p)).collect();
        self.settings_form.profiles = profiles;
        self.settings_form
            .acp_command
            .update(cx, |state, cx| state.set_value(s.acp.command.clone(), window, cx));
        self.settings_form.tab = tab;
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
        cx.notify();
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

    /// 侧栏底部要不要列出内核状态（立即生效，不走「保存并应用」）。
    pub(crate) fn set_sidebar_kernel_status(&mut self, on: bool, cx: &mut Context<Self>) {
        self.save_ui_pref(move |s| s.sidebar_kernel_status = on, cx);
        cx.notify();
    }

    /// 「通用」分类：外观、字号、字体、侧栏、通知（都即时生效，不走「保存并应用」），以及设置文件在哪。
    fn render_general(&self, cx: &mut Context<Self>) -> Div {
        let motion = self.settings.motion;
        let motion_control = seg_control(
            "motion",
            MotionPref::ALL.map(|m| (m, m.label())),
            motion,
            |this, m, _, cx| this.set_motion_pref(m, cx),
            cx,
        );
        let motion_tip = if motion == MotionPref::System && self.system_reduce_motion {
            "展开收起、转圈这类过渡效果（系统里已关闭）"
        } else {
            "展开收起、转圈这类过渡效果"
        };
        let motion_row = setting_row("动画", motion_tip, motion_control);
        let appearance = setting_group([
            setting_row(
                "明暗",
                "跟随系统，或固定浅色 / 深色；每套配色都有这两版",
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
            self.font_scale_row(),
            self.font_family_row(cx),
        ]);
        let reminders = setting_group([
            setting_row(
                "系统通知",
                "后台时回复完成、等你授权或出错就提醒",
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
                "在侧栏底部显示内核连接状态",
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
            .gap_8()
            .child(section("外观", "", None, appearance))
            .child(section("提醒", "", None, reminders))
            .child(section("数据", "", None, setting_group([self.settings_file_row(cx)])))
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
            "API Key 以明文保存；启动时设置的环境变量优先于这里",
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
        let tip = format!(
            "文字、间距、圆角一起等比缩放；{} 放大，{} 缩小",
            Self::key_text(ZOOM_IN_KEY),
            Self::key_text(ZOOM_OUT_KEY)
        );
        setting_row("界面字号", tip, slider)
    }

    /// 配色下拉：六套色板，每项下面一行色块预览。换配色即时生效。
    fn theme_row(&self, cx: &mut Context<Self>) -> Div {
        let app = cx.entity();
        setting_row(
            "配色",
            "整套界面的底色和强调色",
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
            "代码固定用 JetBrains Mono",
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
        s.kernel = Some(f.kernel);
        let mut profiles = f.profiles.clone();
        for (p, inputs) in profiles.iter_mut().zip(&f.inputs) {
            p.name = value(&inputs.name, cx);
            p.base_url = value(&inputs.base_url, cx);
            p.api_key = value(&inputs.api_key, cx);
            p.model = value(&inputs.model, cx);
            p.params.thinking_budget = parse_thinking_budget(&value(&inputs.thinking_budget, cx));
            // 服务端工具（`p.params.server_tools`）由多选开关直接写在表单的档案里，这里不用再收
        }
        s.builtin.profiles = profiles;
        s.builtin.active = f.active.min(s.builtin.profiles.len().saturating_sub(1));
        s.acp.command = value(&f.acp_command, cx);
        s.acp.gateway = f.acp_gateway;
        s.max_steps = parse_max_steps(&value(&f.max_steps, cx));
        s.auto_mode.enabled = f.auto_enabled;
        s.auto_mode.use_session_model = f.auto_follow;
        s.auto_mode.model = value(&f.auto_model, cx);
        s.auto_mode.environment = f.auto_environment.read(cx).value().to_string();
        s.suggestions = f.suggest_enabled;
        s.next_suggestion = f.next_suggestion;
        s.memory_suggest = f.memory_suggest;
        s.suggest_model = value(&f.suggest_model, cx);
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

    /// 失败卡片上的「打开设置」：落到服务商配置，底栏写着上一轮为什么失败、该改哪一项。
    pub(crate) fn open_settings_for_failure(
        &mut self,
        kind: FailureKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_settings_at(KERNEL_TAB, window, cx);
        let why = match kind {
            FailureKind::Auth => {
                "上一轮被服务商拒绝：检查下面的 API Key，改好后点「获取列表」验证一下"
            }
            FailureKind::Endpoint => {
                "上一轮找不到接口：检查下面的端点地址，改好后点「获取列表」验证一下"
            }
            FailureKind::Model => {
                "上一轮找不到模型：点「获取列表」挑一个能用的；当前会话的模型在输入框右下角换"
            }
            FailureKind::Network => "上一轮连不上服务：检查下面的端点地址，以及网络和代理",
            _ => return,
        };
        self.settings_form.message = Some((true, why.into()));
    }

    /// 首页配置引导选了一种接法：打开「内核与模型」并选好那种内核；
    /// ACP 命令还空着时填上 Claude Code 的适配器。
    pub(crate) fn start_setup(
        &mut self,
        kind: KernelChoice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_settings_at(KERNEL_TAB, window, cx);
        self.settings_form.kernel = kind;
        if kind == KernelChoice::Acp
            && self.settings_form.acp_command.read(cx).value().trim().is_empty()
        {
            let command = acp_command(ACP_PRESETS[0].1);
            self.settings_form
                .acp_command
                .update(cx, |state, cx| state.set_value(command, window, cx));
        }
        cx.notify();
    }

    fn save_settings(&mut self, cx: &mut Context<Self>) {
        if let Err(err) = crate::pricing::parse_text(&self.settings_form.prices.read(cx).value()) {
            self.settings_form.message = Some((true, err.into()));
            cx.notify();
            return;
        }
        let new = self.form_settings(cx);
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
                    "「自动」模式开着：先在「服务商」里配好端点地址和模型"
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
            // 自动模式的开关 / 分类器模型变了：模式列表和分类器都要重装
            || new.auto_mode != self.settings.auto_mode;
        let restart_acp = new.acp != self.settings.acp;
        let default = new.kernel();
        let default_changed = default != self.settings.kernel();
        // 存不存原文对已经跑着的网关立刻生效，不用重启内核
        claudesktop_builtin::request_bodies::set_enabled(new.gateway_bodies);
        self.settings = new;
        self.settings_open = false;
        if restart_builtin {
            self.restart_kernel(KernelChoice::Builtin, cx);
        }
        if restart_acp {
            self.restart_kernel(KernelChoice::Acp, cx);
        }
        // 自研内核的配置变了：顺手取一次模型列表，连不上马上告诉用户（不挡保存）
        if restart_builtin && default == KernelChoice::Builtin {
            self.start_fetch_models(Fetch::Verify, cx);
        }
        if default_changed {
            // 默认内核换了：还没对话的会话跟着换，有对话的留在原来的内核上
            for idx in 0..self.sessions.len() {
                self.rebind_session(idx, default);
            }
        }
        // 首次配置：启动时没起内核（见 `open_session`），现在把等着的会话接上
        if !self.kernels[default.index()].started
            && self.sessions.iter().any(|s| s.kernel == default && !s.has_user_message())
        {
            self.ensure_kernel(default);
            self.request_pending_sessions();
        }
        cx.notify();
    }

    // ---------- 档案编辑（表单内） ----------

    /// 表单里切到第 `i` 套档案。
    fn select_form_profile(&mut self, i: usize, cx: &mut Context<Self>) {
        if i < self.settings_form.profiles.len() {
            self.settings_form.active = i;
            self.settings_form.fetch_result = None;
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
        let blank = f.current().is_some_and(|(_, i)| {
            [&i.name, &i.base_url, &i.api_key, &i.model]
                .iter()
                .all(|input| value(input, cx).is_empty())
        });
        if blank {
            f.profiles[f.active] = p;
            f.inputs[f.active] = inputs;
        } else {
            f.inputs.push(inputs);
            f.profiles.push(p);
            f.active = f.profiles.len() - 1;
        }
        f.message = None;
        f.fetch_result = None;
        cx.notify();
    }

    /// 表单里切当前档案的协议。
    fn set_form_protocol(&mut self, protocol: Protocol, cx: &mut Context<Self>) {
        if let Some(p) = self.settings_form.profiles.get_mut(self.settings_form.active) {
            p.protocol = protocol;
            cx.notify();
        }
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
            let Some(inputs) = f.inputs.get(f.active) else {
                return;
            };
            ProviderConfig {
                protocol: f.profiles.get(f.active).map_or(Protocol::OpenAiCompat, |p| p.protocol),
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
                            if let Some(kernel) = &app.kernels[KernelChoice::Builtin.index()].kernel
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
        section("总是允许的操作", "按项目生效，删除立即生效", None, body).child(section(
            "网页搜索",
            "不填 key 时用 DuckDuckGo，可能被限流",
            None,
            setting_group([setting_row(
                "Brave API key",
                "api.search.brave.com 免费注册；也可以用环境变量 CLAUDEDESKTOP_BRAVE_KEY",
                div()
                    .h_flex()
                    .gap_2()
                    .child(
                        div()
                            .w(rems(24.))
                            .child(Input::new(&self.settings_form.brave_key).cleanable(true)),
                    )
                    .child(Self::text_button("brave-key-save", "保存", false).on_click(
                        cx.listener(|this, _, window, cx| {
                            let key = this.settings_form.brave_key.read(cx).value().to_string();
                            let res = claudesktop_builtin::websearch::set_brave_key(Some(&key));
                            this.settings_form.message = Some(match res {
                                Ok(()) => (false, "搜索后端已保存".into()),
                                Err(e) => (true, format!("保存失败：{e}").into()),
                            });
                            // 输入框回到已保存的值（密码态）
                            this.settings_form
                                .brave_key
                                .update(cx, |state, cx| state.set_value(key, window, cx));
                            cx.notify();
                        }),
                    )),
            )]),
        ))
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
            format!("已填入 Antigravity Tools 本地网关 {base_url}（需要它的反代服务在运行）")
                .into(),
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
            "记下每次请求的耗时、用量和重试（不记提示词和 Key），在首页「请求」页签查看；关掉就直连服务商",
            actions,
            Switch::new("usage-gateway").checked(f.usage_gateway).color(rgb(CORAL)).on_change(
                cx.listener(|this, checked: &bool, _, cx| {
                    this.settings_form.usage_gateway = *checked;
                    cx.notify();
                }),
            ),
        )
    }

    /// 网关存不存请求原文（随「保存」生效；网关关着时不可点）。
    fn bodies_row(&self, cx: &mut Context<Self>) -> Div {
        let f = &self.settings_form;
        setting_row(
            "保存请求原文",
            "另存每次请求的完整内容和返回（压缩，留 7 天），在「请求」页签点开查看；里面有对话和代码，只存本机",
            Switch::new("gateway-bodies")
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
            "用来估算费用，内置的是参考价",
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
        div()
            .rounded_lg()
            .border_1()
            .border_color(rgb(LINE))
            .bg(rgb(CARD))
            .px_1()
            .child(Input::new(state).appearance(false).bordered(false))
    }

    /// 表单和生效中的设置有没有不同（底栏据此提示、决定「保存」能不能点）。
    fn form_dirty(&self, cx: &App) -> bool {
        // 单价表写错了也算改动：点「保存」才看得到错在哪
        if crate::pricing::parse_text(&self.settings_form.prices.read(cx).value()).is_err() {
            return true;
        }
        let normalize = |mut s: Settings| {
            s.kernel = Some(s.kernel());
            // 打开设置页时补上的那套空档案不算改动
            let blank = ProviderProfile::default();
            s.builtin.profiles.retain(|p| *p != blank);
            s.builtin.active = s.builtin.active.min(s.builtin.profiles.len().saturating_sub(1));
            s
        };
        normalize(self.form_settings(cx)) != normalize(self.settings.clone())
    }

    /// 「内核与模型」：默认内核三选一，下面是选中那种内核的配置。
    fn render_kernel_tab(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let f = &self.settings_form;
        let mut kernels = choice_panel();
        for (kind, icon, title, desc) in KERNEL_CHOICES {
            kernels = kernels.child(
                choice_row(("kernel", kind.index()), icon, title, desc, f.kernel == kind, None)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.settings_form.kernel = kind;
                        this.settings_form.message = None;
                        cx.notify();
                    })),
            );
        }
        let body = div().v_flex().gap_8().child(section("默认内核", "", None, kernels));
        match f.kernel {
            KernelChoice::Builtin => body
                .child(self.render_providers(window, cx))
                .child(self.render_auto_mode(window, cx))
                .child(self.render_suggest(cx))
                .child(self.render_memory_options(cx))
                .child(section(
                    "运行",
                    "",
                    None,
                    setting_group([
                        setting_row(
                            "一轮最多几步",
                            "一轮里最多请求模型几次，默认 200；调大能跑更长的任务，也更费 token",
                            div().w(NUMBER_INPUT_W).child(Self::text_input(&f.max_steps)),
                        ),
                        self.gateway_row(cx),
                        self.bodies_row(cx),
                        self.prices_row(window, cx),
                    ]),
                )),
            KernelChoice::Acp => body.child(self.render_acp(cx)),
            KernelChoice::Demo => body,
        }
    }

    /// 「自动」模式：开关 + 分类器用哪个模型 + 信任边界。默认关，开了必须配齐。
    fn render_auto_mode(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let f = &self.settings_form;
        let enabled = f.auto_enabled;
        let follow = f.auto_follow;
        let switch = Switch::new("auto-mode-on").checked(enabled).color(rgb(CORAL)).on_change(
            cx.listener(|this, checked: &bool, _, cx| {
                this.settings_form.auto_enabled = *checked;
                cx.notify();
            }),
        );
        let follow_switch = Switch::new("auto-mode-follow")
            .checked(follow)
            .color(rgb(CORAL))
            .on_change(cx.listener(|this, checked: &bool, _, cx| {
                this.settings_form.auto_follow = *checked;
                cx.notify();
            }));
        let rows = setting_group([
            setting_row("启用「自动」", "新对话默认用「自动」", switch),
            setting_row("分类器跟随当前模型", "关掉可另填小模型，判得更快", follow_switch),
            setting_row(
                "分类器模型",
                "同一端点的模型名，留空用会话的",
                div()
                    .w(rems(20.))
                    .child(Input::new(&f.auto_model).cleanable(true))
                    .when(follow, |d| d.opacity(0.5)),
            ),
            setting_row(
                "信任边界",
                "哪些仓库、域名算内部；留空只认工作目录",
                div().w(rems(32.)).child(Textarea::new(&f.auto_environment).appearance(false)),
            ),
        ]);
        // 没配好时把话说在前面（保存时也会拦）
        let unconfigured = enabled && !self.settings.builtin.is_configured();
        let body = div()
            .v_flex()
            .gap_3()
            .child(rows)
            .when(unconfigured, |d| d.child(note(IconAll::Info, "先在「服务商」里配好端点和模型")))
            .child(self.disclosure(
                "auto-mode-rules",
                "它拦什么、放什么",
                auto_mode_docs(),
                window,
                cx,
            ));
        section(
            "自动模式",
            "由分类器代替你逐项审批；它看不到助手的解释和工具结果，只按动作本身判，拦下时把理由交给 agent",
            None,
            body,
        )
    }

    /// 输入补全：两个独立的开关 + 用哪个模型。每次都是一次请求，所以默认关、推荐另填小模型。
    fn render_suggest(&self, cx: &mut Context<Self>) -> Div {
        let f = &self.settings_form;
        let typing = Switch::new("suggest-on")
            .checked(f.suggest_enabled)
            .color(rgb(CORAL))
            .on_change(cx.listener(|this, checked: &bool, _, cx| {
                this.settings_form.suggest_enabled = *checked;
                cx.notify();
            }));
        let next = Switch::new("next-suggestion-on")
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
        section(
            "输入补全",
            "你打字时猜你这句的下一半；每轮回复结束后猜你接下来会发的那一句",
            None,
            setting_group([
                setting_row(
                    "打字时续写",
                    "停顿一下发一次请求，灰字画在光标后面，按 Tab 接上",
                    typing,
                ),
                setting_row(
                    "一轮结束后猜下一句",
                    "回复结束时发一次请求，猜出的那句话用淡色画在空输入框里，按 Tab 填入；\
                     输入框里已有草稿时不猜",
                    next,
                ),
                setting_row(
                    "补全模型",
                    "同一端点的模型名，建议填便宜、不带思考的小模型；留空用会话的",
                    div().w(rems(20.)).child(Input::new(&f.suggest_model).cleanable(true)),
                ),
            ]),
        )
    }

    /// 记忆：一轮结束后要不要让模型提炼「候选」（只进设置页待确认，收下才落盘）。
    fn render_memory_options(&self, cx: &mut Context<Self>) -> Div {
        let f = &self.settings_form;
        let suggest = Switch::new("memory-suggest")
            .checked(f.memory_suggest)
            .color(rgb(CORAL))
            .on_change(cx.listener(|this, checked: &bool, _, cx| {
                this.settings_form.memory_suggest = *checked;
                cx.notify();
            }));
        section(
            "记忆",
            "跨会话记住偏好和约定：每轮带上相关的几条，置顶的永远带上；在设置页「记忆」里看、改、删",
            None,
            setting_group([setting_row(
                "回合结束后提炼记忆",
                "每轮结束多一次请求，提炼出值得记下的话放进「记忆」页顶部等你收下（默认关，关着就是它自己觉得有用时才记）",
                suggest,
            )]),
        )
    }

    /// 自研内核的服务商：档案列表（选中的那套在用）+「添加」菜单，下面编辑选中的那套。
    fn render_providers(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let f = &self.settings_form;
        let app = cx.entity();
        let add = menu_popover(
            "profile-add-menu",
            Anchor::TopRight,
            rems(20.),
            &app,
            &self.menu_focus,
            MenuChip::new(IconAll::Plus, "添加").filled(),
            add_profile_menu_items,
        )
        .into_any_element();

        let mut list = choice_panel();
        for (i, inputs) in f.inputs.iter().enumerate() {
            let active = i == f.active;
            let name = value(&inputs.name, cx);
            let base_url = value(&inputs.base_url, cx);
            let model = value(&inputs.model, cx);
            let title = if name.is_empty() { format!("服务商 {}", i + 1) } else { name };
            let desc = match (host_of(&base_url), model.is_empty()) {
                ("", _) => "还没填端点地址".to_string(),
                (host, true) => host.to_string(),
                (host, false) => format!("{host} · {model}"),
            };
            // 删除只给选中的那行（先选中再删，不会误点）
            let remove = active.then(|| {
                Self::quiet_button(("profile-remove", i), IconAll::Trash, Surface::Paper, cx)
                    .small()
                    .tooltip("删除这套")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.remove_form_profile(i, cx);
                    }))
                    .into_any_element()
            });
            list = list.child(
                choice_row(("profile", i), IconAll::Server, title, desc, active, remove)
                    .on_click(cx.listener(move |this, _, _, cx| this.select_form_profile(i, cx))),
            );
        }

        let mut body = div().v_flex().gap_4();
        // 按已保存的设置判断（不看正在填的输入框），免得填第一个字时说明忽然消失、整页往上跳
        if f.inputs.len() <= 1 && !self.settings.builtin.is_configured() {
            body = body.child(how_to(
                "三步接上模型",
                &[
                    "点右上角「添加」选服务商，没有的选「自定义」",
                    "填 API Key（本机 Ollama 不用）",
                    "「获取列表」挑个模型，再点「保存并应用」",
                ],
            ));
        }
        if !f.inputs.is_empty() {
            body = body.child(list);
        }
        if let Some((p, inputs)) = f.current() {
            body = body.child(self.render_profile_editor(p, inputs, window, cx));
        }
        section("服务商", "选中的那套在用", Some(add), body)
    }

    /// 选中那套档案的表单：名称、协议、端点、Key、模型。
    fn render_profile_editor(
        &self,
        p: &ProviderProfile,
        inputs: &ProfileInputs,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let app = cx.entity();
        let mut model_row = div()
            .h_flex()
            .gap_2()
            .child(div().flex_1().min_w_0().child(Self::text_input(&inputs.model)));
        if !p.models.is_empty() {
            model_row = model_row.child(menu_popover(
                "model-pick-menu",
                Anchor::TopRight,
                rems(20.),
                &app,
                &self.menu_focus,
                MenuChip::new(IconAll::List, format!("{} 个可选", p.models.len())),
                model_menu_items,
            ));
        }
        model_row = model_row.child(
            Self::text_button("fetch-models", "获取列表", false)
                .loading(self.fetching_models)
                .on_click(cx.listener(|this, _, _, cx| this.fetch_models(true, cx))),
        );
        // 上次「获取列表」的结果代替说明：失败时说清改哪一项，下面带服务商的原文
        let fetch_result = self.settings_form.fetch_result.clone();
        let model_hint =
            (fetch_result.is_none() && p.models.is_empty()).then(|| "可以直接填名字".into());
        let model_row = div().v_flex().gap_1p5().child(model_row).when_some(
            fetch_result,
            |d, (error, text, raw)| {
                d.child(
                    div()
                        .text_size(TEXT_SMALL)
                        .text_color(rgb(if error { RED } else { GREEN }))
                        .child(text),
                )
                .when_some(raw, |d, raw| {
                    d.child(
                        div()
                            .text_size(TEXT_META)
                            .text_color(rgb(INK_SOFT))
                            .line_clamp(2)
                            .child(raw),
                    )
                })
            },
        );
        let mut endpoint = div().v_flex().gap_1p5().child(Self::text_input(&inputs.base_url));
        if antigravity_gateway().is_some() {
            endpoint = endpoint.child(
                div().h_flex().child(
                    Self::quiet_button("import-antigravity", IconAll::Download, Surface::Paper, cx)
                        .xsmall()
                        .label("从 Antigravity Tools 导入端点和 Key")
                        .px_2()
                        .ml(rems(-0.5))
                        .on_click(
                            cx.listener(|this, _, window, cx| this.import_antigravity(window, cx)),
                        ),
                ),
            );
        }
        div()
            .v_flex()
            .gap_4()
            .pt_2()
            .child(form_field("名称", None, Self::text_input(&inputs.name)))
            .child(form_field(
                "协议",
                Some("大多数服务选 OpenAI 兼容；Anthropic 官方端点选原生；OpenAI 的 Responses API 选 Responses".into()),
                div().h_flex().child(seg_control(
                    "protocol",
                    [
                        (Protocol::OpenAiCompat, "OpenAI 兼容"),
                        (Protocol::Anthropic, "Anthropic 原生"),
                        (Protocol::Responses, "Responses"),
                    ],
                    p.protocol,
                    |this, protocol, _, cx| this.set_form_protocol(protocol, cx),
                    cx,
                )),
            ))
            .child(form_field(
                "端点地址",
                Some("不带 /chat/completions 或 /messages".into()),
                endpoint,
            ))
            .child(form_field(
                "API Key",
                None,
                div().rounded_lg().border_1().border_color(rgb(LINE)).bg(rgb(CARD)).px_1().child(
                    Input::new(&inputs.api_key).appearance(false).bordered(false).mask_toggle(),
                ),
            ))
            .child(form_field("模型", model_hint, model_row))
            .when(p.protocol == Protocol::Anthropic, |d| {
                d.child(self.render_request_extras(p, inputs, cx))
            })
    }

    /// 档案里只有 Anthropic 原生协议认的两项：思考预算 + 服务端工具。
    /// 协议不是 Anthropic 时整块不显示（填了也不生效，免得误会），见 `render_profile_editor`。
    fn render_request_extras(
        &self,
        p: &ProviderProfile,
        inputs: &ProfileInputs,
        cx: &mut Context<Self>,
    ) -> Div {
        let budget = form_field(
            "思考预算",
            Some("最小 1024，留空不开启".into()),
            div().w(NUMBER_INPUT_W).child(Self::text_input(&inputs.thinking_budget)),
        );
        // 服务端工具：一排开关（沙色面板里的行，和 MCP / 插件那两页一个样式）
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
                                    this.settings_form.profiles.get_mut(this.settings_form.active)
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
        div().v_flex().gap_4().child(budget).child(form_field(
            "服务端工具",
            Some("在 Anthropic 服务器上执行，按次计费".into()),
            tools,
        ))
    }

    /// ACP agent：启动命令（带常用命令一键填入）和要不要经用量网关。
    fn render_acp(&self, cx: &mut Context<Self>) -> Div {
        let f = &self.settings_form;
        let mut quick = div().h_flex().items_center().gap_1().ml(rems(-0.5));
        for (i, (label, command)) in ACP_PRESETS.into_iter().enumerate() {
            quick = quick.child(
                Self::quiet_button(("acp-preset", i), IconAll::SquareTerminal, Surface::Paper, cx)
                    .xsmall()
                    .label(format!("填入 {label}"))
                    .px_2()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let command = acp_command(command);
                        this.settings_form
                            .acp_command
                            .update(cx, |s, cx| s.set_value(command, window, cx));
                        cx.notify();
                    })),
            );
        }
        let command = div().v_flex().gap_1p5().child(Self::text_input(&f.acp_command)).child(quick);
        let gateway_desc = if self.settings.usage_gateway {
            "把 agent 的请求也记进「请求」页签"
        } else {
            "先在自研内核的「运行」里打开用量网关"
        };
        section(
            "ACP agent",
            "登录和模型在 agent 那边设置",
            None,
            div()
                .v_flex()
                .gap_4()
                .child(form_field(
                    "启动命令",
                    Some("需要 Node.js；留空用演示 agent".into()),
                    command,
                ))
                .child(setting_group([setting_row(
                    "经过用量网关",
                    gateway_desc,
                    Switch::new("acp-gateway").checked(f.acp_gateway).color(rgb(CORAL)).on_change(
                        cx.listener(|this, checked: &bool, _, cx| {
                            this.settings_form.acp_gateway = *checked;
                            cx.notify();
                        }),
                    ),
                )])),
        )
    }

    /// 「内核与模型」底部固定的保存栏：左边状态 / 提示，右边取消和保存。
    fn render_save_bar(&self, cx: &mut Context<Self>) -> Div {
        let dirty = self.form_dirty(cx);
        let status = match &self.settings_form.message {
            Some((error, text)) => {
                div().text_color(rgb(if *error { RED } else { GREEN })).child(text.clone())
            }
            None if dirty => div().text_color(rgb(INK)).child("有未保存的改动"),
            None => div().text_color(rgb(INK_SOFT)).child("点「保存并应用」后生效"),
        };
        div()
            .h_flex()
            .items_center()
            .gap_2()
            .flex_none()
            .px_8()
            .py_3()
            .border_t_1()
            .border_color(rgb(LINE))
            .child(div().flex_1().min_w_0().text_size(TEXT_SMALL).child(status))
            .child(Self::text_button("settings-cancel", "取消", false).on_click(cx.listener(
                |this, _, _, cx| {
                    this.settings_open = false;
                    cx.notify();
                },
            )))
            .child(
                Self::text_button("settings-save", "保存并应用", true)
                    .disabled(!dirty)
                    .tooltip("只重启配置变了的内核；它上面进行中的对话会中断，下次发消息时自动续接")
                    .on_click(cx.listener(|this, _, _, cx| this.save_settings(cx))),
            )
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
        // 左侧分类导航
        let mut nav = div().v_flex().w(SETTINGS_NAV_W).flex_none().px_3().gap_0p5();
        for (i, (icon, label)) in SETTINGS_TABS.into_iter().enumerate() {
            if i == super::usage_view::USAGE_TAB {
                continue;
            }
            let active = i == f.tab;
            nav = nav.child(
                div()
                    .id(("settings-tab", i))
                    .h_flex()
                    .items_center()
                    .gap_2p5()
                    .px_3()
                    .py_2()
                    .rounded_lg()
                    .cursor_pointer()
                    .text_size(TEXT_BODY)
                    .text_color(rgb(INK))
                    .when(active, |d| d.bg(rgb(SELECTED)))
                    .when(!active, |d| d.hover(|d| d.bg(rgb(HOVER))))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if this.settings_form.tab != i {
                            // 离开「项目」时把还在输入框里的改动写掉：点分类不一定让输入框失焦，
                            // 回来时会重读文件
                            if this.settings_form.tab == super::project_view::PROJECT_TAB {
                                this.apply_project(cx);
                            }
                            this.settings_form.tab = i;
                            this.settings_form.message = None;
                            // 「记忆」每次进来都重读文件（内核 / 手改都可能动过它）
                            if i == super::memory_view::MEMORY_TAB {
                                this.reload_memory_view();
                            }
                            // 「项目」同理：重读项目根的配置文件
                            if i == super::project_view::PROJECT_TAB {
                                this.reload_project_view(window, cx);
                            }
                            cx.notify();
                        }
                    }))
                    .child(Icon::new(icon).size_4().text_color(rgb(INK_SOFT)))
                    .child(label),
            );
        }
        let kernel_tab = f.tab == KERNEL_TAB;
        // 内容列定宽，滚动时不用每帧把整页来回排好几遍（见 `MeasuredWidth`）
        let content_w = f.content_w.get();
        let width_probe = f.content_w.probe();
        let tab_content: Div = match f.tab {
            super::project_view::PROJECT_TAB => self.render_project(cx),
            KERNEL_TAB => self.render_kernel_tab(window, cx),
            3 => self.render_rules(cx),
            4 => self.render_mcp(window, cx),
            PLUGINS_TAB => self.render_plugins(cx),
            super::memory_view::MEMORY_TAB => self.render_memory(window, cx),
            _ => self.render_general(cx),
        };

        div()
            .id("settings")
            // 焦点锚：设置页里点空白处也留在 ChatApp 键上下文里（Esc 关设置页）
            .track_focus(&self.settings_focus)
            .flex_1()
            .min_h_0()
            .v_flex()
            .child(
                div()
                    .v_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .max_w(SETTINGS_NAV_W + FORM_W)
                    .mx_auto()
                    // 标题行：「设置」与导航项文字左对齐，关闭按钮在右
                    .child(
                        div()
                            .h_flex()
                            .items_center()
                            .flex_none()
                            .px_6()
                            .pt_6()
                            .pb_4()
                            .child(
                                div()
                                    .flex_1()
                                    .text_size(TEXT_HEADING)
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(rgb(INK))
                                    .child("设置"),
                            )
                            .child(
                                Self::quiet_button(
                                    "settings-close",
                                    IconAll::X,
                                    Surface::Paper,
                                    cx,
                                )
                                .tooltip("关闭设置")
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.settings_open = false;
                                        cx.notify();
                                    },
                                )),
                            ),
                    )
                    // 导航固定，只滚右侧内容；「内核与模型」的保存栏固定在右侧底部
                    .child(
                        div().flex().flex_1().min_h_0().child(nav).child(
                            div()
                                .v_flex()
                                .flex_1()
                                .min_w_0()
                                .relative()
                                .child(
                                    div()
                                        .id("settings-scroll")
                                        .track_scroll(&self.settings_scroll)
                                        .flex_1()
                                        .min_h_0()
                                        .overflow_y_scroll()
                                        // pt_1：分区标题与导航第一项的文字对齐
                                        .child(
                                            div()
                                                .v_flex()
                                                .px_8()
                                                .pt_1()
                                                .pb_10()
                                                .when_some(content_w, |d, w| d.w(w))
                                                .child(tab_content),
                                        ),
                                )
                                .child(width_probe)
                                .when(kernel_tab, |d| d.child(self.render_save_bar(cx))),
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

/// 「内核与模型」里默认内核的三种选择。
const KERNEL_CHOICES: [(KernelChoice, IconAll, &str, &str); 3] = [
    (
        KernelChoice::Builtin,
        IconAll::Cpu,
        "自研内核",
        "填 API Key 直连 DeepSeek、OpenAI、Ollama 等",
    ),
    (KernelChoice::Acp, IconAll::SquareTerminal, "ACP agent", "接 Claude Code 这类外部 agent"),
    (KernelChoice::Demo, IconAll::Play, "演示回放", "不连模型，播放界面演示"),
];

/// ACP 启动命令的常用项：(名称, 命令)；Windows 上前面补 `cmd /c`（见 [`acp_command`]）。
const ACP_PRESETS: [(&str, &str); 2] =
    [("Claude Code", "npx -y @agentclientprotocol/claude-agent-acp"), ("pi", "npx -y pi-acp")];

/// npx 在 Windows 上是 .cmd，要经 `cmd /c` 才起得来。
fn acp_command(command: &str) -> String {
    if cfg!(windows) { format!("cmd /c {command}") } else { command.to_string() }
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

/// 「N 个可选」菜单：获取到的模型列表，点了填进模型输入框。
fn model_menu_items(
    app: &ChatApp,
    handle: &Entity<ChatApp>,
    popover: &Entity<PopoverState>,
) -> MenuList {
    let f = &app.settings_form;
    let models = f.profiles.get(f.active).map(|p| p.models.clone()).unwrap_or_default();
    let mut menu = MenuList::new("model-pick-list", handle, popover).scroll(rems(22.5));
    for (i, m) in models.into_iter().enumerate() {
        let pick = m.clone();
        menu = menu.pick_in(
            menu_item(("model-pick", i), IconAll::Box, m, "", false),
            move |this, window, cx| {
                let i = this.settings_form.active;
                if let Some(inputs) = this.settings_form.inputs.get(i) {
                    inputs.model.update(cx, |s, cx| s.set_value(pick.clone(), window, cx));
                }
                cx.notify();
            },
        );
    }
    menu
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
    let mut menu = MenuList::new("theme-list", handle, popover);
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

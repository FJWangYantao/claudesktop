//! 设置页（内核 / provider 档案 / 模型）与输入框旁的模型下拉。

use super::ChatApp;
use super::keys::{ZOOM_IN_KEY, ZOOM_OUT_KEY};
use super::menu::{MenuChip, MenuList, menu_item, menu_popover};
use super::widgets::Surface;
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

/// 设置页左侧的分类（下标即 `SettingsForm::tab`）。
const SETTINGS_TABS: [(IconAll, &str); 5] = [
    (IconAll::Settings, "通用"),
    (IconAll::Cpu, "内核与模型"),
    (IconAll::ShieldCheck, "权限"),
    (IconAll::Plug, "MCP"),
    (IconAll::Blocks, "插件"),
];

/// 设置页「插件」分类的下标（侧栏的「插件」入口直接打开它）
pub(super) const PLUGINS_TAB: usize = 4;

/// 设置项的一行：左边标题 + 灰色说明，右边控件。
fn setting_row(title: &'static str, desc: impl IntoElement, control: impl IntoElement) -> Div {
    div()
        .h_flex()
        .items_center()
        .gap_4()
        .py_4()
        .child(
            div()
                .v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(
                    div()
                        .text_size(TEXT_BODY)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(rgb(INK))
                        .child(title),
                )
                .child(div().text_size(TEXT_SMALL).text_color(rgb(INK_SOFT)).child(desc)),
        )
        .child(div().flex_none().child(control))
}

/// 一组设置行，行与行之间细分隔线。
fn setting_group(rows: impl IntoIterator<Item = Div>) -> Div {
    let mut group = div().v_flex();
    for (i, row) in rows.into_iter().enumerate() {
        group = group.child(
            row.when(i == 0, |d| d.pt_0()).when(i > 0, |d| d.border_t_1().border_color(rgb(LINE))),
        );
    }
    group
}

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
            base_url: make(p.base_url.clone(), "如 https://api.deepseek.com/v1", false, window, cx),
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
    /// 设置页当前所在的 tab（0 通用 / 1 内核与模型 / 2 权限 / 3 MCP）
    pub(super) tab: usize,
    acp_command: Entity<InputState>,
    /// 一轮最多几步（自研内核的工具循环上限）
    max_steps: Entity<InputState>,
    /// 用量网关开关（保存才生效，会重启自研内核）
    usage_gateway: bool,
    /// ACP agent 的请求也经网关（保存才生效，会重启 ACP 内核）
    acp_gateway: bool,
    /// 点了「清空日志」，等再点一次确认
    confirm_clear_gateway: bool,
    /// 模型单价表（一行一个模型，格式见 `pricing.rs`）
    prices: Entity<TextareaState>,
    /// (是否错误, 文本)
    message: Option<(bool, SharedString)>,
    /// 已保存的「总是允许」规则：(项目目录, 规则)；删除即时生效，不等「保存」
    rules: Vec<(String, Vec<String>)>,
}

impl SettingsForm {
    pub fn new(window: &mut Window, cx: &mut Context<ChatApp>) -> Self {
        let acp_command =
            cx.new(|cx| InputState::new(window, cx).placeholder("留空 = 内置 mock agent"));
        let max_steps = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(claudesktop_builtin::DEFAULT_MAX_STEPS.to_string())
        });
        let prices = cx.new(|cx| TextareaState::new(window, cx).auto_grow(3, 12));
        Self {
            kernel: KernelChoice::default(),
            profiles: Vec::new(),
            active: 0,
            inputs: Vec::new(),
            tab: 0,
            acp_command,
            max_steps,
            usage_gateway: true,
            acp_gateway: false,
            confirm_clear_gateway: false,
            prices,
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

impl ChatApp {
    pub(crate) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // 记住上次看的 tab（打开时不动它），要指定 tab 的走 `open_settings_at`
        let tab = self.settings_form.tab;
        self.open_settings_at(tab, window, cx);
    }

    /// 打开设置页并切到第 `tab` 个分类（0 通用 / 1 内核与模型 / 2 权限 / 3 MCP / 4 插件）。
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
        self.settings_form.usage_gateway = s.usage_gateway;
        self.settings_form.acp_gateway = s.acp.gateway;
        self.settings_form.confirm_clear_gateway = false;
        let prices = crate::pricing::to_text(&crate::pricing::Table {
            prices: s.prices(),
            peaks: s.price_peaks(),
        });
        self.settings_form.prices.update(cx, |state, cx| state.set_value(prices, window, cx));
        self.settings_form.rules = PermissionRules::load(default_rules_path()).list();
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

    /// 「通用」分类：外观、字号、字体、侧栏内核状态、系统通知（都即时生效，不走「保存并应用」）。
    fn render_general(&self, cx: &mut Context<Self>) -> Div {
        let current = self.settings.appearance;
        let mut segs = div().h_flex().gap_0p5().p_0p5().rounded_lg().bg(rgb(SAND));
        for (i, a) in Appearance::ALL.into_iter().enumerate() {
            segs = segs.child(
                div()
                    .id(("appearance", i))
                    .cursor_pointer()
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.set_appearance(a, window, cx)),
                    )
                    .child(Self::seg(a.label(), a == current)),
            );
        }
        let motion = self.settings.motion;
        let mut motion_segs = div().h_flex().gap_0p5().p_0p5().rounded_lg().bg(rgb(SAND));
        for (i, m) in MotionPref::ALL.into_iter().enumerate() {
            motion_segs = motion_segs.child(
                div()
                    .id(("motion", i))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| this.set_motion_pref(m, cx)))
                    .child(Self::seg(m.label(), m == motion)),
            );
        }
        let motion_desc = if motion == MotionPref::System && self.system_reduce_motion {
            "展开收起、转圈等过渡；Windows 里已关闭"
        } else {
            "展开收起、转圈等过渡；可跟随 Windows 设置"
        };
        setting_group([
            setting_row("外观", "跟随系统，或固定用浅色 / 深色；立即生效", segs),
            setting_row("动画", motion_desc, motion_segs),
            self.font_scale_row(),
            self.font_family_row(cx),
            setting_row(
                "侧栏显示内核状态",
                "在侧栏底部列出每个已启动内核的连接状态；关掉不影响内核运行",
                Switch::new("sidebar-kernel-status")
                    .checked(self.settings.sidebar_kernel_status)
                    .color(rgb(CORAL))
                    .on_change(cx.listener(|this, checked, _, cx| {
                        this.set_sidebar_kernel_status(*checked, cx)
                    })),
            ),
            setting_row(
                "系统通知",
                "窗口在后台时，回复完成、等授权或出错会弹通知；点通知回到该会话",
                Switch::new("notifications")
                    .checked(self.settings.notifications)
                    .color(rgb(CORAL))
                    .on_change(cx.listener(|this, checked, _, cx| {
                        let on = *checked;
                        this.save_ui_pref(move |s| s.notifications = on, cx);
                        cx.notify();
                    })),
            ),
        ])
    }

    /// 字号滑杆：右边带当前百分比。事件订阅在 `ChatApp::new`（`SliderState` 自己 emit），
    /// 拖动与松手都即时生效。
    fn font_scale_row(&self) -> Div {
        let percent = (self.settings.font_scale * 100.).round();
        let slider = div()
            .h_flex()
            .items_center()
            .gap_3()
            .w(px(240.))
            .child(div().flex_1().min_w_0().child(Slider::new(&self.font_slider)))
            .child(
                // 宽度用 rem：字号放大后「140%」也不会折成两行
                div()
                    .w(rems(3.))
                    .flex_shrink_0()
                    .whitespace_nowrap()
                    .text_right()
                    .font_features(tabular_nums())
                    .child(format!("{percent:.0}%")),
            );
        let desc =
            div().v_flex().gap_1p5().child("整界面等比缩放（文字、间距、圆角一起变）").child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_1()
                    .child(Self::kbd(ZOOM_IN_KEY))
                    .child("放大")
                    .child(div().w_1())
                    .child(Self::kbd(ZOOM_OUT_KEY))
                    .child("缩小"),
            );
        setting_row("界面字号", desc, slider)
    }

    /// 字体族下拉：内嵌的 Inter + 系统已装字体（下拉里可搜索）。
    fn font_family_row(&self, cx: &mut Context<Self>) -> Div {
        let app = cx.entity();
        let label: SharedString =
            self.settings.font_family_or_default().unwrap_or_else(|| "Inter（内嵌）".into());
        setting_row(
            "界面字体",
            "界面文字用哪套字体；代码块始终用等宽的 JetBrains Mono",
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
        }
        s.builtin.profiles = profiles;
        s.builtin.active = f.active.min(s.builtin.profiles.len().saturating_sub(1));
        s.acp.command = value(&f.acp_command, cx);
        s.acp.gateway = f.acp_gateway;
        s.max_steps = parse_max_steps(&value(&f.max_steps, cx));
        s.usage_gateway = f.usage_gateway;
        // 单价表写错了留着原来的（`save_settings` 先报错，不会走到这里）；和内置的一样就不存
        if let Ok(table) = crate::pricing::parse_text(&f.prices.read(cx).value()) {
            let defaults = crate::pricing::Table::defaults();
            s.model_prices = (table.prices != defaults.prices).then_some(table.prices);
            s.price_peaks = (table.peaks != defaults.peaks).then_some(table.peaks);
        }
        s
    }

    /// 首页配置引导选了一种接法：打开「内核与模型」并选好那种内核；
    /// ACP 命令还空着时填上 Claude Code 的适配器。
    pub(crate) fn start_setup(
        &mut self,
        kind: KernelChoice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_settings_at(1, window, cx);
        self.settings_form.kernel = kind;
        if kind == KernelChoice::Acp
            && self.settings_form.acp_command.read(cx).value().trim().is_empty()
        {
            let command = if cfg!(windows) {
                "cmd /c npx -y @agentclientprotocol/claude-agent-acp"
            } else {
                "npx -y @agentclientprotocol/claude-agent-acp"
            };
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
        if new.kernel() == KernelChoice::Builtin
            && let Err(err) = new.builtin_config(self.cwd.clone())
        {
            self.settings_form.message = Some((true, err.to_string().into()));
            cx.notify();
            return;
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
            || new.usage_gateway != self.settings.usage_gateway;
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
        self.start_fetch_models(from_form, false, cx);
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
        self.start_fetch_models(false, true, cx);
    }

    /// `quiet`：后台顺手取的，失败不往会话里写提示。
    fn start_fetch_models(&mut self, from_form: bool, quiet: bool, cx: &mut Context<Self>) {
        if self.fetching_models {
            return;
        }
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
            if !quiet {
                self.settings_form.message = Some((true, "先填端点地址".into()));
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
                            app.settings_form.message =
                                Some((false, format!("获取到 {n} 个模型").into()));
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
                        let text: SharedString = format!("获取模型失败：{err}").into();
                        if from_form {
                            app.settings_form.message = Some((true, text));
                        } else if !quiet {
                            app.toast(Notification::error(text), cx);
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
            .child(
                div()
                    .text_size(TEXT_BODY)
                    .font_weight(FontWeight::MEDIUM)
                    .child("已保存的权限规则"),
            )
            .child(div().text_size(TEXT_SMALL).text_color(rgb(INK_SOFT)).child(
                "自研内核里在权限卡片上选「总是允许」时记下的，按项目生效；删除后立即生效。",
            ));
        let rules = &self.settings_form.rules;
        if rules.is_empty() {
            return section.child(
                div().py_2().text_size(TEXT_SMALL).text_color(rgb(INK_SOFT)).child("还没有规则"),
            );
        }
        let mut n = 0usize;
        for (project, list) in rules {
            let mut card =
                div().v_flex().rounded_lg().border_1().border_color(rgb(LINE)).bg(rgb(CARD)).child(
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
                            Self::quiet_button(("rule-remove", n), IconAll::X, Surface::Paper, cx)
                                .small()
                                .tooltip("删除这条规则")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.remove_rule(p.clone(), r.clone(), cx)
                                })),
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

    /// 用量网关：开关（随「保存」生效）、打开日志目录、清空日志（再点一次确认）。
    fn gateway_field(&self, cx: &mut Context<Self>) -> Div {
        let f = &self.settings_form;
        let confirm = f.confirm_clear_gateway;
        let row = div()
            .h_flex()
            .items_center()
            .gap_2()
            .child(
                Switch::new("usage-gateway").checked(f.usage_gateway).color(rgb(CORAL)).on_change(
                    cx.listener(|this, checked: &bool, _, cx| {
                        this.settings_form.usage_gateway = *checked;
                        cx.notify();
                    }),
                ),
            )
            .child(div().flex_1())
            .child(
                Self::text_button("gateway-open", "打开日志目录", false)
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| this.open_gateway_dir(cx))),
            )
            .child(
                Self::text_button(
                    "gateway-clear",
                    if confirm { "确定清空？" } else { "清空日志" },
                    false,
                )
                .small()
                .when(confirm, |b| b.with_variant(ButtonVariant::Danger))
                .on_click(cx.listener(|this, _, _, cx| this.clear_gateway_logs(cx))),
            );
        Self::field(
            "用量网关",
            Some(
                "自研内核的每个请求经本地网关转发，记下耗时、用量、重试和停止（不记提示词和 key），\
                 在首页用量的「请求」页签里看；关掉就直连服务商",
            ),
            row,
        )
    }

    /// 模型单价表：多行文本框 + 「恢复默认」。
    fn prices_field(&self, cx: &mut Context<Self>) -> Div {
        let editor = div()
            .rounded_md()
            .border_1()
            .border_color(rgb(LINE))
            .bg(rgb(CARD))
            .px_1()
            .font_family(MONO)
            .text_size(TEXT_SMALL)
            .child(Textarea::new(&self.settings_form.prices).appearance(false).bordered(false));
        let row = div().v_flex().gap_1p5().child(editor).child(
            div().h_flex().child(div().flex_1()).child(
                Self::text_button("prices-reset", "恢复默认", false).small().on_click(cx.listener(
                    |this, _, window, cx| {
                        let text = crate::pricing::to_text(&crate::pricing::Table::defaults());
                        this.settings_form
                            .prices
                            .update(cx, |state, cx| state.set_value(text, window, cx));
                        cx.notify();
                    },
                )),
            ),
        );
        Self::field(
            "模型单价",
            Some(
                "估算首页「请求」页签的费用用。一行一个模型：名称 输入 输出 缓存命中，\
                 按每百万 token，数字前写 $ 或 ¥；名称按包含匹配、取最长的一条。内置的是参考价，以服务商为准。\
                 分峰谷价的模型在后面写 x2（写的是谷价）；峰时段单独一行：@peak 1-5 09:00-12:00 +08:00",
            ),
            row,
        )
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
        let choice =
            |id: &'static str, kind: KernelChoice, title: &'static str, desc: &'static str| {
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

        let mut body = div().v_flex().gap_5().child(
            div()
                .v_flex()
                .gap_2()
                .child(div().text_size(TEXT_BODY).font_weight(FontWeight::MEDIUM).child("默认内核"))
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
                                Self::quiet_button(
                                    ("profile-remove", i),
                                    IconAll::X,
                                    Surface::Sand,
                                    cx,
                                )
                                .xsmall()
                                .tooltip("删除这套档案")
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.remove_form_profile(i, cx);
                                    },
                                )),
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

                let mut builtin_body = div().v_flex().gap_4().child(Self::field(
                    "配置档案",
                    Some("可以存多套 provider 配置，输入框旁的菜单可快速切换"),
                    profile_row,
                ));
                if !f.profiles.is_empty()
                    && let Some((p, inputs)) = f.current()
                {
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
                    let mut model_row = div()
                        .h_flex()
                        .gap_2()
                        .child(div().flex_1().child(Self::text_input(&inputs.model)))
                        .child(
                            Self::text_button("fetch-models", "获取模型列表", false)
                                .loading(self.fetching_models)
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.fetch_models(true, cx)),
                                ),
                        );
                    if antigravity_gateway().is_some() {
                        model_row = model_row.child(
                            Self::text_button(
                                "import-antigravity",
                                "从 Antigravity Tools 导入",
                                false,
                            )
                            .on_click(cx.listener(
                                |this, _, window, cx| this.import_antigravity(window, cx),
                            )),
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
                                        inputs
                                            .model
                                            .update(cx, |s, cx| s.set_value(pick, window, cx));
                                    }
                                    cx.notify();
                                }))
                                .child(m.clone()),
                        );
                    }
                    builtin_body = builtin_body
                        .child(Self::field(
                            "常用服务预设",
                            Some("点击追加一套已填好端点的档案，再补上自己的 key"),
                            presets,
                        ))
                        .child(Self::field("名称", None, Self::text_input(&inputs.name)))
                        .child(Self::field(
                            "协议",
                            Some("绝大多数服务选 OpenAI 兼容；Anthropic 官方端点用原生协议"),
                            div()
                                .h_flex()
                                .gap_1p5()
                                .child(proto(
                                    "protocol-openai",
                                    "OpenAI 兼容",
                                    Protocol::OpenAiCompat,
                                ))
                                .child(proto(
                                    "protocol-anthropic",
                                    "Anthropic 原生",
                                    Protocol::Anthropic,
                                )),
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
                // 内核级配置，不属于某一套档案
                builtin_body = builtin_body.child(Self::field(
                    "一轮最多几步",
                    Some(
                        "一轮里最多请求模型几次（工具循环上限，默认 200）；\
                         调大能跑更长的任务，耗 token 也更多",
                    ),
                    div().w(NUMBER_INPUT_W).child(Self::text_input(&f.max_steps)),
                ));
                builtin_body =
                    builtin_body.child(self.gateway_field(cx)).child(self.prices_field(cx));
                body = body.child(builtin_body);
            }
            KernelChoice::Acp => {
                body = body
                    .child(Self::field(
                        "启动命令",
                        Some("例：cmd /c npx -y @agentclientprotocol/claude-agent-acp　或　cmd /c npx -y pi-acp"),
                        Self::text_input(&f.acp_command),
                    ))
                    .child(Self::field(
                        "经过用量网关",
                        Some(if self.settings.usage_gateway {
                            "启动 agent 时把它的 ANTHROPIC_BASE_URL / OPENAI_BASE_URL 指到本地网关，它的请求也记进首页「请求」页签\
                             （Claude Code 能对上会话和轮次）；agent 不认这两个变量时不起作用。保存后重启 agent 生效"
                        } else {
                            "用量网关关着（在「自研内核」的设置里打开），这一项不起作用"
                        }),
                        Switch::new("acp-gateway").checked(f.acp_gateway).color(rgb(CORAL)).on_change(
                            cx.listener(|this, checked: &bool, _, cx| {
                                this.settings_form.acp_gateway = *checked;
                                cx.notify();
                            }),
                        ),
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
        let path = self.settings_path.as_ref().map_or_else(
            || "（数据目录不可用，设置不会保存）".to_string(),
            |p| p.display().to_string(),
        );
        let kernel_tab = body
            .child(
                div()
                    .h_flex()
                    .gap_2()
                    .child(
                        Self::text_button("settings-save", "保存并应用", true)
                            .on_click(cx.listener(|this, _, _, cx| this.save_settings(cx))),
                    )
                    .child(Self::text_button("settings-cancel", "取消", false).on_click(cx.listener(
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

        // 左侧分类导航：通用 / 内核与模型 / 权限
        let mut nav = div().v_flex().w(SETTINGS_NAV_W).flex_none().px_3().gap_0p5();
        for (i, (icon, label)) in SETTINGS_TABS.into_iter().enumerate() {
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
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if this.settings_form.tab != i {
                            this.settings_form.tab = i;
                            cx.notify();
                        }
                    }))
                    .child(Icon::new(icon).size_4().text_color(rgb(INK_SOFT)))
                    .child(label),
            );
        }
        let tab_content: Div = match f.tab {
            1 => kernel_tab,
            2 => self.render_rules(cx),
            3 => self.render_mcp(cx),
            PLUGINS_TAB => self.render_plugins(cx),
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
                    // 导航固定，只滚右侧内容
                    .child(
                        div().flex().flex_1().min_h_0().child(nav).child(
                            div()
                                .id("settings-scroll")
                                .flex_1()
                                .min_w_0()
                                .overflow_y_scroll()
                                // pt_2：首行文字与导航第一项的文字对齐
                                .child(div().v_flex().px_8().pt_2().pb_8().child(tab_content)),
                        ),
                    ),
            )
    }
}

/// 规则 → 给人看的说明
fn rule_label(rule: &str) -> String {
    if let Some(prefix) = rule.strip_prefix("shell:") {
        format!("运行以「{prefix}」开头的命令")
    } else if let Some(host) = rule.strip_prefix("fetch:") {
        format!("访问网站 {host}")
    } else if let Some(tool) = rule.strip_prefix("mcp:") {
        format!("调用 MCP 工具 {}", tool.replacen(':', " 的 ", 1))
    } else if rule == "edit" {
        "编辑项目内的文件".into()
    } else {
        rule.to_string()
    }
}

/// 界面字体下拉的条目：第一项是内嵌的 Inter，后面跟系统已装字体（可滚动，最多列一批）。
/// 当前项打勾；列不完的靠「还有 N 款」提示，需要的话在设置文件里手写字体名。
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
        menu_item(
            "font-embedded",
            IconAll::ALargeSmall,
            "Inter（内嵌）",
            "随应用分发，中文由系统字体回退",
            current.is_none(),
        ),
        |this, cx| this.set_font_family(None, cx),
    );

    let fonts = app.system_fonts();
    for (i, name) in fonts.iter().take(MAX_SYSTEM_FONTS).enumerate() {
        let active = current.as_deref() == Some(name.as_str());
        let picked: SharedString = name.clone().into();
        menu = menu.pick(
            menu_item(("font-item", i), IconAll::Type, name.clone(), "", active),
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

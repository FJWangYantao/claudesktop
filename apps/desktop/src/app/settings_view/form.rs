//! 设置表单的读写与模型 / 服务商管理（保存、拉取、增删档案）。

use super::*;

impl ChatApp {
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

    /// 首页选了某家模型服务：直接展开它的端点和密钥表单。
    pub(crate) fn start_setup_provider(
        &mut self,
        preset: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_settings_at(PROVIDER_TAB, window, cx);
        self.settings_form.kernel = KernelChoice::Builtin;
        let profile = preset.map(|i| preset_profiles().remove(i));
        let existing = profile.as_ref().and_then(|preset| {
            self.settings_form
                .profiles
                .iter()
                .position(|p| p.base_url.trim().eq_ignore_ascii_case(preset.base_url.trim()))
        });
        let i = match existing {
            Some(i) => {
                self.select_form_profile(i, window, cx);
                self.settings_form.editing_profile = true;
                i
            }
            None => {
                self.add_form_profile(preset, window, cx);
                self.settings_form.view
            }
        };
        if !self.settings.builtin.is_configured()
            && self.settings_form.profiles[i].model.trim().is_empty()
        {
            self.settings_form.active = i;
        }
        let local = profile.as_ref().is_none_or(|p| p.base_url.contains("11434"));
        self.settings_form.message = Some((
            false,
            if profile.is_none() {
                "填上服务商的端点、API Key 和模型名，再点「保存」".into()
            } else if profile.as_ref().is_some_and(|p| p.model.trim().is_empty()) {
                "填好端点后保存，再到「模型」页选择模型".into()
            } else {
                "填上 API Key，点右上角「保存」即可开始".into()
            },
        ));
        let input = {
            let inputs = &self.settings_form.inputs[i];
            if local { inputs.base_url.clone() } else { inputs.api_key.clone() }
        };
        input.update(cx, |state, cx| state.focus(window, cx));
        cx.notify();
    }

    /// 首页选了某个本机 agent：定位到它，检测到已安装时只启用这一家。
    pub(crate) fn start_setup_agent(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_settings_at(KERNEL_TAB, window, cx);
        self.settings_form.kernel = KernelChoice::Acp(id.to_string());
        match self.settings_form.agent_detect {
            AgentDetect::Done(_) => self.enable_detected_agent(),
            _ => {
                self.settings_form.enable_on_detect = true;
                self.settings_form.message =
                    Some((false, "正在检测本机安装，检测后自动启用".into()));
            }
        }
        cx.notify();
    }

    /// 启用首页选中的那家 agent（只改表单，保存后生效）。
    pub(super) fn enable_detected_agent(&mut self) {
        let f = &mut self.settings_form;
        let KernelChoice::Acp(id) = &f.kernel else { return };
        let AgentDetect::Done(list) = &f.agent_detect else { return };
        let Some(preset) = claudesktop_core::agents::AgentPreset::find(id) else { return };
        let Some(hit) = list.iter().find(|d| d.preset.id == preset.id) else {
            f.message = Some((
                false,
                format!("还没有安装{}，点右侧「如何安装」查看命令", preset.name).into(),
            ));
            return;
        };
        if !hit.is_ready() {
            f.message = Some((true, hit.summary().into()));
            return;
        }
        if !f.agents.iter().any(|a| a.id == preset.id) {
            f.agents.push(AgentProfile::from_preset(preset));
        }
        f.message = Some((false, format!("已选择{}，点「保存」生效", preset.name).into()));
    }

    pub(super) fn save_settings(&mut self, cx: &mut Context<Self>) {
        if let Err(err) = crate::pricing::parse_text(&self.settings_form.prices.read(cx).value()) {
            self.settings_form.message = Some((true, err.into()));
            cx.notify();
            return;
        }
        let new = self.form_settings(cx);
        // 自定义命令和已启用预设的命令一样：菜单里会出现两个一模一样的内核
        if let Some(preset) = duplicate_agent_command(&new) {
            self.settings_form.message = Some((
                true,
                format!(
                    "自定义命令与已启用的「{}」重复：请清空自定义命令，或停用该预设",
                    preset.display_name()
                )
                .into(),
            ));
            cx.notify();
            return;
        }
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
                self.rebind_session(idx, default.clone(), cx);
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
    pub(super) fn fetch_missing_models(&mut self, cx: &mut Context<Self>) {
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

    /// 查看服务商 Profile，不改全局默认模型。
    pub(super) fn select_form_profile(
        &mut self,
        i: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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

    pub(super) fn set_form_thinking(
        &mut self,
        level: Option<ThinkingLevel>,
        cx: &mut Context<Self>,
    ) {
        let active = self.settings_form.active;
        if let Some(profile) = self.settings_form.profiles.get_mut(active) {
            profile.params.thinking = level;
        }
        self.settings_form.message = None;
        cx.notify();
    }

    pub(super) fn form_settings(&self, cx: &App) -> Settings {
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
        // 默认内核：保留已保存的选择；所选档案被停用 / 删掉时才回退（交给 kernel() 重新自动选）
        keep_kernel_choice(&mut s);
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
    pub(super) fn remove_form_profile(&mut self, i: usize, cx: &mut Context<Self>) {
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
    pub(super) fn add_form_profile(
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
    pub(super) fn set_form_protocol(&mut self, protocol: Protocol, cx: &mut Context<Self>) {
        if let Some(p) = self.settings_form.profiles.get_mut(self.settings_form.view) {
            p.protocol = protocol;
            cx.notify();
        }
    }

    /// 改一处设置并只把这一处写回文件：生效中的设置可能含有环境变量给的值
    /// （如 key），没点「保存」就不该落盘。
    pub(crate) fn update_settings(&mut self, change: impl Fn(&mut Settings)) {
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

    pub(super) fn select_model_source(&mut self, source: Option<usize>, cx: &mut Context<Self>) {
        self.settings_form.model_source = source.filter(|&i| i < self.settings_form.profiles.len());
        self.settings_form.fetch_result = None;
        self.model_wheel = None;
        self.settings_form.model_scroll.set_offset(point(px(0.), px(0.)));
        cx.notify();
    }

    pub(super) fn refresh_model_sources(&mut self, cx: &mut Context<Self>) {
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
                                if saved == app.settings.builtin.active
                                    && let Some(kernel) = app.kernel_of(&KernelChoice::Builtin)
                                {
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
}

/// 自定义档案的启动命令和某个已启用预设相同时，返回撞车的那个预设。
/// 比较用 [`command_key`]：空白折叠、Windows 上忽略 `cmd /c` 前缀和大小写
/// （设置页手填提示本来就不带 `cmd /c`）。
fn duplicate_agent_command(s: &Settings) -> Option<&AgentProfile> {
    let windows = cfg!(windows);
    let key = |cmd: &str| command_key(cmd, windows);
    let customs: Vec<String> = s
        .acp
        .agents
        .iter()
        .filter(|a| a.preset().is_none())
        .map(|a| key(&a.command_for(windows)))
        .filter(|c| !c.is_empty())
        .collect();
    if customs.is_empty() {
        return None;
    }
    s.acp
        .agents
        .iter()
        .filter(|a| a.preset().is_some())
        .find(|a| customs.iter().any(|c| *c == key(&a.command_for(windows))))
}

fn command_key(cmd: &str, windows: bool) -> String {
    let mut parts: Vec<&str> = cmd.split_whitespace().collect();
    if windows
        && parts.len() >= 2
        && parts[0].eq_ignore_ascii_case("cmd")
        && parts[1].eq_ignore_ascii_case("/c")
    {
        parts.drain(..2);
    }
    let joined = parts.join(" ");
    if windows { joined.to_lowercase() } else { joined }
}

/// 保存设置时保留默认内核的选择；选择失效时才置空，交给 [`Settings::kernel`] 重新自动选。
fn keep_kernel_choice(s: &mut Settings) {
    // 选了某个 ACP 档案，但它被停用 / 删掉了
    if let Some(KernelChoice::Acp(id)) = &s.kernel
        && s.acp.get(id).is_none()
    {
        s.kernel = None;
    }
    // 默认是自研，但自研已经没配好、又有启用的 agent：否则保存会被自研校验
    // （「还没有设置端点地址」）锁死，而设置页没有改默认内核的地方
    if matches!(s.kernel, Some(KernelChoice::Builtin))
        && !s.builtin.is_configured()
        && s.acp.first().is_some()
    {
        s.kernel = None;
    }
}

#[cfg(test)]
mod kernel_choice_tests {
    use super::*;

    fn preset_profile(id: &str) -> AgentProfile {
        AgentProfile::from_preset(claudesktop_core::agents::AgentPreset::find(id).unwrap())
    }

    #[core::prelude::v1::test]
    fn saved_kernel_choice_survives_when_its_agent_is_still_enabled() {
        let mut s = Settings::default();
        s.acp.agents = vec![preset_profile("claude"), preset_profile("pi")];
        s.kernel = Some(KernelChoice::Acp("claude".into()));
        keep_kernel_choice(&mut s);
        assert_eq!(s.kernel, Some(KernelChoice::Acp("claude".into())));
        assert_eq!(s.kernel(), KernelChoice::Acp("claude".into()));
    }

    #[core::prelude::v1::test]
    fn disabled_agent_falls_back_to_auto_choice() {
        let mut s = Settings::default();
        s.acp.agents = vec![preset_profile("pi")];
        s.kernel = Some(KernelChoice::Acp("claude".into()));
        keep_kernel_choice(&mut s);
        assert_eq!(s.kernel, None);
    }

    #[core::prelude::v1::test]
    fn builtin_default_survives_while_builtin_is_configured() {
        let mut s = Settings::default();
        s.builtin.current_mut().base_url = "https://api.example.com".into();
        s.builtin.current_mut().model = "m".into();
        s.acp.agents = vec![preset_profile("pi")];
        s.kernel = Some(KernelChoice::Builtin);
        keep_kernel_choice(&mut s);
        assert_eq!(s.kernel, Some(KernelChoice::Builtin));
    }

    /// 自研被清空后默认还停在自研：不能把保存锁死在自研校验上
    #[core::prelude::v1::test]
    fn unconfigured_builtin_default_falls_back_to_enabled_agent() {
        let mut s = Settings::default();
        s.acp.agents = vec![preset_profile("pi")];
        s.kernel = Some(KernelChoice::Builtin);
        keep_kernel_choice(&mut s);
        assert_eq!(s.kernel, None);
        assert_eq!(s.kernel(), KernelChoice::Acp("pi".into()));
    }

    #[core::prelude::v1::test]
    fn custom_command_matching_enabled_preset_is_rejected() {
        let mut s = Settings::default();
        let pi = preset_profile("pi");
        let clone = AgentProfile::custom(1, &pi.command_for(cfg!(windows)));
        s.acp.agents = vec![pi, clone];
        assert_eq!(duplicate_agent_command(&s).map(|a| a.id.clone()), Some("pi".into()));
    }

    #[core::prelude::v1::test]
    fn whitespace_differences_still_count_as_duplicate() {
        let mut s = Settings::default();
        let pi = preset_profile("pi");
        let spaced = format!("  {}  ", pi.command_for(cfg!(windows)).replace(' ', "  "));
        s.acp.agents = vec![pi, AgentProfile::custom(1, &spaced)];
        assert!(duplicate_agent_command(&s).is_some());
    }

    #[core::prelude::v1::test]
    fn different_custom_command_is_allowed() {
        let mut s = Settings::default();
        s.acp.agents = vec![preset_profile("pi"), AgentProfile::custom(1, "npx -y other-acp")];
        assert!(duplicate_agent_command(&s).is_none());
    }

    /// 预设的启动命令带 `cmd /c`，用户手填一般不带：前缀和大小写都不能绕过检查
    #[cfg(windows)]
    #[core::prelude::v1::test]
    fn windows_cmd_prefix_and_case_do_not_bypass_duplicate_check() {
        let mut s = Settings::default();
        s.acp.agents = vec![preset_profile("pi"), AgentProfile::custom(1, "CMD /C npx -y pi-acp")];
        assert!(duplicate_agent_command(&s).is_some());
    }
}

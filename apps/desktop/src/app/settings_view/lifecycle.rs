//! 设置页的开合、外观生效、内核状态刷新与 Magpie 引导。

use super::*;

impl ChatApp {
    pub(crate) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // 记住上次看的 tab（打开时不动它），要指定 tab 的走 `open_settings_at`；
        // 停在用量页时回到进用量页之前的分类（用量不算设置的分类）
        let mut tab = self.settings_form.tab;
        if tab == crate::app::usage_view::USAGE_TAB {
            tab = self.usage_page.settings_tab;
        }
        // 切走再回来时若外部没改过设置，保留未保存的草稿方便继续修改或保存
        if self.settings_form.base_settings.as_ref() == Some(&self.settings) && self.form_dirty(cx)
        {
            self.settings_form.tab = tab;
            self.settings_open = true;
            self.suggestion.clear();
            cx.notify();
            return;
        }
        self.open_settings_at(tab, window, cx);
    }

    pub(crate) fn close_settings(&mut self, cx: &mut Context<Self>) {
        if self.settings_form.tab == crate::app::project_view::PROJECT_TAB {
            self.apply_project(cx);
        }
        let dirty =
            self.settings_form.tab != crate::app::usage_view::USAGE_TAB && self.form_dirty(cx);
        self.settings_open = false;
        if dirty {
            self.toast(
                Notification::warning("设置里有未保存的改动，暂未生效（回设置页可保存或撤销）"),
                cx,
            );
        }
        cx.notify();
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
        let brave = claudesktop_builtin::websearch::brave_key().unwrap_or_default();
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
        self.settings_form.base_settings = Some(s);
        self.settings_wheel = None;
        self.model_wheel = None;
        self.settings_scroll.set_offset(point(px(0.), px(0.)));
        self.settings_form.model_scroll.set_offset(point(px(0.), px(0.)));
        self.settings_open = true;
        // 设置页开着时输入框不在画面上：光标后面那句灰字先撤掉（关掉设置页后接着打字会重猜）
        self.suggestion.clear();
        // 「记忆」一栏按需读两份记忆文件（内核可能刚记下一条）
        if tab == crate::app::memory_view::MEMORY_TAB {
            self.reload_memory_view();
        }
        // 「项目」一栏重读项目根的那个配置文件（可能是手改过的）
        if tab == crate::app::project_view::PROJECT_TAB {
            self.reload_project_view(window, cx);
        }
        if tab == KERNEL_TAB && matches!(self.settings_form.agent_detect, AgentDetect::Idle) {
            self.detect_agents(cx);
        }
        cx.notify();
    }

    pub(super) fn kernel_status_busy(&self) -> bool {
        matches!(self.settings_form.agent_detect, AgentDetect::Running)
            || self.settings_form.magpie.probing
            || self.settings_form.magpie.starting
            || self.settings_form.magpie.downloading.is_some()
    }

    pub(super) fn refresh_kernel_status(&mut self, cx: &mut Context<Self>) {
        if self.kernel_status_busy() {
            return;
        }
        self.detect_agents(cx);
        self.probe_magpie(cx);
        cx.notify();
    }

    /// 后台探一遍本机装了哪些 agent（PATH + 常见安装目录，每家起一次子进程问版本）。
    pub(super) fn detect_agents(&mut self, cx: &mut Context<Self>) {
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
    pub(super) fn probe_magpie(&mut self, cx: &mut Context<Self>) {
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
    pub(super) fn setup_magpie(&mut self, cx: &mut Context<Self>) {
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
    pub(super) fn start_magpie(&mut self, exe: &std::path::Path, cx: &mut Context<Self>) {
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
    pub(super) fn auto_start_magpie_if_active(&mut self, cx: &mut Context<Self>) {
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
    pub(super) fn apply_font(&mut self, cx: &mut Context<Self>) {
        // 色板与代码配色先设：后面 `Theme::update` 里的 `apply_to_theme` 按它们写组件主题
        crate::theme::set_palette(self.settings.theme.index());
        crate::syntax::set_code_theme(self.settings.code_theme);
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

    /// 设置页换代码块语法配色：立即生效，并只把这一项写回文件。
    pub(crate) fn set_code_theme(&mut self, code_theme: CodeThemeChoice, cx: &mut Context<Self>) {
        self.save_ui_pref(move |s| s.code_theme = code_theme, cx);
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

    pub(super) fn set_appearance(
        &mut self,
        a: Appearance,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        self.save_ui_pref(move |s| s.appearance = a, cx);
        self.apply_appearance(window, cx);
    }

    /// 设置页切「动画」：立即生效并写回。
    pub(super) fn set_motion_pref(&mut self, pref: MotionPref, cx: &mut Context<Self>) {
        self.save_ui_pref(move |s| s.motion = pref, cx);
        self.apply_motion_pref(cx);
        cx.notify();
    }

    /// 侧栏底部要不要列出内核状态（立即生效，不走「保存」）。
    pub(super) fn set_sidebar_kernel_status(&mut self, on: bool, cx: &mut Context<Self>) {
        self.save_ui_pref(move |s| s.sidebar_kernel_status = on, cx);
        cx.notify();
    }
}

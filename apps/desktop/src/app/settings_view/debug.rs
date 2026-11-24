//! 设置页的无头调试入口（预览与状态探针）。

use super::*;

impl ChatApp {
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
            self.settings = self.form_settings(cx);
            self.settings_form.base_settings = Some(self.settings.clone());
            assert_eq!(self.dirty_tabs(cx), (false, false, false));
            self.select_model_source(None, cx);
            self.select_form_model(1, "gpt-5".into(), window, cx);
            assert!(self.dirty_tabs(cx).2);
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
            let dirty_tabs = self.dirty_tabs(cx);
            self.close_settings(cx);
            assert!(!self.settings_open);
            self.open_settings(window, cx);
            assert!(self.settings_open && self.form_dirty(cx));
            assert_eq!(self.dirty_tabs(cx), dirty_tabs);
            self.open_settings_at(MODEL_TAB, window, cx);
            assert!(!self.form_dirty(cx));
            assert_eq!(self.sidebar_open, sidebar);
            assert!(self.settings_navigation_only());
            eprintln!(
                "settings checks passed: source filter, default routing, thinking ownership, removal, manual model, draft preservation, discard, sidebar state"
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
}

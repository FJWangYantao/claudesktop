//! 「联网搜索」一栏：服务商、密钥与开关。

use super::*;

impl ChatApp {
    /// 「网页搜索」：先用的后端 + 允许哪些后端 + 各自的 key。
    ///
    /// 选择存在内核那边的数据目录 `search.json`（搜索是即时读取的，不经 `Kernel::set_*`），
    /// 所以这里不走页头「保存」：改了就写盘，下一轮搜索立刻用新顺序。默认顺序和加 DeepSeek
    /// 之前一致（Brave → DuckDuckGo），DeepSeek 是按次计费的新花费，要用户自己选。
    pub(super) fn render_web_search(&self, cx: &mut Context<Self>) -> Div {
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
    pub(super) fn deepseek_search_key_from_profiles(&self) -> Option<String> {
        self.settings_form
            .profiles
            .iter()
            .filter(|p| crate::settings::is_deepseek_endpoint(p))
            .map(|p| p.api_key.trim().to_string())
            .find(|k| !k.is_empty())
    }

    /// 把搜索后端的选择写进数据目录（不用点页头「保存」）。
    pub(super) fn save_search(
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
}

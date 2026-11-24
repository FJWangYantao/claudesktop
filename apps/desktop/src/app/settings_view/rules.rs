//! 「规则」一栏：项目 / 全局规则的展示与删除。

use super::*;

impl ChatApp {
    /// 删一条「总是允许」规则。内核每次判断前都重读规则文件，所以立即生效。
    pub(super) fn remove_rule(&mut self, project: String, rule: String, cx: &mut Context<Self>) {
        let mut rules = PermissionRules::load(default_rules_path());
        if let Err(err) = rules.remove(&project, &rule) {
            self.settings_form.message = Some((true, format!("删除规则失败：{err}").into()));
        }
        self.settings_form.rules = rules.list();
        cx.notify();
    }

    pub(super) fn render_rules(&self, cx: &mut Context<Self>) -> Div {
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
}
/// 规则 → 给人看的说明
pub(super) fn rule_label(rule: &str) -> String {
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

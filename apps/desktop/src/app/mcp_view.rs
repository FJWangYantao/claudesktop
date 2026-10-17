//! 设置页「MCP」：自研内核接的 MCP 服务器，各自的连接状态，编辑配置文件 / 重新加载。
//!
//! 配置在数据目录的 `mcp.json`（格式同 Claude Code 的 `.mcp.json`），由内核读（`claudesktop_builtin::mcp`）；
//! 状态经 `KernelEvent::McpStatus` 回来，存在自研内核那个 `KernelSlot::mcp` 里。

use super::ChatApp;
use crate::settings::KernelChoice;
use crate::theme::rgb;
use crate::theme::*;
use claudesktop_builtin::default_mcp_config;
use claudesktop_builtin::mcp::{self, CONFIG_TEMPLATE};
use claudesktop_core::{McpServerView, McpState};
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

impl ChatApp {
    /// 用系统默认程序打开 `mcp.json`；还没有就先写一份模板（示例服务器是禁用的）。
    fn edit_mcp_config(&mut self, cx: &mut Context<Self>) {
        let Some(path) = default_mcp_config() else {
            self.toast(Notification::error("找不到数据目录，没法建 mcp.json"), cx);
            return;
        };
        if !path.exists() {
            let written = path
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| std::fs::write(&path, CONFIG_TEMPLATE));
            if let Err(err) = written {
                self.toast(Notification::error(format!("写不了 {}：{err}", path.display())), cx);
                return;
            }
        }
        cx.open_with_system(&path);
    }

    /// 重新读配置、重连全部服务器；自研内核还没起来就把它起来（起来时自己会连）。
    fn reload_mcp(&mut self, cx: &mut Context<Self>) {
        match self.kernel_of(KernelChoice::Builtin) {
            Some(kernel) => kernel.reload_mcp(),
            None => self.ensure_kernel(KernelChoice::Builtin),
        }
        cx.notify();
    }

    pub(super) fn render_mcp(&self, cx: &mut Context<Self>) -> Div {
        let path =
            default_mcp_config().map_or_else(|| "mcp.json".into(), |p| p.display().to_string());
        let header = div()
            .h_flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .text_size(TEXT_BODY)
                    .font_weight(FontWeight::MEDIUM)
                    .child("MCP 服务器"),
            )
            .child(
                Self::text_button("mcp-edit", "编辑配置文件", false)
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| this.edit_mcp_config(cx))),
            )
            .child(
                Self::text_button("mcp-reload", "重新加载", false)
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| this.reload_mcp(cx))),
            );
        let section = div().v_flex().gap_2().child(header).child(
            div().text_size(TEXT_SMALL).text_color(rgb(INK_SOFT)).child(format!(
                "自研内核可以调用 MCP 服务器提供的工具（只支持本地命令）。配置写在 {path}，格式同 Claude Code 的 .mcp.json；改完点「重新加载」。工具调用照常弹权限卡。"
            )),
        );

        let running = self.kernel_of(KernelChoice::Builtin).is_some();
        let servers: Vec<McpServerView> = if running {
            self.kernels[KernelChoice::Builtin.index()].mcp.clone()
        } else {
            // 内核没起来：照配置文件列出来，状态写「未连接」
            default_mcp_config()
                .and_then(|p| mcp::load_config(&p).ok())
                .unwrap_or_default()
                .into_iter()
                .map(|c| McpServerView {
                    name: c.name,
                    state: if c.disabled { McpState::Disabled } else { McpState::Connecting },
                    tools: 0,
                })
                .collect()
        };
        if servers.is_empty() {
            return section.child(
                div()
                    .py_2()
                    .text_size(TEXT_SMALL)
                    .text_color(rgb(INK_SOFT))
                    .child("还没有配置 MCP 服务器。点「编辑配置文件」，照模板填好、去掉 \"disabled\"，再点「重新加载」。"),
            );
        }
        let mut card = div().v_flex().rounded_lg().border_1().border_color(rgb(LINE)).bg(rgb(CARD));
        for (i, server) in servers.iter().enumerate() {
            let (dot, label, detail) = match &server.state {
                McpState::Ready => (GREEN, format!("{} 个工具", server.tools), None),
                McpState::Connecting if !running => {
                    (INK_FAINT, "未连接（自研内核没在运行）".into(), None)
                }
                McpState::Connecting => (INK_FAINT, "连接中…".into(), None),
                McpState::Disabled => (INK_FAINT, "已禁用".into(), None),
                McpState::Failed(reason) => (RED, "没连上".into(), Some(reason.clone())),
            };
            card = card.child(
                div()
                    .v_flex()
                    .gap_1()
                    .px_3()
                    .py_2()
                    .when(i > 0, |d| d.border_t_1().border_color(rgb(LINE)))
                    .child(
                        div()
                            .h_flex()
                            .items_center()
                            .gap_2()
                            .child(div().size(px(7.)).flex_shrink_0().rounded_full().bg(rgb(dot)))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .font_family(MONO)
                                    .text_size(TEXT_SMALL)
                                    .text_color(rgb(INK))
                                    .child(server.name.clone()),
                            )
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_size(TEXT_SMALL)
                                    .text_color(rgb(if dot == RED { RED } else { INK_SOFT }))
                                    .font_features(tabular_nums())
                                    .child(label),
                            ),
                    )
                    // 失败原因（含服务器 stderr 的最后几行）：等宽小字，缩进到名字下面
                    .when_some(detail, |d, reason| {
                        d.child(
                            div()
                                .pl(px(15.))
                                .font_family(MONO)
                                .text_size(TEXT_META)
                                .text_color(rgb(INK_SOFT))
                                .child(reason),
                        )
                    }),
            );
        }
        section.child(card).child(
            div()
                .h_flex()
                .items_center()
                .gap_1p5()
                .text_size(TEXT_META)
                .text_color(rgb(INK_SOFT))
                .child(Icon::new(IconAll::Info).size_3p5().flex_shrink_0())
                .child("模型看到的工具名是 mcp__服务器__工具；「总是允许」按项目、服务器和工具记在权限规则里。"),
        )
    }
}

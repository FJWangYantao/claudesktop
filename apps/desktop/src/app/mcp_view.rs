//! 设置页「MCP」：自研内核接的 MCP 服务器（本地命令或远程 HTTP / SSE），各自的连接状态，
//! 编辑配置文件 / 重新加载 / 单个服务重新连接。连上过又断了的服务内核会自己重连，这里显示「重连中」。
//!
//! 分「已安装」「推荐」两栏，上面一个搜索框：推荐的常用服务（`claudesktop_core::catalog::MCP_PICKS`）
//! 点「添加」就写进 mcp.json 并重新加载；要填的东西（文件夹、令牌）先问。mcp.json 里的服务可以直接开关、删除。
//!
//! 配置在数据目录的 `mcp.json`（格式同 Claude Code 的 `.mcp.json`），由内核读（`claudesktop_builtin::mcp`）；
//! 状态经 `KernelEvent::McpStatus` 回来，存在自研内核那个 `KernelSlot::mcp` 里。

use super::ChatApp;
use super::catalog_view::{
    Shelf, ShelfState, list_card, matches, no_match, pick_done, pick_row, shelf_bar,
};
use super::settings_kit::{example_block, how_to, note, section};
use crate::settings::KernelChoice;
use crate::theme::rgb;
use crate::theme::*;
use claudesktop_builtin::default_mcp_config;
use claudesktop_builtin::mcp::{self, CONFIG_TEMPLATE};
use claudesktop_core::catalog::{MCP_PICKS, McpPick, PickInput};
use claudesktop_core::{McpServerView, McpState};
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::button::{
    Button, ButtonCustomVariant, ButtonVariant, ButtonVariants as _,
};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

/// 设置页「配置示例」里的 mcp.json：一个本地命令、一个远程服务。
const CONFIG_EXAMPLE: &str = r#"{
  "mcpServers": {
    "filesystem": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "D:/notes"]
    },
    "remote": {
      "type": "http",
      "url": "https://example.com/mcp",
      "headers": { "Authorization": "Bearer ${MY_TOKEN}" }
    }
  }
}"#;

/// MCP 页自己的状态：栏位和搜索、正在填令牌的推荐项、等确认删除的服务。
pub(super) struct McpForm {
    pub(super) shelf: ShelfState,
    /// 推荐项要的令牌输入框（同一时间只问一个）
    secret: Entity<InputState>,
    /// 点了「添加」、正在填令牌的推荐项
    asking: Option<&'static str>,
    /// 点了删除、等确认的服务器名
    confirm_remove: Option<String>,
}

impl McpForm {
    pub(super) fn new(window: &mut Window, cx: &mut Context<ChatApp>) -> Self {
        let secret = cx.new(|cx| InputState::new(window, cx).masked(true));
        cx.subscribe_in(&secret, window, |this: &mut ChatApp, _, event: &InputEvent, _, cx| {
            if let InputEvent::PressEnter { .. } = event
                && let Some(pick) = this.mcp_form.asking.and_then(find_pick)
            {
                this.submit_secret(pick, cx);
            }
        })
        .detach();
        Self {
            shelf: ShelfState::new("搜索服务器名字或说明", window, cx),
            secret,
            asking: None,
            confirm_remove: None,
        }
    }
}

fn find_pick(name: &str) -> Option<&'static McpPick> {
    MCP_PICKS.iter().find(|p| p.name == name)
}

impl ChatApp {
    /// 推荐项「添加」：要文件夹的先弹选文件夹，要令牌的先展开输入框，其余直接写进 mcp.json。
    fn add_mcp_pick(
        &mut self,
        pick: &'static McpPick,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 装了一个之后还停在「推荐」（栏位自动时会跳去「已安装」），方便接着加
        self.mcp_form.shelf.shelf = Some(Shelf::Picks);
        match pick.input {
            None => self.write_mcp_pick(pick, "", cx),
            Some(PickInput::Folder) => {
                let rx = cx.prompt_for_paths(PathPromptOptions {
                    files: false,
                    directories: true,
                    multiple: false,
                    prompt: Some(format!("选择「{}」能访问的文件夹", pick.title).into()),
                });
                cx.spawn(async move |this, cx| {
                    if let Ok(Ok(Some(paths))) = rx.await
                        && let Some(dir) = paths.into_iter().next()
                    {
                        let dir = dir.to_string_lossy().into_owned();
                        let _ = this.update(cx, |app, cx| app.write_mcp_pick(pick, &dir, cx));
                    }
                })
                .detach();
            }
            Some(PickInput::Secret { .. }) => {
                self.mcp_form.asking = Some(pick.name);
                self.mcp_form.secret.update(cx, |s, cx| {
                    s.set_value("", window, cx);
                    s.focus(window, cx);
                });
                cx.notify();
            }
        }
    }

    /// 令牌填好了（回车或「保存」）。
    fn submit_secret(&mut self, pick: &'static McpPick, cx: &mut Context<Self>) {
        let value = self.mcp_form.secret.read(cx).value().trim().to_string();
        if value.is_empty() {
            return;
        }
        self.mcp_form.asking = None;
        self.write_mcp_pick(pick, &value, cx);
    }

    /// 把推荐项写进 mcp.json，重新加载（内核没起来就起来，起来时自己连）。
    fn write_mcp_pick(&mut self, pick: &McpPick, input: &str, cx: &mut Context<Self>) {
        let Some(path) = default_mcp_config() else {
            self.toast(Notification::error("找不到数据目录，没法建 mcp.json"), cx);
            return;
        };
        match mcp::add_server(&path, pick.name, pick.entry(input)) {
            Ok(()) => {
                self.reload_mcp(cx);
                self.toast(
                    Notification::success(format!(
                        "已添加「{}」，正在连接；在「已安装」里看状态",
                        pick.title
                    )),
                    cx,
                );
            }
            Err(err) => self.toast(Notification::error(err), cx),
        }
        cx.notify();
    }

    /// 开关 mcp.json 里的一个服务器。
    fn set_mcp_enabled(&mut self, name: &str, on: bool, cx: &mut Context<Self>) {
        let Some(path) = default_mcp_config() else {
            return;
        };
        match mcp::set_server_disabled(&path, name, !on) {
            Ok(()) => self.reload_mcp(cx),
            Err(err) => self.toast(Notification::error(err), cx),
        }
    }

    /// 从 mcp.json 删掉一个服务器。
    fn remove_mcp_server(&mut self, name: &str, cx: &mut Context<Self>) {
        self.mcp_form.confirm_remove = None;
        let Some(path) = default_mcp_config() else {
            return;
        };
        match mcp::remove_server(&path, name) {
            Ok(()) => self.reload_mcp(cx),
            Err(err) => self.toast(Notification::error(err), cx),
        }
        cx.notify();
    }

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
            // 列表里马上出现模板里的示例（禁用状态），不用等手动「重新加载」
            self.reload_mcp(cx);
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

    /// 重连一个没连上的服务。
    fn reconnect_mcp(&mut self, name: &str, cx: &mut Context<Self>) {
        if let Some(kernel) = self.kernel_of(KernelChoice::Builtin) {
            kernel.reconnect_mcp(name);
        }
        cx.notify();
    }

    pub(super) fn render_mcp(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let path =
            default_mcp_config().map_or_else(|| "mcp.json".into(), |p| p.display().to_string());
        let actions = div()
            .h_flex()
            .gap_1p5()
            .child(
                Self::text_button("mcp-edit", "编辑配置文件", false)
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| this.edit_mcp_config(cx))),
            )
            .child(
                Self::text_button("mcp-reload", "重新加载", false)
                    .small()
                    .tooltip("改完配置文件后点它，重新读配置并重连全部服务器")
                    .on_click(cx.listener(|this, _, _, cx| this.reload_mcp(cx))),
            )
            .into_any_element();
        // 配置示例：空着时和列表下面都能展开看
        let example = div()
            .v_flex()
            .gap_2()
            .child(example_block(CONFIG_EXAMPLE))
            .child(note(IconAll::Dot, "本地程序写 command 和 args（需要时加 env），远程服务写 type 和 url"))
            .child(note(
                IconAll::Dot,
                "headers 和 env 的值里可以写 ${环境变量}，Key 不用明文写进文件；旧版 SSE 服务把 type 写成 \"sse\"",
            ))
            .child(note(IconAll::Dot, "\"disabled\": true 表示先不连；格式同 Claude Code 的 .mcp.json"));
        let footer = div()
            .v_flex()
            .gap_1()
            .child(self.disclosure("mcp-example", "配置示例", example, window, cx))
            .child(note(IconAll::Braces, format!("配置文件：{path}")))
            .child(note(
                IconAll::ShieldCheck,
                "调用 MCP 工具同样会弹权限卡片；模型看到的工具名是 mcp__服务器__工具。启用的插件带来的服务器也列在这里",
            ));

        let running = self.kernel_of(KernelChoice::Builtin).is_some();
        let servers: Vec<McpServerView> = if running {
            self.kernels[KernelChoice::Builtin.index()].mcp.clone()
        } else {
            // 内核没起来：照配置文件列出来，状态写「未连接」
            match default_mcp_config().map(|p| mcp::load_config(&p)) {
                Some(Ok(configs)) => configs
                    .into_iter()
                    .map(|c| McpServerView {
                        state: if c.disabled { McpState::Disabled } else { McpState::Connecting },
                        tools: 0,
                        plugin: None,
                        url: (c.transport != mcp::Transport::Stdio).then(|| c.url.clone()),
                        retry: false,
                        name: c.name,
                    })
                    .collect(),
                // 文件坏了：和内核起来后报的一样，列成一行「mcp.json 格式不对」
                Some(Err(reason)) => vec![McpServerView {
                    name: "mcp.json".into(),
                    state: McpState::Failed(reason),
                    tools: 0,
                    plugin: None,
                    url: None,
                    retry: false,
                }],
                None => Vec::new(),
            }
        };
        let shelf = &self.mcp_form.shelf;
        let query = shelf.query(cx);
        let bar = shelf_bar(
            "mcp-shelf",
            shelf,
            servers.len(),
            MCP_PICKS.len(),
            |this| &mut this.mcp_form.shelf,
            cx,
        );
        let list = match shelf.current(servers.len()) {
            Shelf::Picks => self.mcp_picks(&servers, &query, cx),
            Shelf::Installed if servers.is_empty() => how_to(
                "还没有 MCP 服务器",
                &[
                    "点上面的「推荐」，挑常用的点「添加」，马上就能用",
                    "别的服务器：点右上角「编辑配置文件」，照下面的「配置示例」写上，保存",
                    "回来点「重新加载」，这里会列出每个服务器连没连上",
                ],
            ),
            Shelf::Installed => {
                let mut card = list_card();
                let mut shown = 0;
                for (i, server) in servers.iter().enumerate() {
                    let plugin = server.plugin.as_deref().unwrap_or_default();
                    // 配置文件坏了那一行总是列出来
                    if server.name != "mcp.json" && !matches(&query, &[&server.name, plugin]) {
                        continue;
                    }
                    card = card.child(self.mcp_row(i, shown == 0, server, running, cx));
                    shown += 1;
                }
                if shown == 0 { no_match(&query) } else { card }
            }
        };
        section(
            "MCP 服务器",
            "给自研内核接上外部工具，比如浏览器、数据库、GitHub",
            Some(actions),
            div().v_flex().gap_4().child(bar).child(list).child(footer),
        )
    }

    /// 「推荐」栏：每个常用服务一行，已经在 mcp.json 里的写「已添加」。
    fn mcp_picks(&self, servers: &[McpServerView], query: &str, cx: &mut Context<Self>) -> Div {
        let mut card = list_card();
        let mut shown = 0;
        for (i, pick) in MCP_PICKS.iter().enumerate() {
            if !matches(query, &[pick.name, pick.title, pick.description]) {
                continue;
            }
            let added = servers.iter().any(|s| s.name == pick.name && s.plugin.is_none());
            let asking = self.mcp_form.asking == Some(pick.name);
            let action = if added {
                pick_done("已添加").into_any_element()
            } else {
                Self::text_button(("mcp-pick", i), "添加", false)
                    .small()
                    .with_variant(ButtonVariant::Secondary)
                    .disabled(asking)
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.add_mcp_pick(pick, window, cx)),
                    )
                    .into_any_element()
            };
            let row =
                pick_row(shown == 0, pick.title, pick.name, pick.needs, pick.description, action);
            card = card.child(row);
            if asking && let Some(PickInput::Secret { label, hint }) = pick.input {
                card = card.child(self.secret_form(pick, label, hint, cx));
            }
            shown += 1;
        }
        if shown == 0 { no_match(query) } else { card }
    }

    /// 推荐项要令牌时展开的一行：输入框 + 保存 / 取消，下面一句去哪里拿。
    fn secret_form(
        &self,
        pick: &'static McpPick,
        label: &'static str,
        hint: &'static str,
        cx: &mut Context<Self>,
    ) -> Div {
        div()
            .v_flex()
            .gap_1p5()
            .px_3()
            .pb_2p5()
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(TEXT_SMALL)
                            .text_color(rgb(INK_SOFT))
                            .child(label),
                    )
                    .child(
                        div().flex_1().min_w_0().rounded_lg().bg(rgb(SAND)).px_1().child(
                            Input::new(&self.mcp_form.secret).appearance(false).bordered(false),
                        ),
                    )
                    .child(
                        Self::text_button("mcp-secret-save", "保存", true).small().on_click(
                            cx.listener(move |this, _, _, cx| this.submit_secret(pick, cx)),
                        ),
                    )
                    .child(
                        Self::text_button("mcp-secret-cancel", "取消", false)
                            .small()
                            .with_variant(ButtonVariant::Secondary)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.mcp_form.asking = None;
                                cx.notify();
                            })),
                    ),
            )
            .child(note(IconAll::Info, hint))
    }

    /// 「已安装」里的一个服务器：状态点、名字、来源、状态，按需给登录 / 重连；
    /// mcp.json 里的还有开关和删除。
    fn mcp_row(
        &self,
        i: usize,
        first: bool,
        server: &McpServerView,
        running: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        // 配置文件本身坏了：内核把它报成一个叫 mcp.json 的失败服务，改好文件点「重新加载」，单独重连没用
        let config_error = server.name == "mcp.json"
            && server.url.is_none()
            && server.plugin.is_none()
            && matches!(server.state, McpState::Failed(_));
        // 需要 OAuth 登录：给一个「登录」按钮，打开授权页
        let (dot, label, detail, sign_in) = match &server.state {
            McpState::Failed(reason) if config_error => (
                RED,
                "格式不对".into(),
                Some(format!(
                    "{}。改好后点「重新加载」",
                    reason.strip_prefix("mcp.json 格式不对：").unwrap_or(reason)
                )),
                None,
            ),
            McpState::Ready => (GREEN, format!("{} 个工具", server.tools), None, None),
            McpState::Connecting if !running => {
                (INK_FAINT, "未连接（自研内核没在运行）".into(), None, None)
            }
            McpState::Connecting => (INK_FAINT, "连接中…".into(), None, None),
            McpState::Disabled => (INK_FAINT, "已停用".into(), None, None),
            McpState::Failed(reason) => (RED, "没连上".into(), Some(reason.clone()), None),
            McpState::Reconnecting(detail) => {
                (CORAL, "断开了，重连中…".into(), Some(detail.clone()), None)
            }
            McpState::NeedsAuth { url } => (
                CORAL,
                "需要登录".into(),
                Some("在浏览器里完成授权，回来后自动连上".into()),
                Some(url.clone()),
            ),
        };
        // mcp.json 里的（不是插件带来的、不是坏文件那一行）：能开关、删除
        let own = server.plugin.is_none() && !config_error;
        let enabled = !matches!(server.state, McpState::Disabled);
        let retry_name = server.name.clone();
        let toggle_name = server.name.clone();
        let remove_name = server.name.clone();
        let confirming = self.mcp_form.confirm_remove.as_deref() == Some(server.name.as_str());
        div()
            .v_flex()
            .gap_1()
            .px_3()
            .py_2()
            .when(!first, |d| d.border_t_1().border_color(rgb(LINE)))
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
                    // 插件带来的服务：注明来源（不在 mcp.json 里，要去插件页停用）
                    .when_some(server.plugin.clone(), |d, plugin| {
                        d.child(
                            div()
                                .flex_shrink_0()
                                .text_size(TEXT_SMALL)
                                .text_color(rgb(INK_SOFT))
                                .child(format!("插件 {plugin}")),
                        )
                    })
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(TEXT_SMALL)
                            .text_color(rgb(if dot == RED { RED } else { INK_SOFT }))
                            .font_features(tabular_nums())
                            .child(label),
                    )
                    // 需要登录时主按钮是「登录」；重新连接照样给（流程过期了要重来一轮）
                    .when_some(sign_in, |d, url| {
                        d.child(Self::text_button(("mcp-login", i), "登录", true).small().on_click(
                            cx.listener(move |_, _, _, cx| {
                                cx.open_url(&url);
                            }),
                        ))
                    })
                    .when(server.retry && running && !config_error, |d| {
                        d.child(
                            Self::text_button(("mcp-retry", i), "重新连接", false)
                                .small()
                                .with_variant(ButtonVariant::Secondary)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.reconnect_mcp(&retry_name, cx)
                                })),
                        )
                    })
                    .when(own, |d| {
                        d.child(
                            Self::quiet_button(
                                ("mcp-remove", i),
                                IconAll::Trash,
                                super::widgets::Surface::Paper,
                                cx,
                            )
                            .small()
                            .tooltip("从 mcp.json 删除")
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.mcp_form.confirm_remove = Some(remove_name.clone());
                                    cx.notify();
                                },
                            )),
                        )
                        .child(
                            Switch::new(("mcp-enabled", i))
                                .checked(enabled)
                                .color(rgb(CORAL))
                                .on_change(cx.listener(move |this, on: &bool, _, cx| {
                                    this.set_mcp_enabled(&toggle_name, *on, cx)
                                })),
                        )
                    }),
            )
            // 远程服务的地址
            .when_some(server.url.clone(), |d, url| {
                d.child(
                    div()
                        .pl(px(15.))
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .font_family(MONO)
                        .text_size(TEXT_META)
                        .text_color(rgb(INK_SOFT))
                        .child(url),
                )
            })
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
            })
            .when(confirming, |d| d.child(self.remove_prompt(i, &server.name, cx)))
    }

    /// 点了删除之后行底部的确认。
    fn remove_prompt(&self, i: usize, name: &str, cx: &mut Context<Self>) -> Div {
        let name = name.to_string();
        div()
            .h_flex()
            .items_center()
            .gap_2()
            .pl(px(15.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(TEXT_SMALL)
                    .text_color(rgb(RED))
                    .child(format!("从 mcp.json 删除 {name}？它的配置（包括令牌）会一起删掉")),
            )
            .child(
                Button::new(("mcp-remove-yes", i))
                    .small()
                    .label("删除")
                    .cursor_pointer()
                    .custom(
                        ButtonCustomVariant::new(cx)
                            .foreground(rgb(RED).into())
                            .hover(rgb(HOVER).into())
                            .active(rgb(HOVER).into()),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.remove_mcp_server(&name, cx))),
            )
            .child(
                Self::text_button(("mcp-remove-no", i), "取消", false)
                    .small()
                    .with_variant(ButtonVariant::Secondary)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.mcp_form.confirm_remove = None;
                        cx.notify();
                    })),
            )
    }
}

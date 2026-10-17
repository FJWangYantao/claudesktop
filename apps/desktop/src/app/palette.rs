//! 命令面板（Ctrl+K）：对话框里放 gpui-kit `Command`，搜操作、会话、视图、权限模式和模型。
//!
//! 条目不挂 `.action()`：对话框在 gpui-kit Root 的浮层里、不在 `ChatApp` 的键上下文下，
//! 派发出去的动作到不了 `keys.rs` 的处理函数。确认统一走 `on_confirm`，按下标找回 `PaletteCmd`。
//! 行内容用 `.child()` 自绘（图标、名称、灰色说明、快捷键 / 对勾），`.label()` / `.keywords()` 只管搜索。

use super::ChatApp;
use super::keys::{NEW_SESSION_KEY, SHORTCUTS_KEY, TOGGLE_SIDEBAR_KEY};
use super::project_label;
use crate::model::ViewMode;
use crate::settings::KernelChoice;
use crate::theme::rgb;
use crate::theme::*;
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::command::{Command, CommandGroup, CommandItem, CommandState};
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::rc::Rc;

/// 面板宽度（rem：跟着界面缩放）
const PALETTE_W: Rems = rems(36.);

/// 一条命令确认后做的事。
#[derive(Clone, Debug, PartialEq)]
enum PaletteCmd {
    NewSession,
    ToggleSidebar,
    PickWorkspace,
    OpenSettings,
    Shortcuts,
    Export,
    Compact,
    Session(usize),
    View(ViewMode),
    Mode(usize),
    Model(String),
}

/// 一条命令的显示内容。
struct Entry {
    icon: IconAll,
    label: SharedString,
    desc: SharedString,
    key: Option<&'static str>,
    checked: bool,
    mono: bool,
    cmd: PaletteCmd,
}

impl Entry {
    fn new(icon: IconAll, label: impl Into<SharedString>, cmd: PaletteCmd) -> Self {
        Self {
            icon,
            label: label.into(),
            desc: SharedString::default(),
            key: None,
            checked: false,
            mono: false,
            cmd,
        }
    }

    fn desc(mut self, desc: impl Into<SharedString>) -> Self {
        self.desc = desc.into();
        self
    }

    fn key(mut self, key: &'static str) -> Self {
        self.key = Some(key);
        self
    }

    fn checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }

    fn mono(mut self) -> Self {
        self.mono = true;
        self
    }

    /// 面板里的一行：图标 + 名称 + 灰色说明，右端快捷键或当前项对勾。
    fn item(&self) -> CommandItem {
        let (icon, label, desc) = (self.icon, self.label.clone(), self.desc.clone());
        let (key, checked, mono) = (self.key, self.checked, self.mono);
        CommandItem::new().label(label.clone()).keywords([desc.clone()]).child(move |_, _| {
            div()
                .h_flex()
                .items_center()
                .gap_2p5()
                .w_full()
                .min_w_0()
                .child(Icon::new(icon).size_4().text_color(rgb(INK_SOFT)))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(TEXT_BODY)
                        .text_color(rgb(INK))
                        .when(mono, |d| d.font_family(MONO))
                        .child(label.clone()),
                )
                .when(!desc.is_empty(), |d| {
                    d.child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(TEXT_SMALL)
                            .text_color(rgb(INK_SOFT))
                            .child(desc.clone()),
                    )
                })
                .child(div().flex_1())
                .when_some(key, |d, k| d.child(ChatApp::kbd(k)))
                .when(checked, |d| {
                    d.child(Icon::new(IconAll::Check).size_4().text_color(rgb(CORAL)))
                })
        })
    }
}

/// 按当前状态列出面板的各组命令（空组不出现）。
fn palette_groups(app: &ChatApp) -> Vec<(&'static str, Vec<Entry>)> {
    let s = app.session();
    let mut actions = vec![
        Entry::new(IconAll::MessageSquarePlus, "新会话", PaletteCmd::NewSession)
            .key(NEW_SESSION_KEY),
        Entry::new(
            IconAll::PanelLeft,
            if app.sidebar_open { "收起侧栏" } else { "展开侧栏" },
            PaletteCmd::ToggleSidebar,
        )
        .key(TOGGLE_SIDEBAR_KEY),
        Entry::new(IconAll::FolderOpen, "切换工作目录…", PaletteCmd::PickWorkspace),
        Entry::new(IconAll::Settings, "打开设置", PaletteCmd::OpenSettings),
        Entry::new(IconAll::Keyboard, "键盘快捷键", PaletteCmd::Shortcuts).key(SHORTCUTS_KEY),
    ];
    if s.has_user_message() {
        actions.push(Entry::new(IconAll::Share, "导出为 Markdown", PaletteCmd::Export));
        actions.push(
            Entry::new(IconAll::ListCollapse, "压缩上下文", PaletteCmd::Compact)
                .desc("把较早的对话换成摘要"),
        );
    }

    // 会话按侧栏的显示顺序
    let sessions = app
        .session_groups()
        .into_iter()
        .flat_map(|(_, members)| members)
        .map(|i| {
            let session = &app.sessions[i];
            Entry::new(IconAll::MessageSquare, session.title.clone(), PaletteCmd::Session(i))
                .desc(project_label(&session.cwd))
                .checked(i == app.active && !app.settings_open)
        })
        .collect();

    let views = ViewMode::ALL
        .into_iter()
        .map(|m| {
            Entry::new(m.icon(), format!("视图：{}", m.label()), PaletteCmd::View(m))
                .desc(m.desc())
                .checked(m == app.settings.view_mode)
        })
        .collect();

    let modes = s
        .modes
        .iter()
        .enumerate()
        .map(|(i, m)| {
            Entry::new(m.icon, m.name.clone(), PaletteCmd::Mode(i))
                .desc(m.desc.clone())
                .checked(i == s.permission_mode)
        })
        .collect();

    // 只有自研内核能在这里换模型（别的内核由 agent 自己决定）
    let models = if s.kernel == KernelChoice::Builtin {
        let current = s.model_label.clone();
        app.settings
            .builtin
            .models()
            .iter()
            .map(|m| {
                Entry::new(IconAll::Cpu, m.clone(), PaletteCmd::Model(m.clone()))
                    .mono()
                    .checked(current.as_ref() == m.as_str())
            })
            .collect()
    } else {
        Vec::new()
    };

    [("操作", actions), ("会话", sessions), ("视图", views), ("权限模式", modes), ("模型", models)]
        .into_iter()
        .filter(|(_, entries)| !entries.is_empty())
        .collect()
}

impl ChatApp {
    /// 打开命令面板（已有对话框开着时不叠第二个）。
    pub(crate) fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if window.has_active_dialog(cx) {
            return;
        }
        let state = cx.new(|cx| CommandState::new(window, cx));
        let app = cx.entity();
        let palette = state.clone();
        window.open_dialog(cx, move |dialog, _, cx| {
            let groups = palette_groups(app.read(cx));
            let mut command = Command::new(&palette)
                .placeholder("搜索命令、会话、模型…")
                .bordered(false)
                .empty(|_, _, _| {
                    div()
                        .py_6()
                        .text_center()
                        .text_size(TEXT_SMALL)
                        .text_color(rgb(INK_SOFT))
                        .child("没有匹配的命令")
                });
            for (title, entries) in &groups {
                command = command.group(
                    CommandGroup::new().label(*title).items(entries.iter().map(Entry::item)),
                );
            }
            // 只有分组时 IndexPath 的 section 就是组序号、row 是组内序号
            let cmds: Rc<Vec<Vec<PaletteCmd>>> = Rc::new(
                groups
                    .into_iter()
                    .map(|(_, entries)| entries.into_iter().map(|e| e.cmd).collect())
                    .collect(),
            );
            let app = app.clone();
            command = command.on_confirm(move |ix, window, cx| {
                let Some(cmd) = cmds.get(ix.section).and_then(|g| g.get(ix.row)).cloned() else {
                    return;
                };
                window.close_dialog(cx);
                app.update(cx, |this, cx| this.run_palette(cmd, window, cx));
            });
            dialog.w(PALETTE_W.to_pixels(root_font_size())).close_button(false).p_1().child(command)
        });
        state.update(cx, |state, cx| state.focus(window, cx));
    }

    fn run_palette(&mut self, cmd: PaletteCmd, window: &mut Window, cx: &mut Context<Self>) {
        match cmd {
            PaletteCmd::NewSession => {
                self.settings_open = false;
                self.add_session(cx);
            }
            PaletteCmd::ToggleSidebar => self.sidebar_open = !self.sidebar_open,
            PaletteCmd::PickWorkspace => self.pick_workspace(cx),
            PaletteCmd::OpenSettings => self.open_settings(window, cx),
            // 面板的对话框刚关，同一帧里还算开着：等下一帧再开速查
            PaletteCmd::Shortcuts => {
                cx.defer_in(window, |this, window, cx| this.open_shortcuts(window, cx));
            }
            PaletteCmd::Export => self.share_session(window, cx),
            PaletteCmd::Compact => self.compact_session(cx),
            PaletteCmd::Session(i) => {
                self.settings_open = false;
                self.activate_session(i, cx);
            }
            PaletteCmd::View(mode) => self.set_view_mode(mode, cx),
            PaletteCmd::Mode(i) => self.choose_mode(i, cx),
            PaletteCmd::Model(model) => self.choose_model(model, cx),
        }
        self.restore_focus(window, cx);
        cx.notify();
    }
}

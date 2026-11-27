//! 自由 Pane 工作台：把对话 / 文件 / 改动 / 计划 / 子代理 / 终端 / 浏览器
//! 都做成可拖拽换位、合并 Tab、左右上下分屏、Zoom 全宽、弹出成独立窗的面板。
//!
//! 底座是 `gpui_base::dock`（vendor 里打过补丁，见 `vendor/README.md`）：那里的 `DockArea`
//! 只管布局树和实体缓存，**一点外观都不画**，外观全靠本模块的
//! [`skin::ClaudesktopDockSkin`]（`DockAreaRenderer` + `TabGroupRenderer`）。
//!
//! 这里的两层 trait 是为了绕开「子 trait object 无法从父 trait object 还原」：
//! base 只认 [`gpui_base::dock::PanelView`]，而皮肤要读标题、图标、badge 这些展示信息，
//! 所以面板统一包一层具体类型 [`PaneHandle`]，皮肤用
//! `panel.as_any().downcast_ref::<PaneHandle>()` 拿回来。

pub(crate) mod chat_pane;
pub(crate) mod diff_pane;
pub(crate) mod files_pane;
pub(crate) mod persist;
pub(crate) mod plan_pane;
pub(crate) mod popout;
pub(crate) mod side_chat_pane;
pub(crate) mod skin;
pub(crate) mod subagent_pane;
pub(crate) mod terminal_pane;
pub(crate) mod workspace;

use gpui_base::dock::Panel as BasePanel;
use gpui_base::dock::{PanelId, PanelInfo, PanelState, PanelView as BasePanelView};
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::*;
use std::{any::Any, sync::Arc};

/// Pane 的种类：Tab 上的图标 / 标题、持久化时的 `panel_name` 都按它定。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PaneKind {
    Chat,
    SideChat,
    Diff,
    Files,
    Plan,
    Subagent,
    Terminal,
    Browser,
}

impl PaneKind {
    /// 写进 `layout.json` 的 `panel_name`：改了就再也读不回旧布局，别随便改。
    pub(crate) fn panel_name(self) -> &'static str {
        match self {
            Self::Chat => "claudesktop-chat",
            Self::SideChat => "claudesktop-side-chat",
            Self::Diff => "claudesktop-diff",
            Self::Files => "claudesktop-files",
            Self::Plan => "claudesktop-plan",
            Self::Subagent => "claudesktop-subagent",
            Self::Terminal => "claudesktop-terminal",
            Self::Browser => "claudesktop-browser",
        }
    }

    /// 名称 → 种类；将来读 `layout.json` 里存的自定义 payload 时用
    #[allow(dead_code)]
    pub(crate) fn from_panel_name(name: &str) -> Option<Self> {
        [
            Self::Chat,
            Self::SideChat,
            Self::Diff,
            Self::Files,
            Self::Plan,
            Self::Subagent,
            Self::Terminal,
            Self::Browser,
        ]
        .into_iter()
        .find(|kind| kind.panel_name() == name)
    }

    pub(crate) fn title(self) -> SharedString {
        match self {
            Self::Chat => "对话".into(),
            Self::SideChat => "侧问".into(),
            Self::Diff => "改动".into(),
            Self::Files => "文件".into(),
            Self::Plan => "计划".into(),
            Self::Subagent => "子代理".into(),
            Self::Terminal => "终端".into(),
            Self::Browser => "浏览器".into(),
        }
    }

    pub(crate) fn icon(self) -> IconAll {
        match self {
            Self::Chat | Self::SideChat => IconAll::MessageSquare,
            Self::Diff => IconAll::FileDiff,
            Self::Files => IconAll::FolderTree,
            Self::Plan => IconAll::ListTodo,
            Self::Subagent => IconAll::GitBranch,
            Self::Terminal => IconAll::Terminal,
            Self::Browser => IconAll::Globe,
        }
    }
}

/// 展示与跨窗生命周期扩展（叠加在 `gpui_base::dock::Panel` 之上）。
///
/// `icon` / `session_token` / `on_window_changed` 现在只有改动 Pane 用到其中一部分，
/// 其余是 Phase 2–5（弹出窗、侧问、终端、浏览器）要用的协议，先按完整形状定下来。
#[allow(dead_code)]
pub(crate) trait ClaudesktopPane: BasePanel {
    fn pane_kind(&self) -> PaneKind;

    fn title(&self, _cx: &App) -> SharedString {
        self.pane_kind().title()
    }

    fn icon(&self, _cx: &App) -> IconAll {
        self.pane_kind().icon()
    }

    /// Tab 标题右边的小标记（改动数、运行中的后台命令数……）
    fn badge(&self, _cx: &App) -> Option<SharedString> {
        None
    }

    /// 这个 Pane 在看哪个会话（侧问、改动、子代理都是会话相关的）
    fn session_token(&self, _cx: &App) -> Option<u64> {
        None
    }

    /// Tab 栏右侧的自定义按钮（改动 Scope 切换、文件刷新、浏览器端口芯片……）
    fn header_actions(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> Vec<AnyElement> {
        Vec::new()
    }

    /// 换窗口（弹出 / 吸回）之后必须调：绑定旧 `Window` 的输入状态要在这里重建
    fn on_window_changed(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {}
}

/// [`ClaudesktopPane`] 的对象安全镜像。
#[allow(dead_code)]
pub(crate) trait ClaudesktopPaneView: BasePanelView {
    fn pane_kind(&self, cx: &App) -> PaneKind;
    fn title(&self, cx: &App) -> SharedString;
    fn icon(&self, cx: &App) -> IconAll;
    fn badge(&self, cx: &App) -> Option<SharedString>;
    fn session_token(&self, cx: &App) -> Option<u64>;
    fn header_actions(&self, window: &mut Window, cx: &mut App) -> Vec<AnyElement>;
    fn on_window_changed(&self, window: &mut Window, cx: &mut App);
}

/// 面板的具体句柄。base 只存 `Arc<dyn PanelView>`，皮肤要拿回展示信息只能靠
/// 具体类型的 `downcast_ref`——所以每个面板都包成它再交给 base。
#[derive(Clone)]
pub(crate) struct PaneHandle(Arc<dyn ClaudesktopPaneView>);

impl PaneHandle {
    pub(crate) fn new<P: ClaudesktopPane>(panel: Entity<P>) -> Self {
        Self(Arc::new(panel))
    }

    /// 从 base 给的句柄还原；不是本模块包出来的（比如 base 自己的占位面板）得到 None。
    pub(crate) fn of(panel: &Arc<dyn BasePanelView>) -> Option<&Self> {
        panel.as_any().downcast_ref::<Self>()
    }

    pub(crate) fn kind(&self, cx: &App) -> PaneKind {
        self.0.pane_kind(cx)
    }

    pub(crate) fn display_title(&self, cx: &App) -> SharedString {
        self.0.title(cx)
    }

    pub(crate) fn display_badge(&self, cx: &App) -> Option<SharedString> {
        self.0.badge(cx)
    }

    pub(crate) fn actions(&self, window: &mut Window, cx: &mut App) -> Vec<AnyElement> {
        self.0.header_actions(window, cx)
    }
}

/// 跨窗迁移（Phase 2）要用的读口：图标、会话令牌、换窗口后的重建钩子。
#[allow(dead_code)]
impl PaneHandle {
    pub(crate) fn display_icon(&self, cx: &App) -> IconAll {
        self.0.icon(cx)
    }

    pub(crate) fn token(&self, cx: &App) -> Option<u64> {
        self.0.session_token(cx)
    }

    pub(crate) fn window_changed(&self, window: &mut Window, cx: &mut App) {
        self.0.on_window_changed(window, cx)
    }
}

impl BasePanelView for PaneHandle {
    fn panel_name(&self, cx: &App) -> &'static str {
        self.0.panel_name(cx)
    }

    fn panel_id(&self, cx: &App) -> PanelId {
        self.0.panel_id(cx)
    }

    fn closable(&self, cx: &App) -> bool {
        self.0.closable(cx)
    }

    fn zoomable(&self, cx: &App) -> bool {
        self.0.zoomable(cx)
    }

    fn visible(&self, cx: &App) -> bool {
        self.0.visible(cx)
    }

    fn set_active(&self, active: bool, window: &mut Window, cx: &mut App) {
        self.0.set_active(active, window, cx);
    }

    fn set_zoomed(&self, zoomed: bool, window: &mut Window, cx: &mut App) {
        self.0.set_zoomed(zoomed, window, cx);
    }

    fn on_added_to(
        &self,
        group: WeakEntity<gpui_base::dock::TabGroup>,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.0.on_added_to(group, window, cx);
    }

    fn on_removed(&self, window: &mut Window, cx: &mut App) {
        self.0.on_removed(window, cx);
    }

    fn view(&self) -> AnyView {
        self.0.view()
    }

    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.0.focus_handle(cx)
    }

    fn dump(&self, cx: &App) -> PanelState {
        self.0.dump(cx)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// `Entity<P>` 直接当 [`ClaudesktopPaneView`] 用（拿到具体句柄之后的默认实现）。
impl<P: ClaudesktopPane> ClaudesktopPaneView for Entity<P> {
    fn pane_kind(&self, cx: &App) -> PaneKind {
        self.read(cx).pane_kind()
    }

    fn title(&self, cx: &App) -> SharedString {
        self.read(cx).title(cx)
    }

    fn icon(&self, cx: &App) -> IconAll {
        self.read(cx).icon(cx)
    }

    fn badge(&self, cx: &App) -> Option<SharedString> {
        self.read(cx).badge(cx)
    }

    fn session_token(&self, cx: &App) -> Option<u64> {
        self.read(cx).session_token(cx)
    }

    fn header_actions(&self, window: &mut Window, cx: &mut App) -> Vec<AnyElement> {
        self.update(cx, |this, cx| this.header_actions(window, cx))
    }

    fn on_window_changed(&self, window: &mut Window, cx: &mut App) {
        self.update(cx, |this, cx| this.on_window_changed(window, cx))
    }
}

/// 面板自己的持久化载荷：`panel_name` 决定谁来重建它，`info` 里放这个面板要记住的东西。
pub(crate) fn pane_state(kind: PaneKind, info: serde_json::Value) -> PanelState {
    PanelState {
        panel_name: kind.panel_name().to_string(),
        children: Vec::new(),
        info: PanelInfo::panel(info),
    }
}

/// 让一个 Pane 跟着 `WorkspaceHub` 的事件自己重画。
///
/// 主窗里的 Pane 靠 `ChatApp` 每帧重绘顺带刷新，但**弹出到独立窗之后就没人管它**了：
/// 主窗流式生成、切会话、改文件时那张窗会完全静止。所以每个 Pane 建的时候都订一次，
/// 事件类型先不细挑——Pane 的渲染都是便宜的投影，重画一次比漏一次刷新划算。
pub(crate) fn follow_hub<T: 'static>(
    hub: &Entity<crate::app::hub::WorkspaceHub>,
    cx: &mut Context<T>,
) {
    cx.subscribe(
        hub,
        |_: &mut T,
         _: Entity<crate::app::hub::WorkspaceHub>,
         _: &crate::app::hub::HubEvent,
         cx: &mut Context<T>| {
            cx.notify();
        },
    )
    .detach();
}

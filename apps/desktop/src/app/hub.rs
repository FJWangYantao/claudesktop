//! `WorkspaceHub`：跨窗口共享的那一层状态（L1）。
//!
//! 主窗（[`super::ChatApp`]）、每个 Pane、每个弹出窗都从这同一份数据源读：
//! 活跃会话、会话摘要、当前设置、浮层开合。它只持有**别的窗口要用的那份投影**，
//! 会话记录本体、内核槽、输入框这些仍然跟着各自的视图走（见
//! `docs/dev/gpui-ui.md` 的视图分层）。
//!
//! 变化用 [`HubEvent`] 定向广播：事件带会话令牌，订阅者自己判断是不是在看的那个会话，
//! 不认识的令牌直接忽略。这样主窗切会话、发消息时，弹出窗里的 Diff / Plan / 侧问
//! 只会被叫醒一次，不用每帧轮询。

use crate::settings::Settings;
use gpui_kit::component::WindowExt as _;
use gpui_kit::*;

/// 一条会话的轻量摘要：Pane 的 Tab 标题、侧问 Pane 判断上下文、弹出窗标题都读它。
/// 会话本体仍在主窗里，这里只放「不打开会话也知道」的那些字段。
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SessionSummary {
    pub token: u64,
    pub title: SharedString,
    /// 聊天空间（没有工作目录）还是代码空间
    pub chat: bool,
    /// 任务会话（侧栏「任务」分组）
    pub task: bool,
    /// 内核正在跑（有一轮没结束）
    pub busy: bool,
    /// 这个会话的工作目录（代码空间才有）
    pub dir: Option<std::path::PathBuf>,
}

/// 跨窗口广播的事件。所有 Pane 在初始化时订阅，按令牌过滤。
///
/// 带会话令牌的几样（改动 / 计划 / 子代理）在 Pane 层接上之后才有发布点，
/// 现在挂着只为把协议一次定完，避免后面改订阅方签名。
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum HubEvent {
    /// 主窗换了活跃会话（`u64` 是新会话的令牌）
    ActiveSessionChanged(u64),
    /// 某个会话的记录 / 状态行 / 工具进度变了
    SessionUpdated(u64),
    /// 某个会话的改动预览（diff）变了
    DiffChanged(u64),
    /// 某个会话的待办 / 计划变了
    PlanChanged(u64),
    /// 某个会话的子代理列表变了
    SubagentChanged(u64),
    /// 后台命令的输出 / 状态变了（不区分会话：停靠条是全局的）
    BgTasksChanged,
    /// 主题或字号变了（Pane 里的自绘元素要重算尺寸）
    ThemeOrFontChanged,
    /// 主窗上有浮层（命令面板 / 设置 / 对话框 / Popover）开合了。
    /// `BrowserPane` 的遮挡保护靠它决定要不要先把 WebView2 藏起来贴占位帧。
    OverlayVisibilityChanged(bool),
}

pub(crate) struct WorkspaceHub {
    /// 当前活跃会话的令牌；还没有会话时为 None
    active_session: Option<u64>,
    /// 会话摘要（顺序同侧栏：打开顺序 / 最近活动由主窗决定）
    sessions: Vec<SessionSummary>,
    /// 生效中的设置（Pane 读主题、默认模型、字号）
    settings: Settings,
    /// 主窗上有没有浮层挡着（命令面板 / 对话 / 设置 / 弹层 / 重看浮层）
    overlay_visible: bool,
}

impl EventEmitter<HubEvent> for WorkspaceHub {}

/// 读口（`sessions` / `session` / `settings` / `overlay_visible`）是给 Pane 层用的：
/// 现在只有主窗在读写投影，读口暂时没有调用者。
#[allow(dead_code)]
impl WorkspaceHub {
    pub(crate) fn new(settings: Settings) -> Self {
        Self { active_session: None, sessions: Vec::new(), settings, overlay_visible: false }
    }

    pub(crate) fn new_entity(cx: &mut App, settings: Settings) -> Entity<Self> {
        cx.new(|_| Self::new(settings))
    }

    pub(crate) fn active_session(&self) -> Option<u64> {
        self.active_session
    }

    pub(crate) fn sessions(&self) -> &[SessionSummary] {
        &self.sessions
    }
    pub(crate) fn session(&self, token: u64) -> Option<&SessionSummary> {
        self.sessions.iter().find(|s| s.token == token)
    }

    pub(crate) fn settings(&self) -> &Settings {
        &self.settings
    }

    /// 主窗换了活跃会话：静默记下（外面的 `publish` 负责发事件）。
    pub(crate) fn set_active_session(&mut self, token: Option<u64>) {
        self.active_session = token;
    }

    /// 主窗把会话摘要同步过来。返回摘要是否真的变了——没变就不发事件，
    /// 免得每帧一次的全量同步把订阅者白白叫醒。
    pub(crate) fn sync_sessions(&mut self, sessions: Vec<SessionSummary>) -> bool {
        if self.sessions == sessions {
            return false;
        }
        self.sessions = sessions;
        true
    }

    /// 设置变了（主题 / 字号 / 模型）：返回是否真的变了。
    pub(crate) fn sync_settings(&mut self, settings: Settings) -> bool {
        if self.settings == settings {
            return false;
        }
        self.settings = settings;
        true
    }

    /// 主窗把「现在有浮层吗」的结论交进来（命令面板 / 设置 / 重看浮层 / 审查页）：
    /// 翻转才广播。`BrowserPane` 的遮挡保护靠它。
    pub(crate) fn sync_overlay(&mut self, visible: bool) -> bool {
        self.set_overlay_visible(visible)
    }

    /// 浮层开合。**绝对值**而不是计数：主窗每帧把「现在有浮层吗」算一次的结论交进来，
    /// 少一次配对就不会永远卡在「被遮挡」。返回可见性是否翻转，只有翻转才值得广播。
    pub(crate) fn set_overlay_visible(&mut self, visible: bool) -> bool {
        let was = self.overlay_visible;
        self.overlay_visible = visible;
        was != visible
    }

    pub(crate) fn overlay_visible(&self) -> bool {
        self.overlay_visible
    }
}

impl super::ChatApp {
    /// 把主窗的会话清单 / 活跃会话 / 设置投影到 hub，并把变了的那几样广播出去。
    ///
    /// 每帧调一次（和 `sync_sidebar` 同一个位置）：内部只做一次 Vec 比较，
    /// 没变化时不发事件、不通知任何订阅者，所以稳态下是零成本。
    pub(super) fn sync_hub(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let active = self.session().token;
        let settings = self.settings.clone();
        let summaries: Vec<super::hub::SessionSummary> = self
            .sessions
            .iter()
            .map(|s| super::hub::SessionSummary {
                token: s.token,
                title: s.title.clone(),
                chat: s.is_chat(),
                task: s.task,
                busy: s.busy,
                dir: (!s.is_chat()).then(|| s.group_dir().to_path_buf()),
            })
            .collect();

        let hub = self.hub.clone();
        let mut events = Vec::new();
        // 浮层：命令面板 / 设置 / 审查页 / 回退浮层都是「盖住工作区」的那一类。
        // 命令面板是 gpui 的 dialog，靠窗口状态问出来；其余是 ChatApp 自己的标记。
        let overlay = self.settings_open
            || self.review.is_some()
            || self.rewind.is_some()
            || window.has_active_dialog(cx);
        hub.update(cx, |hub, _| {
            let active_changed = hub.active_session() != Some(active);
            hub.set_active_session(Some(active));
            if hub.sync_sessions(summaries) || active_changed {
                events.push(HubEvent::SessionUpdated(active));
            }
            if active_changed {
                events.push(HubEvent::ActiveSessionChanged(active));
            }
            if hub.sync_settings(settings) {
                events.push(HubEvent::ThemeOrFontChanged);
            }
            if hub.sync_overlay(overlay) {
                events.push(HubEvent::OverlayVisibilityChanged(overlay));
            }
        });
        for event in events {
            hub.update(cx, |_, cx| cx.emit(event));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hub() -> WorkspaceHub {
        WorkspaceHub::new(Settings::default())
    }

    /// 同一批摘要同步两次，第二次不算变化（否则每帧都会惊动订阅者）。
    #[core::prelude::v1::test]
    fn syncing_the_same_sessions_twice_reports_no_change() {
        let mut hub = hub();
        let one = vec![SessionSummary {
            token: 1,
            title: "会话".into(),
            chat: false,
            task: false,
            busy: false,
            dir: None,
        }];
        assert!(hub.sync_sessions(one.clone()));
        assert!(!hub.sync_sessions(one));
    }

    /// 浮层深度是计数：开着两层时关掉一层，仍然是「被遮挡」。
    #[core::prelude::v1::test]
    fn overlay_visibility_only_flips_at_the_edges() {
        let mut hub = hub();
        assert!(hub.set_overlay_visible(true));
        assert!(!hub.set_overlay_visible(true), "第二层不算翻转");
        assert!(hub.overlay_visible());
        assert!(!hub.set_overlay_visible(false), "关掉一层仍然被遮挡");
        assert!(hub.set_overlay_visible(false), "最后一层关掉才算露出");
        assert!(!hub.overlay_visible());
    }
}

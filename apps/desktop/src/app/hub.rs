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
    /// 会话清单的指纹（主窗算、这里存，没变就不重建摘要）
    fingerprint: u64,
    /// 设置的修订号（主窗设置一变就 +1，这里存已发布的那个）
    settings_rev: u64,
}

impl EventEmitter<HubEvent> for WorkspaceHub {}

/// 读口（`sessions` / `session` / `settings` / `overlay_visible`）是给 Pane 层用的：
/// 现在只有主窗在读写投影，读口暂时没有调用者。
#[allow(dead_code)]
impl WorkspaceHub {
    pub(crate) fn new(settings: Settings) -> Self {
        Self {
            active_session: None,
            sessions: Vec::new(),
            settings,
            overlay_visible: false,
            fingerprint: 0,
            settings_rev: 0,
        }
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

    /// 会话清单指纹（主窗算好交进来，只用来判断「要不要重建摘要」）。
    pub(crate) fn fingerprint(&self) -> u64 {
        self.fingerprint
    }

    pub(crate) fn set_fingerprint(&mut self, fingerprint: u64) {
        self.fingerprint = fingerprint;
    }

    /// 设置修订号：主窗设置一变就 +1，比 `Settings` 本身便宜得多。
    pub(crate) fn settings_rev(&self) -> u64 {
        self.settings_rev
    }

    pub(crate) fn set_settings_rev(&mut self, rev: u64) {
        self.settings_rev = rev;
    }

    /// 整体换掉会话摘要（只在指纹变了之后调）。
    pub(crate) fn replace_sessions(&mut self, sessions: Vec<SessionSummary>) {
        self.sessions = sessions;
    }

    /// 整体换掉设置（只在修订号变了之后调）。
    pub(crate) fn replace_settings(&mut self, settings: Settings) {
        self.settings = settings;
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
    /// 每帧都会被 `render` 叫到，所以先比指纹：会话清单和设置都没变时**不分配**
    /// （以前每帧 clone 一次 `Settings` 再重建一遍 `Vec<SessionSummary>` + 每个 `PathBuf`，
    /// 100 个会话就是每帧几百次分配）。浮层只有一个 bool，顺手比掉。
    pub(super) fn sync_hub(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // 改动 Pane 的 badge 也要在面板收起时跟上（它不渲染就不会自己刷新）
        self.refresh_changed_file_count();
        let active = self.session().token;
        // 浮层：命令面板 / 设置 / 审查页 / 回退浮层都是「盖住工作区」的那一类。
        // 命令面板是 gpui 的 dialog，靠窗口状态问出来；其余是 ChatApp 自己的标记。
        let overlay = self.settings_open
            || self.review.is_some()
            || self.rewind.is_some()
            || window.has_active_dialog(cx);
        let fingerprint = self.hub_fingerprint();
        let settings_rev = self.settings_rev;

        let hub = self.hub.clone();
        let mut events = Vec::new();
        let mut overlay_flipped = false;
        hub.update(cx, |hub, _| {
            overlay_flipped = hub.sync_overlay(overlay);
            let active_changed = hub.active_session() != Some(active);
            if !active_changed
                && fingerprint == hub.fingerprint()
                && settings_rev == hub.settings_rev()
            {
                return;
            }
            hub.set_active_session(Some(active));
            hub.set_fingerprint(fingerprint);
            hub.set_settings_rev(settings_rev);
            if active_changed {
                events.push(HubEvent::ActiveSessionChanged(active));
            }
            events.push(HubEvent::SessionUpdated(active));
        });
        if overlay_flipped {
            events.push(HubEvent::OverlayVisibilityChanged(overlay));
        }
        // 摘要 / 设置只在真有变化时才重建，重建完写进 hub
        let rebuild = events.iter().any(|e| matches!(e, HubEvent::SessionUpdated(_)));
        if rebuild {
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
            let settings = self.settings.clone();
            hub.update(cx, |hub, _| {
                hub.replace_sessions(summaries);
                hub.replace_settings(settings);
            });
            if settings_rev != self.hub_settings_published {
                self.hub_settings_published = settings_rev;
                events.push(HubEvent::ThemeOrFontChanged);
            }
        }
        for event in events {
            hub.update(cx, |_, cx| cx.emit(event));
        }
    }

    /// 会话清单的便宜指纹：只算已有字段，不分配。
    ///
    /// 除了「有几个会话、标题变没变」，还要带上**活跃会话的推进量**：Agent 跑一轮时
    /// `busy` 从头到尾都是 `true`，只看它的话，这一整轮里改文件、推进计划、子代理前进一步
    /// 都不会让指纹变，弹出窗里的 Diff / Plan / Subagent 就得等整轮结束才刷。
    fn hub_fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.sessions.len().hash(&mut hasher);
        for session in &self.sessions {
            session.token.hash(&mut hasher);
            session.title.len().hash(&mut hasher);
            session.busy.hash(&mut hasher);
            session.task.hash(&mut hasher);
            session.is_chat().hash(&mut hasher);
            if !session.is_chat() {
                session.group_dir().as_os_str().len().hash(&mut hasher);
            }
        }
        // 活跃会话的推进量：都是整数，零分配
        let active = self.session();
        active.diff_revision.hash(&mut hasher);
        active.diff_epoch.hash(&mut hasher);
        active.transcript.len().hash(&mut hasher);
        // 子代理的进度（步数和条目数）：按 id 排序后累加，避免 HashMap 顺序抖动
        let mut ids: Vec<&String> = active.subagents.keys().collect();
        ids.sort();
        for id in ids {
            id.len().hash(&mut hasher);
            active.subagents[id].items.len().hash(&mut hasher);
        }
        hasher.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hub() -> WorkspaceHub {
        WorkspaceHub::new(Settings::default())
    }

    /// 指纹一样就不重建摘要：`replace_sessions` 只在主窗算出新指纹之后才调，
    /// 这里断言「同一批摘要 + 同一指纹」不会让 hub 里多出东西。
    #[core::prelude::v1::test]
    fn replacing_sessions_keeps_the_fingerprint_it_was_given() {
        let mut hub = hub();
        let one = vec![SessionSummary {
            token: 1,
            title: "会话".into(),
            chat: false,
            task: false,
            busy: false,
            dir: None,
        }];
        hub.replace_sessions(one);
        hub.set_fingerprint(7);
        assert_eq!(hub.fingerprint(), 7);
        assert_eq!(hub.sessions().len(), 1);
        assert_eq!(hub.session(1).map(|s| s.title.as_ref()), Some("会话"));
    }

    /// 浮层是绝对值：重复设同一个值不算翻转，只有跨过边界那一次才广播。
    #[core::prelude::v1::test]
    fn overlay_visibility_only_flips_at_the_edges() {
        let mut hub = hub();
        assert!(hub.set_overlay_visible(true), "第一次露出浮层");
        assert!(!hub.set_overlay_visible(true), "还是开着，不算翻转");
        assert!(hub.overlay_visible());
        assert!(hub.set_overlay_visible(false), "关掉算翻转");
        assert!(!hub.overlay_visible());
        assert!(!hub.set_overlay_visible(false), "已经关了，不算翻转");
    }
}

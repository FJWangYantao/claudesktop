//! 应用根视图 `ChatApp`：会话管理、内核事件分发与整体布局。
//! 各区域的渲染拆在子模块里（都是 `impl ChatApp`）。

mod catalog_view;
mod chat;
mod chrome;
mod complete_view;
mod composer;
mod content;
mod gauge;
mod history;
pub(crate) mod keys;
mod mascot;
pub mod math;
mod mcp_view;
mod memory_view;
mod menu;
mod motion;
mod notify;
mod overview;
mod palette;
mod plugin_versions;
mod plugins_view;
mod project_view;
mod raw_request;
pub(crate) mod remote_image;
mod requests_view;
mod review;
mod rewind;
mod search;
mod settings_kit;
mod settings_view;
mod shortcuts;
mod sidebar;
mod status;
mod subagent_view;
mod suggest;
mod usage_view;
mod widgets;
mod worktree;
mod worktree_setup;

use crate::demo::*;
use crate::model::*;
use crate::session::*;
use crate::settings::*;
use crate::theme::rgb;
use crate::theme::*;
use crate::usage::{self, TurnStat};
use claudesktop_builtin::BuiltinKernel;
use claudesktop_core::{
    AcpKernel, AcpKernelConfig, EventSink, Kernel, KernelEvent, PromptImage, Record, RoutedEvent,
    SessionKind, SessionSpec, SessionStore, UserPrompt,
};
use content::ContentView;
use futures::StreamExt;
use futures::channel::mpsc::UnboundedSender;
use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::slider::{SliderEvent, SliderState};
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use settings_view::SettingsForm;
use sidebar::{SharedSpots, SidebarSpinners, SidebarView};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// 启动时最多恢复的历史会话数（最近更新的优先）。
const MAX_RESTORED: usize = 100;

pub struct ChatApp {
    input: Entity<TextareaState>,
    /// 输入框待发送的图片附件（粘贴 / 选择文件加入，随下一条消息一起发出）
    pub(crate) pending: Vec<PendingImage>,
    /// 聊天会话待发送的文本附件（内容内联进消息，代码会话不用这个；见 `crates/core/src/attach.rs`）
    pub(crate) pending_files: Vec<claudesktop_core::attach::TextAttachment>,
    /// 正从系统往会话区拖文件（盖一层「松开添加」的提示，见 `composer.rs` 的 `render_drop_hint`）
    file_drag: bool,
    /// 内核会话（可多个）；至少有一个，`active` 始终有效
    sessions: Vec<Session>,
    active: usize,
    next_token: u64,
    /// 会话持久化；None = 不落盘（演示回放模式或数据目录不可用）
    store: Option<SessionStore>,
    /// 默认工作目录（新会话用）
    cwd: PathBuf,
    /// 当前项目名（取工作目录名），侧栏分组名与输入框标签用
    project_name: SharedString,
    overview_tab: usize, // 0 概览 / 1 模型 / 2 请求
    /// 首页「请求」页签：读到的用量网关日志、筛选、点开的轮
    requests_view: requests_view::RequestsView,
    overview_range: usize, // 0 全部 / 1 30 天 / 2 7 天
    /// 用量页的状态（筛选在 `requests_view` 上）
    usage_page: usage_view::UsagePage,
    sidebar_open: bool,
    /// 侧栏会话按最近活动排序（否则按打开顺序）
    sessions_by_activity: bool,
    /// 侧栏里正在重命名的会话（令牌）
    renaming: Option<u64>,
    rename_input: Entity<InputState>,
    /// 重命名框真正拿到过焦点（之后失焦才算提交；刚打开时的失焦事件不算）
    rename_focused: bool,
    /// 输入框的 `/指令`、`@文件` 补全（见 `complete_view.rs`）
    completion: complete_view::CompletionState,
    /// 输入历史（发出去的消息，输入框里上下键回溯，见 `history.rs`）
    history: history::InputHistory,
    /// 侧栏里等待确认删除的会话（令牌）
    confirm_delete: Option<u64>,
    /// 内核槽，按 [`KernelChoice::key`] 找：用到时才启动，可以同时在线，会话各自绑定一个
    kernels: Vec<(String, KernelSlot)>,
    /// 事件从哪条内核来（内核 key：`builtin` / `demo` / `acp:<档案 ID>`）
    ev_tx: UnboundedSender<(String, u64, RoutedEvent)>,
    /// 环境变量给的 ACP 基础配置（工作目录、调试开关）
    acp_base: AcpKernelConfig,
    /// 生效中的设置（启动时已合并环境变量）
    settings: Settings,
    settings_path: Option<PathBuf>,
    settings_open: bool,
    /// 设置页的焦点锚（设置页没有输入框可以兜底，快捷键靠它留在 `ChatApp` 键上下文里）
    settings_focus: FocusHandle,
    /// 下拉菜单打开时的焦点（同一时间只开一个菜单，共用；见 menu.rs）
    menu_focus: FocusHandle,
    settings_form: SettingsForm,
    /// 正在后台获取模型列表
    fetching_models: bool,
    /// 本次启动已为上下文窗口大小悄悄取过一次模型列表（见 `probe_context_window`）
    context_probed: bool,
    /// 刚点了「复制」的地方（会话令牌, 记录下标, 代码块在 Markdown 里的起点；整条消息为 None），
    /// 1.5 秒内按钮显示「已复制」
    copied: Option<(u64, usize, Option<usize>)>,
    /// 点「复制」的时刻（对勾的入场动画）
    copied_at: Option<Instant>,
    /// 鼠标停在对话里第几条消息上（当前会话；消息下方的操作按钮淡入淡出）
    hovered_msg: Option<usize>,
    /// 「回到底部」的平滑滚动进行中（见 `chat.rs` 的 `step_jump_to_bottom`）
    jump: Option<motion::JumpAnim>,
    /// 所有会话的每轮统计（首页用量概览）
    usage: Vec<TurnStat>,
    /// 压测钩子：每次 render 的时刻（None = 不记录）
    pub(crate) frame_log: Option<Vec<std::time::Instant>>,
    /// 压测钩子：每帧从 render 开始到整棵树画完用了多少毫秒（只在记 `frame_log` 时记）
    frame_work: std::rc::Rc<std::cell::RefCell<Vec<f64>>>,
    /// 窗口在前台：激活且没最小化（不在前台时才发系统通知，见 `notify.rs`）
    window_active: bool,
    /// 发出去还没撤的系统通知标签（回到窗口时撤掉）
    notified: Vec<SharedString>,
    /// 本窗口（推应用内提示用，见 `toast`）
    window_handle: AnyWindowHandle,
    /// 设置页的字号滑杆（拖动时即时生效）
    pub(crate) font_slider: Entity<SliderState>,
    /// 系统「动画效果」的最近一次读数（gpui-base 写进减少动态效果开关的值，见 `apply_motion_pref`）
    system_reduce_motion: bool,
    /// 侧栏的缓存 view 与它上次画时的指纹（见 `sidebar.rs` 的 `sync_sidebar`）
    sidebar_view: Entity<SidebarView>,
    sidebar_key: u64,
    /// 侧栏里运行中会话的转圈位置（侧栏记、叠在上面的 `SidebarSpinners` 画，见 `sidebar.rs`）
    sidebar_spinners: SharedSpots,
    sidebar_spinners_view: Entity<SidebarSpinners>,
    /// 输入框上方的内容区（缓存 view，`ChatApp` 每次通知都让它重画，见 `content.rs`）
    content_view: Entity<ContentView>,
    /// 内容区里的转圈 / 呼吸点 / 扫光叠在这上面画（见 motion.rs 文件头）
    loop_overlay: Entity<motion::LoopOverlay>,
    /// 首页滚动区内容的定宽（见 `MeasuredWidth`）
    home_w: widgets::MeasuredWidth,
    /// 首页整页的滚动位置（「请求」页签据此只排视口附近的几轮）
    home_scroll: ScrollHandle,
    /// 子任务详情页的滚动位置（每次打开从顶上看起）
    task_scroll: ScrollHandle,
    /// 设置页右侧内容（和用量页）的滚动位置（用量页的请求明细同样只排视口附近的几轮）
    settings_scroll: ScrollHandle,
    /// 侧栏会话列表的滚动位置（底部渐隐跟手，见 `ScrollEdge`）
    sidebar_scroll: ScrollHandle,
    /// macOS 红绿灯上次按哪个根字号摆的（字号变了才重摆，见 `chrome::traffic_light_origin`）
    #[cfg(target_os = "macos")]
    traffic_lights_rem: Option<Pixels>,
    /// 鼠标停在侧栏哪个会话上（令牌；行右侧的操作按钮淡入淡出）
    sidebar_hover: Option<u64>,
    /// 侧栏只列哪些会话（全部 / 进行中 / 待查看）
    sidebar_filter: sidebar::SidebarFilter,
    /// 侧栏里点了「显示其余 N 个」的组（`SessionGroup::key`）。只在内存里，重启回到只列最近几个
    sidebar_expanded: std::collections::HashSet<String>,
    /// 插件目录里的全部插件（启动时和设置页「重新扫描」时读；启用哪些见 `enabled_plugins`）
    plugins: Vec<claudesktop_core::Plugin>,
    /// 插件页的安装框和进行中的 git 操作
    plugins_form: plugins_view::PluginsForm,
    /// 插件卡片上版本菜单的状态（读到的 tag 列表、正在换到哪一版）
    plugin_versions: plugin_versions::VersionForm,
    mcp_form: mcp_view::McpForm,
    /// 设置页「记忆」：看哪一栏、搜索、读到的两份记忆条目
    memory_form: memory_view::MemoryForm,
    /// 内核提炼出来的「待确认」记忆（收下才落盘；只在内存里，重启就没了）
    memory_candidates: memory_view::MemoryCandidates,
    /// 设置页「项目」：项目根、表单值、读文件时的问题
    project_form: project_view::ProjectForm,
    /// 输入框当前的占位文字（首页是建议的提示词，见 `overview.rs` 的 `sync_placeholder`）
    placeholder: SharedString,
    /// 「回退到这里」浮层开着时算好的预览（打开时算一次，见 rewind.rs）
    rewind: Option<rewind::RewindPreview>,
    /// 浮层里「冲突的文件也覆盖」勾上了
    rewind_overwrite: bool,
    /// 审查页（最新一轮的逐块撤回，见 review.rs）：打开时算好；None = 没开
    review: Option<review::ReviewPage>,
    review_scroll: ScrollHandle,
    /// 命令面板里按正文搜到的消息（查询变了才重搜，见 search.rs）
    palette_hits: Vec<search::MessageHit>,
    /// 磁盘上搜到的命中（更早的会话，没载入内存；见 `search::scan_history`）
    palette_history: Vec<search::HistoryHitState>,
    /// 上一次（已排上节拍的）搜索关键词，小写、去过首尾空白
    palette_query: String,
    /// 面板每关 / 开一次 +1：排在节拍里的搜索回来时靠它判断结果是否已经过期
    palette_generation: u64,
    /// 待执行的那次搜索（防抖用的定时任务，见 `ChatApp::search_palette`）
    palette_timer: Option<Task<()>>,
    /// 刚从搜索结果跳过去的消息：(会话令牌, 行号, 跳过去的时刻)，那一行闪一下底色
    flash: Option<(u64, usize, Instant)>,
    /// 输入框的 Tab 补全（模型猜的下文，灰字画在光标后面；见 suggest.rs）
    suggestion: suggest::SuggestionState,
    /// 一轮结束后猜的「下一句」（淡色画在空输入框里，按 Tab 填入；见 suggest.rs）
    next_suggestion: suggest::NextSuggestion,
    /// 每个空间上次看的是哪个会话（[0] 代码 / [1] 聊天；按 `Session::token` 记，
    /// 删会话不会错位）。切空间时回到它，见 `ChatApp::switch_space`
    last_active: [Option<u64>; 2],
}

/// 一种内核的运行状态。
#[derive(Default)]
pub(crate) struct KernelSlot {
    /// None = 演示回放、还没启动或启动失败
    kernel: Option<Arc<dyn Kernel>>,
    /// 启动过（失败也算；改了设置才会重试）
    started: bool,
    /// 代号：每次（重新）启动 +1，事件带着代号回来，不是当前代号的丢弃
    generation: u64,
    /// 与内核的连接已建立（可以创建会话）
    ready: bool,
    error: bool,
    status: SharedString,
    /// 内核报的致命错误原文（起不来、agent 退出）；重新启动或连上后清掉
    fatal: Option<String>,
    /// 内核标识（写进会话元信息，续接时比对）
    agent_cmd: String,
    /// MCP 服务器的状态（只有自研内核有；设置页「MCP」显示）
    mcp: Vec<claudesktop_core::McpServerView>,
    /// 后台命令（只有自研内核有；会话顶栏显示运行中的）
    tasks: Vec<claudesktop_core::BackgroundTaskView>,
}

impl Drop for ChatApp {
    fn drop(&mut self) {
        for slot in &self.kernels {
            if let Some(kernel) = &slot.1.kernel {
                kernel.shutdown();
            }
        }
    }
}

/// 从聊天复制到 Code 会话时只抄这些记录：对话本身。
/// 文件改动 / 检查点 / 回退不抄（新会话的目录里没有那些文件，抄过去回退会乱）。
fn copyable(record: &Record) -> bool {
    matches!(
        record,
        Record::User { .. }
            | Record::Event { .. }
            | Record::PermissionResolved { .. }
            | Record::Turn { .. }
            | Record::Thought { .. }
    )
}

/// 把一张图片附件拷进会话 `id` 的资产目录，返回新路径；原文件已经不在了返回 None。
fn copy_image(store: &SessionStore, id: &str, path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("png");
    let copied = store.save_image(id, ext, &bytes).ok()?;
    Some(copied.to_string_lossy().into_owned())
}

impl ChatApp {
    /// 按槽的 key 找（只读）。
    pub(crate) fn slot(&self, key: &str) -> Option<&KernelSlot> {
        self.kernels.iter().find(|(k, _)| k == key).map(|(_, s)| s)
    }

    /// 按槽的 key 找（可改）。
    pub(crate) fn slot_mut(&mut self, key: &str) -> Option<&mut KernelSlot> {
        self.kernels.iter_mut().find(|(k, _)| k == key).map(|(_, s)| s)
    }

    /// 找不到就补一个空槽（懒启动的入口）。
    fn slot_entry(&mut self, key: &str) -> &mut KernelSlot {
        if !self.kernels.iter().any(|(k, _)| k == key) {
            self.kernels.push((key.to_string(), KernelSlot::default()));
        }
        self.slot_mut(key).expect("刚补过")
    }

    pub(crate) fn kernel_of(&self, kind: &KernelChoice) -> Option<Arc<dyn Kernel>> {
        self.kernel_by_key(&kind.key())
    }

    pub(crate) fn kernel_by_key(&self, key: &str) -> Option<Arc<dyn Kernel>> {
        self.slot(key).and_then(|s| s.kernel.clone())
    }

    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // 多行、至少两行高、随内容长高（最多 10 行后出滚动条）；回车发送，Shift+回车换行
        let input =
            cx.new(|cx| TextareaState::new(window, cx).auto_grow(2, 10).submit_on_enter(true));

        cx.subscribe_in(
            &input,
            window,
            |this: &mut ChatApp,
             _entity: &Entity<TextareaState>,
             event: &InputEvent,
             window: &mut Window,
             cx: &mut Context<ChatApp>| {
                match event {
                    // Shift+回车已由输入框插入换行，同样会发这个事件，要排除
                    InputEvent::PressEnter { shift: false, .. } => this.send(window, cx),
                    InputEvent::PressEnter { .. } => {}
                    // 打字时刷新 `/`、`@` 补全。只重画输入区，不碰对话（见 content.rs）
                    InputEvent::Change => {
                        // 用户改了内容：退出历史回溯，别让下一条历史把新写的字冲掉
                        this.history.touch();
                        this.update_completion(cx);
                        // 停顿一下就猜一句下文（见 suggest.rs）
                        this.schedule_suggestion(cx);
                        this.refresh_composer(cx);
                    }
                    // 发送按钮随输入框空 / 非空换样式；输入框卡片随聚焦换边框
                    InputEvent::Focus | InputEvent::Blur => this.refresh_composer(cx),
                }
            },
        )
        .detach();
        // 快捷键挂在根视图的键上下文上，要求焦点在界面里：启动时给输入框，
        // 聚焦的元素被移出界面时（关设置页、重命名提交、菜单关闭）再找回来
        input.update(cx, |state, cx| state.focus(window, cx));
        cx.on_focus_lost(window, |this: &mut ChatApp, window, cx| this.restore_focus(window, cx))
            .detach();
        // 外观跟随系统时，系统切深浅色要跟着变
        cx.observe_window_appearance(window, |this: &mut ChatApp, window, cx| {
            this.apply_appearance(window, cx)
        })
        .detach();

        // 内核与配置：设置文件 + 环境变量覆盖（见 settings.rs）；
        // 每次（重新）启动内核都换一个代号，旧内核的迟到事件按代号丢弃
        let acp_base = AcpKernelConfig::from_env();
        let settings_path = Settings::default_path();
        let (settings, settings_error) = Settings::load(settings_path.as_deref());
        let settings = settings.with_env();
        claudesktop_builtin::request_bodies::set_enabled(settings.gateway_bodies);
        // 回复里的网络图片：默认不自动加载（隐私，见 remote_image.rs），这里把设置同步过去
        remote_image::set_auto_load(settings.auto_load_images);
        // 默认工作目录：环境变量 ACP_CWD 优先，其次上次选的（目录还在的话），最后是启动目录
        let cwd = settings
            .workspace
            .clone()
            .filter(|dir| std::env::var_os("ACP_CWD").is_none() && dir.is_dir())
            .unwrap_or_else(|| acp_base.cwd.clone());
        let project_name = project_label(&cwd);

        let (ev_tx, mut ev_rx) = futures::channel::mpsc::unbounded::<(String, u64, RoutedEvent)>();
        cx.spawn(async move |this, cx| {
            while let Some((key, generation, event)) = ev_rx.next().await {
                if this
                    .update(cx, |app, cx| {
                        // 迟到事件：槽不在了（档案停用）或代号对不上都丢掉
                        if let Some(slot) = app.slot(&key)
                            && generation == slot.generation
                        {
                            app.on_kernel_event(key, event, cx);
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        // 运行状态行计时：当前会话忙时每秒重绘一次（等授权时没有转圈动画驱动重绘）
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let alive = this.update(cx, |app, cx| {
                    if app.session().busy {
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();

        // 侧栏重命名框：回车或失焦即提交
        let rename_input = cx.new(|cx| InputState::new(window, cx));
        cx.subscribe_in(
            &rename_input,
            window,
            |this: &mut ChatApp, _: &Entity<InputState>, event: &InputEvent, _, cx| match event {
                InputEvent::Focus => this.rename_focused = true,
                InputEvent::PressEnter { .. } => this.commit_rename(cx),
                InputEvent::Blur if this.rename_focused => this.commit_rename(cx),
                _ => {}
            },
        )
        .detach();

        let settings_form = SettingsForm::new(window, cx);
        let plugins_form = plugins_view::PluginsForm::new(window, cx);
        let mcp_form = mcp_view::McpForm::new(window, cx);
        let memory_form = memory_view::MemoryForm::new(window, cx);
        let project_form = project_view::ProjectForm::new(window, cx);
        // 字号滑杆：85% ~ 140%，步进 5%
        let font_slider = cx.new(|_| {
            SliderState::new()
                .min(FONT_SCALE_MIN * 100.)
                .max(FONT_SCALE_MAX * 100.)
                .step(FONT_SCALE_STEP)
                .default_value(settings.font_scale * 100.)
        });
        // 字号滑杆：事件由 SliderState 自己 emit（组件没有 on_change 链式回调）
        cx.subscribe(&font_slider, |this: &mut ChatApp, state, event: &SliderEvent, cx| {
            let value = match event {
                SliderEvent::Change(v) | SliderEvent::Release(v) => *v,
            };
            let scale = value.end() / 100.;
            // 拖动时值经常没变，不必每帧写文件
            if (scale - this.settings.font_scale).abs() > 1e-4 {
                this.set_font_scale(scale, cx);
            }
            let _ = state;
        })
        .detach();

        let weak = cx.entity().downgrade();
        let mut app = Self {
            input,
            pending: Vec::new(),
            pending_files: Vec::new(),
            file_drag: false,
            sessions: Vec::new(),
            active: 0,
            next_token: 1,
            store: None,
            cwd,
            project_name,
            overview_tab: 0,
            requests_view: Default::default(),
            overview_range: 0,
            usage_page: Default::default(),
            sidebar_open: true,
            sessions_by_activity: false,
            renaming: None,
            rename_input,
            rename_focused: false,
            completion: Default::default(),
            history: Default::default(),
            confirm_delete: None,
            kernels: Default::default(),
            ev_tx,
            acp_base,
            settings,
            settings_path,
            settings_open: false,
            settings_focus: cx.focus_handle(),
            menu_focus: cx.focus_handle(),
            settings_form,
            fetching_models: false,
            context_probed: false,
            copied: None,
            copied_at: None,
            hovered_msg: None,
            jump: None,
            usage: Vec::new(),
            frame_log: None,
            frame_work: Default::default(),
            window_active: true,
            notified: Vec::new(),
            window_handle: window.window_handle(),
            font_slider,
            // gpui_kit::init 已按系统「动画效果」写好开关；之后按设置页的「动画」覆盖
            system_reduce_motion: cx.reduce_motion(),
            sidebar_view: cx.new(|_| SidebarView::new(weak.clone())),
            sidebar_key: 0,
            sidebar_spinners: Default::default(),
            sidebar_spinners_view: cx.new(|_| SidebarSpinners::new(weak.clone())),
            content_view: cx.new(|_| ContentView::new(weak.clone())),
            loop_overlay: cx.new(|_| motion::LoopOverlay),
            home_w: Default::default(),
            home_scroll: ScrollHandle::new(),
            task_scroll: ScrollHandle::new(),
            settings_scroll: ScrollHandle::new(),
            sidebar_scroll: ScrollHandle::new(),
            #[cfg(target_os = "macos")]
            traffic_lights_rem: None,
            sidebar_hover: None,
            sidebar_filter: Default::default(),
            sidebar_expanded: Default::default(),
            plugins_form,
            plugin_versions: Default::default(),
            mcp_form,
            memory_form,
            memory_candidates: Default::default(),
            project_form,
            plugins: claudesktop_core::plugin::default_plugins_dir()
                .map(|d| claudesktop_core::plugin::discover(&d))
                .unwrap_or_default(),
            placeholder: SharedString::default(),
            rewind: None,
            rewind_overwrite: false,
            review: None,
            review_scroll: ScrollHandle::new(),
            palette_hits: Vec::new(),
            palette_history: Vec::new(),
            palette_query: String::new(),
            palette_generation: 0,
            palette_timer: None,
            flash: None,
            suggestion: suggest::SuggestionState::default(),
            next_suggestion: suggest::NextSuggestion::default(),
            last_active: [None, None],
        };
        // 内容区缓存的失效规则：`ChatApp` 有任何变化都让它重画（只动输入区的走 `refresh_composer`）
        cx.observe_self(|this: &mut ChatApp, cx| this.content_view.update(cx, |_, cx| cx.notify()))
            .detach();
        // 第一个会话用当前空间的内核：连接就绪（Connected）后统一补发 session/new；
        // 其他内核等有会话用到时再启动
        let space = app.settings.space;
        let sk = space.kind();
        let (kind, dir) = app.space_defaults(space);
        app.open_session_of(kind, dir, sk);
        app.remember_space(space);
        // 启动时的问题不自动消失：用户可能还没看窗口
        if let Some(err) = settings_error {
            app.toast(Notification::error(err).autohide(false), cx);
        }
        // 从磁盘恢复历史会话（演示回放模式下新会话不落盘，但历史照常显示）
        match SessionStore::open_default() {
            Ok(store) => {
                // 最近的进侧栏；更早的只读进用量统计
                for (i, meta) in store.list().into_iter().enumerate() {
                    let records = store.load(&meta.id).unwrap_or_default();
                    app.usage.extend(usage::collect(&meta, &records));
                    if i < MAX_RESTORED {
                        app.sessions.push(Session::from_records(
                            app.next_token,
                            meta,
                            records,
                            &app.settings.acp,
                        ));
                        app.next_token += 1;
                    }
                }
                app.store = Some(store);
            }
            Err(err) => {
                app.toast(Notification::error(format!("会话历史不可用：{err}")).autohide(false), cx)
            }
        }
        app.apply_appearance(window, cx);
        app.apply_motion_pref(cx);
        app.init_notifications(window, cx);
        // 启动时的会话里，worktree 目录没了的（用户删了 / 换了电脑）：现在就把分支接回来
        for idx in 0..app.sessions.len() {
            if ChatApp::worktree_is_missing(&app.sessions[idx]) {
                app.repair_worktree(idx, cx);
            }
        }
        app
    }

    /// 按当前设置启动某种内核（不处理会话，见 [`Self::restart_kernel`]）。
    fn spawn_kernel(&mut self, kind: &KernelChoice) {
        let key = kind.key();
        let slot = self.slot_entry(&key);
        slot.started = true;
        slot.generation += 1;
        let generation = slot.generation;
        let ev_key = key.clone();
        let tx = self.ev_tx.clone();
        let sink: EventSink = Arc::new(move |event| {
            let _ = tx.unbounded_send((ev_key.clone(), generation, event));
        });
        // 启用的插件：自研内核启动时就带上（MCP 只连一次），ACP 内核起来后交给它
        let plugins = self.enabled_plugins();
        let (kernel, status, agent_cmd): (Option<Arc<dyn Kernel>>, SharedString, String) =
            match kind {
                KernelChoice::Demo => (None, "未连接内核".into(), String::new()),
                KernelChoice::Builtin => {
                    match self.settings.builtin_config(self.cwd.clone()).and_then(|mut config| {
                        config.plugins = plugins;
                        let identity = config.identity();
                        BuiltinKernel::spawn(config, sink).map(|k| (k, identity))
                    }) {
                        Ok((k, identity)) => {
                            (Some(Arc::new(k) as Arc<dyn Kernel>), "内核启动中…".into(), identity)
                        }
                        Err(err) => (None, format!("启动失败：{err}").into(), String::new()),
                    }
                }
                KernelChoice::Acp(id) => match self.settings.acp.get(id).cloned() {
                    // 档案停用 / 删掉了：别退回 `acp_base` 的命令（那是 ACP_AGENT 或仓库里的 mock agent）
                    None => (
                        None,
                        "这个 agent 已停用：到设置「本机 agent」里启用，或换个内核".into(),
                        String::new(),
                    ),
                    Some(profile) => {
                        let config =
                            self.settings.acp_config_for(self.acp_base.clone(), Some(&profile));
                        let agent_id = crate::settings::acp_agent_id(id, &config.command);
                        match AcpKernel::spawn(config, sink) {
                            Ok(k) => {
                                k.set_plugins(plugins);
                                (
                                    Some(Arc::new(k) as Arc<dyn Kernel>),
                                    "内核启动中…".into(),
                                    agent_id,
                                )
                            }
                            Err(err) => (None, format!("启动失败：{err}").into(), agent_id),
                        }
                    }
                },
            };
        let slot = self.slot_entry(&key);
        slot.ready = kind.is_demo();
        slot.error = kernel.is_none() && !kind.is_demo();
        slot.kernel = kernel;
        slot.status = status;
        slot.agent_cmd = agent_cmd;
        slot.fatal = None;
        slot.mcp.clear();
    }

    /// 还没启动过就启动。
    fn ensure_kernel(&mut self, kind: &KernelChoice) {
        let key = kind.key();
        if !self.slot(&key).is_some_and(|s| s.started) {
            self.spawn_kernel(kind);
        }
    }

    /// 某种内核的设置改了：关掉旧的，绑在它上面的会话全部脱离；
    /// 有对话的会话下次发消息时按「历史会话」续接，空会话等新连接就绪后重新创建。
    /// 没启动过的内核什么也不做（用到时自然按新设置启动）。
    pub(crate) fn restart_kernel(&mut self, kind: &KernelChoice, cx: &mut Context<Self>) {
        let key = kind.key();
        if !self.slot(&key).is_some_and(|s| s.started) {
            return;
        }
        let old = self.slot_mut(&key).and_then(|s| s.kernel.take());
        for s in self.sessions.iter_mut().filter(|s| &s.kernel == kind) {
            s.detach_kernel(old.as_ref());
        }
        if let Some(old) = old {
            old.shutdown();
        }
        self.spawn_kernel(kind);
        self.request_pending_sessions();
        cx.notify();
    }

    /// 插件卡片上版本菜单的状态（读到的 tag 列表、正在换到哪一版）
    pub(super) fn version_form(&self) -> &plugin_versions::VersionForm {
        &self.plugin_versions
    }

    /// 第 `idx` 个会话绑定的内核
    pub(crate) fn session_kernel(&self, idx: usize) -> Option<Arc<dyn Kernel>> {
        self.kernel_of(&self.sessions[idx].kernel)
    }

    /// 直接关掉一个内核槽（档案被停用 / 删除时）：绑在它上面的会话全部脱离。
    pub(crate) fn shutdown_slot(&mut self, key: &str) {
        let old = self.slot_mut(key).and_then(|s| s.kernel.take());
        if let Some(slot) = self.slot_mut(key) {
            slot.ready = false;
            slot.started = false;
            slot.status = "未连接内核".into();
        }
        for s in self.sessions.iter_mut().filter(|s| s.kernel_key == key) {
            s.detach_kernel(old.as_ref());
        }
        if let Some(old) = old {
            old.shutdown();
        }
    }

    /// 停掉一条后台命令（顶栏的小标点的）。
    pub(crate) fn stop_background(&mut self, id: &str, cx: &mut Context<Self>) {
        let kind = self.sessions[self.active].kernel.clone();
        if let Some(kernel) = self.kernel_of(&kind)
            && kernel.kind() == "builtin"
        {
            kernel.stop_background_task(id);
        }
        cx.notify();
    }

    /// 系统已装字体族（设置页的字体下拉用）。枚举一次要上百毫秒，缓存在 `theme` 里。
    pub(crate) fn system_fonts(&self) -> &'static [String] {
        crate::theme::cached_fonts()
    }

    pub(crate) fn session(&self) -> &Session {
        &self.sessions[self.active]
    }

    /// 当前空间（侧栏顶部「聊天 | 代码」）。
    pub(crate) fn space(&self) -> Space {
        self.settings.space
    }

    /// 聊天会话的占位目录：数据目录下的 `chat/`（建好、空着）。
    /// 漏网的相对路径落进这里而不是用户的启动目录（见「设计说明-聊天与Code分离.md」第三章）。
    pub(crate) fn chat_dir(&self) -> PathBuf {
        claudesktop_core::default_data_dir()
            .map(|d| d.join("chat"))
            .unwrap_or_else(|_| self.cwd.join(".chat"))
    }

    /// 新会话跑在哪个目录：聊天是占位目录，其余是当前工作目录。
    fn new_session_dir(&self, kind: SessionKind) -> PathBuf {
        match kind {
            SessionKind::Chat => self.chat_dir(),
            SessionKind::Code => self.cwd.clone(),
        }
    }

    /// 某个空间新会话的内核和目录：聊天空间只能是自研内核、目录是占位目录。
    fn space_defaults(&self, space: Space) -> (KernelChoice, PathBuf) {
        (self.settings.kernel_for_space(space), self.new_session_dir(space.kind()))
    }

    /// 某个空间在 `last_active` 里的下标（[0] 代码 / [1] 聊天）。
    fn space_slot(space: Space) -> usize {
        match space {
            Space::Code => 0,
            Space::Chat => 1,
        }
    }

    /// 记下某个空间此刻看的是哪个会话（按令牌记，删会话不会错位）。
    fn remember_space(&mut self, space: Space) {
        let token = self.sessions[self.active].token;
        self.last_active[Self::space_slot(space)] = Some(token);
    }

    /// 切空间（侧栏顶部「聊天 | 代码」）：回到那个空间上次看的会话，
    /// 没有就开一个空会话；空间本身写回设置，下次启动回到这里。
    pub(crate) fn switch_space(&mut self, space: Space, cx: &mut Context<Self>) {
        if space == self.space() {
            return;
        }
        self.remember_space(self.space());
        self.settings.space = space;
        self.update_settings(|s| s.space = space);
        let token = self.last_active[Self::space_slot(space)];
        match token.and_then(|t| self.sessions.iter().position(|s| s.token == t)) {
            Some(idx) => self.activate_session(idx, cx),
            None => {
                // 那个空间还没有会话：开着设置页就先不建（首页在那儿），只换空间
                self.open_in_space(space, cx);
            }
        }
        cx.notify();
    }

    /// 「在项目里继续…」（见设计说明第八章）：把当前聊天**复制**成一个 Code 会话。
    ///
    /// 新会话拿一份新的日志（新 ID、`kind: Code`、选的那个目录），内容原样抄过来；
    /// 自研内核续接时按日志重建上下文，所以它「记得」前面聊过什么，而且现在有了工具。
    /// 原来的聊天不动（转换会让原聊天消失，复制干净）。
    pub(crate) fn continue_in_dir(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        let src = self.active;
        let Some(store) = self.store.as_ref() else {
            self.toast(Notification::warning("数据目录不可用，没法把这段聊天接到项目里"), cx);
            return;
        };
        let records = match &self.sessions[src].meta {
            Some(meta) => store.load(&meta.id).unwrap_or_default(),
            // 还没落过盘（一个字都没发）：没有可复制的东西
            None => Vec::new(),
        };
        // 先折算回退（回退掉的轮次不能在新会话里复活），再只抄对话本身：
        // 文件改动 / 检查点 / 回退不抄（新目录里没有那些文件，抄过去回退会乱）
        let records: Vec<Record> =
            claudesktop_core::effective(&records).into_iter().filter(copyable).collect();
        let title = format!("{}（接着聊）", self.sessions[src].title);
        let kind = self.settings.kernel_for_space(Space::Code);
        let agent_cmd = self.slot(&kind.key()).map(|s| s.agent_cmd.clone()).unwrap_or_default();
        let created =
            store.create(&title, dir.clone(), &agent_cmd).and_then(|(mut meta, mut log)| {
                meta.kind = SessionKind::Code;
                meta.task = false;
                store.save_meta(&meta)?;
                for record in records {
                    let record = match record {
                        // 图片附件拷进新会话自己的资产目录：删掉原聊天时会连资产一起删。
                        // 已经不在的去掉
                        Record::User { text, images, sent } => Record::User {
                            text,
                            images: images
                                .iter()
                                .filter_map(|p| copy_image(store, &meta.id, Path::new(p)))
                                .collect(),
                            sent,
                        },
                        other => other,
                    };
                    log.append(record)?;
                }
                Ok(meta)
            });
        let meta = match created {
            Ok(meta) => meta,
            Err(err) => {
                self.toast(Notification::error(format!("没能接到项目里：{err}")), cx);
                return;
            }
        };
        // 在代码空间里开出来并切过去（新会话要先续接内核、重建上下文）
        self.remember_space(self.space());
        self.settings.space = Space::Code;
        self.update_settings(|s| s.space = Space::Code);
        let idx = self.open_session_of(kind, dir.clone(), SessionKind::Code);
        self.sessions[idx].meta = Some(meta);
        self.sessions[idx].title = title.into();
        self.sessions[idx].restored = true;
        self.sessions[idx].renamed = true;
        self.sessions[idx].note(format!(
            "从聊天接过来的（目录：{}）。前面聊过的内容都在上下文里，现在可以动文件、跑命令了",
            dir.display()
        ));
        self.active = idx;
        self.remember_space(Space::Code);
        self.suggestion.clear();
        cx.notify();
    }

    /// 在某个空间里开一个新会话（用该空间的默认内核和目录），并切过去。
    fn open_in_space(&mut self, space: Space, cx: &mut Context<Self>) {
        let (kind, cwd) = self.space_defaults(space);
        let idx = self.open_session_of(kind, cwd, space.kind());
        self.active = idx;
        self.remember_space(space);
        self.refresh_composer(cx);
    }

    /// 命令面板的「新建聊天 / 新建代码会话」：先切到那个空间，再按那个空间的规矩新建
    ///（已经有空会话就切过去，见 `add_session`）。
    pub(crate) fn new_in_space(&mut self, space: Space, cx: &mut Context<Self>) {
        if space != self.space() {
            self.switch_space(space, cx);
        }
        self.add_session(cx);
    }

    /// 新建一个会话并切过去（在当前空间里：聊天空间建聊天，代码空间按设置的默认内核）。
    pub(crate) fn add_session(&mut self, cx: &mut Context<Self>) {
        let space = self.space();
        let kind_of_session = space.kind();
        let (kind, cwd) = self.space_defaults(space);
        // 已经有一个还没开始的空会话（启动时开的、刚点过的）就切过去，别在侧栏堆一串「新会话」；
        // 它要是建过一个没动过的 worktree，顺手清掉（见 worktree.rs）
        let blank = self.sessions.iter().position(|s| {
            !s.has_user_message()
                && !s.busy
                && !s.renamed
                && s.kernel == kind
                && s.cwd == cwd
                && s.kind == kind_of_session
        });
        if let Some(idx) = blank {
            self.cleanup_worktree(idx);
        }
        self.active = match blank {
            Some(idx) => idx,
            None => self.open_session_of(kind, cwd, kind_of_session),
        };
        self.remember_space(space);
        // 换会话了：上一条对话猜的灰字作废
        self.suggestion.clear();
        cx.notify();
    }

    /// 在某种内核上开一个 Code 会话，返回下标（`rerun_on` / worktree 那几处用）。
    fn open_session(&mut self, kind: KernelChoice, cwd: PathBuf) -> usize {
        self.open_session_of(kind, cwd, SessionKind::Code)
    }

    /// 在某种内核上开一个会话，返回下标。内核没启动就先启动；
    /// 连接已就绪就立刻向内核要会话，否则等 Connected 时补发。
    fn open_session_of(&mut self, kind: KernelChoice, cwd: PathBuf, sk: SessionKind) -> usize {
        // 还没配置时先不起内核，配好保存时再起（见 `save_settings`）
        if !self.settings.needs_setup() {
            self.ensure_kernel(&kind);
        }
        let token = self.next_token;
        self.next_token += 1;
        self.sessions.push(self.new_session_of(kind, cwd, token, sk));
        self.request_pending_sessions();
        self.sessions.len() - 1
    }

    /// 新建一个会话（还没向内核要）。设置里开着「自动」（自研内核）时，新对话默认就用它：
    /// 先改界面上的选中项，内核会话建立（Started）后再交给内核。
    /// 没为 `cwd` 选过文件夹的会话算**任务**（侧栏归到「任务」区，见 `sidebar.rs`）；
    /// 聊天会话没有工作目录，恒为任务之外（`task = false`，侧栏按日期分组）
    fn new_session(&self, kind: KernelChoice, cwd: PathBuf, token: u64) -> Session {
        self.new_session_of(kind, cwd, token, SessionKind::Code)
    }

    fn new_session_of(
        &self,
        kind: KernelChoice,
        cwd: PathBuf,
        token: u64,
        sk: SessionKind,
    ) -> Session {
        let is_builtin = kind == KernelChoice::Builtin;
        let mut session = Session::of_kind(token, kind, cwd.clone(), sk);
        session.task = sk == SessionKind::Code && !self.is_project_dir(&cwd);
        if sk == SessionKind::Chat {
            // 联网默认值取设置里的「默认联网」，用户可以在输入框里随时关
            session.web = self.settings.chat.web;
            session.pending_web = Some(session.web);
        }
        if is_builtin && sk == SessionKind::Code && self.settings.auto_mode_default() {
            session.default_to(MODE_AUTO_ID);
        }
        // agent 档案记着上次用的模型：Started 后 agent 没报模型就切过去
        if let KernelChoice::Acp(id) = &session.kernel {
            session.last_model = self.settings.acp.get(id).and_then(|a| a.last_model.clone());
        }
        session
    }

    /// 这个目录是不是项目：默认目录（环境变量给的或启动目录）以外的都是用户选过的；
    /// 默认目录本身只有在目录菜单里明确选过（记进设置）才算。不是项目的，在里面的对话算任务
    pub(super) fn is_project_dir(&self, dir: &Path) -> bool {
        self.settings.workspace.as_deref() == Some(dir) || dir != self.acp_base.cwd
    }

    /// 当前会话所在的目录是个 git 仓库（目录菜单里据此给「在新 worktree 中开始」）。
    /// 只看有没有 `.git`（目录或 worktree 的文件），不在 render 里跑 git。
    pub(super) fn git_repo_here(&self) -> bool {
        let s = self.session();
        s.worktree.is_none() && s.cwd.join(".git").exists()
    }

    /// 把一个还没对话的会话换到另一种内核上：原地换成新会话，
    /// 旧内核会话的迟到事件因为令牌 / 内核不同会被忽略。
    pub(crate) fn rebind_session(&mut self, idx: usize, kind: KernelChoice) {
        let s = &self.sessions[idx];
        if s.kernel == kind || s.has_user_message() || s.busy {
            return;
        }
        let cwd = s.cwd.clone();
        self.replace_session(idx, kind, cwd);
    }

    /// 第 `idx` 个会话原地换成 `kind` 内核、`cwd` 目录下的新会话（调用方保证它还没对话）。
    fn replace_session(&mut self, idx: usize, kind: KernelChoice, cwd: PathBuf) {
        let old = self.session_kernel(idx);
        self.sessions[idx].cancel_pending_permissions(old.as_ref());
        self.ensure_kernel(&kind);
        let token = self.next_token;
        self.next_token += 1;
        self.sessions[idx] = self.new_session(kind, cwd, token);
        self.request_pending_sessions();
    }

    /// 切换工作目录（输入框上方的目录 chip）：设为新会话的默认目录并记进设置；
    /// 当前会话还没对话就原地换到这个目录，已有对话就在那里开一个新会话（对话中途不换目录）。
    /// 选过文件夹之后，在当前这个目录里的对话不再算「任务」（见 `Session::task`）
    pub(crate) fn switch_workspace(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        self.project_name = project_label(&dir);
        self.cwd = dir.clone();
        self.update_settings(|s| s.workspace = Some(dir.clone()));
        let s = self.session();
        if s.cwd != dir {
            if s.has_user_message() || s.busy {
                self.active = self.open_session(self.settings.kernel(), dir);
            } else {
                let kind = s.kernel.clone();
                self.replace_session(self.active, kind, dir);
            }
        } else if self.sessions[self.active].task {
            // 选的就是它已经在跑的目录：只是从「任务」改成项目，对话不动
            self.sessions[self.active].task = false;
            self.save_task(self.active);
        }
        cx.notify();
    }

    /// 目录菜单里的「不选项目（任务）」：当前对话回到任务区（侧栏下半区）。
    /// 已经跑在默认目录里就只改归类；还没对话就原地换到默认目录，否则在默认目录另开一个（对话中途不换目录）。
    pub(crate) fn as_task(&mut self, cx: &mut Context<Self>) {
        let default_dir = self.acp_base.cwd.clone();
        let s = self.session();
        let kind = s.kernel.clone();
        if s.cwd == default_dir {
            self.sessions[self.active].task = true;
            self.save_task(self.active);
        } else if !s.has_user_message() && !s.busy {
            self.replace_session(self.active, kind, default_dir);
            self.sessions[self.active].task = true;
        } else {
            let idx = self.open_session(kind, default_dir);
            self.sessions[idx].task = true;
            self.active = idx;
        }
        cx.notify();
    }

    /// 把第 `idx` 个会话的「任务」归类写进元信息（还没落盘的会话在建档时按 `Session::task` 写）。
    fn save_task(&mut self, idx: usize) {
        let Some(store) = &self.store else {
            return;
        };
        let s = &mut self.sessions[idx];
        if let Some(meta) = &mut s.meta {
            meta.task = s.task;
            let _ = store.save_meta(meta);
        }
    }

    /// 「在项目里继续…」（顶栏的按钮）：选一个目录，把当前聊天接到那个目录的 Code 会话里。
    pub(crate) fn continue_in_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("选一个项目目录，把这段聊天接过去".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await
                && let Some(dir) = paths.into_iter().next()
            {
                let _ = this.update_in(cx, |app, _, cx| app.continue_in_dir(dir, cx));
            }
        })
        .detach();
    }

    /// 目录菜单里的「选择文件夹…」：系统对话框选一个目录切过去。
    pub(crate) fn pick_workspace(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("选择工作目录".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await
                && let Some(dir) = paths.into_iter().next()
            {
                let _ = this.update(cx, |app, cx| app.switch_workspace(dir, cx));
            }
        })
        .detach();
    }

    /// 目录菜单列出的工作目录：默认目录在前，其余按会话最近活动排，已不存在的略去，最多 8 个。
    /// worktree 会话不列（它们的 `cwd` 是数据目录里的 worktree，列出来是一串看不懂的路径）。
    pub(crate) fn recent_workspaces(&self) -> Vec<PathBuf> {
        // 无元信息的会话（刚开、还没落盘）视为最新；排序稳定，默认目录排在同为最新的前面
        let mut dirs: Vec<(&Path, u64)> = vec![(self.cwd.as_path(), u64::MAX)];
        for s in self.sessions.iter().filter(|s| s.worktree.is_none()) {
            let dir = &s.cwd;
            let at = s.meta.as_ref().map_or(u64::MAX, |m| m.updated_ms);
            match dirs.iter_mut().find(|(d, _)| *d == dir.as_path()) {
                Some((_, last)) => *last = (*last).max(at),
                None => dirs.push((dir.as_path(), at)),
            }
        }
        dirs.sort_by_key(|(_, at)| std::cmp::Reverse(*at));
        dirs.into_iter().map(|(d, _)| d.to_path_buf()).filter(|d| d.is_dir()).take(8).collect()
    }

    /// 切到第 `i` 个会话（侧栏点击 / 快捷键共用）：重开会话的模型名从日志恢复了，
    /// 但窗口大小可能还没缓存过，顺带悄悄探测一次（见 [`Self::probe_context_window`]）。
    pub(crate) fn activate_session(&mut self, i: usize, cx: &mut Context<Self>) {
        self.active = i;
        self.settings_open = false;
        // 从搜索结果、系统通知点进别的空间的会话时自动切过去（空间本来就是它那个）
        let space = Space::of(self.sessions[i].kind);
        if space != self.space() {
            self.settings.space = space;
            self.update_settings(|s| s.space = space);
        }
        self.last_active[Self::space_slot(space)] = Some(self.sessions[i].token);
        self.sessions[i].wheel = None;
        // worktree 目录被用户删了 / 换了电脑：切过来时试着重新检出接上（见 worktree.rs）
        if ChatApp::worktree_is_missing(&self.sessions[i]) {
            self.repair_worktree(i, cx);
        }
        // worktree 会话：切过来时读一次分支现状（后台跑），chip 上才显示「几个文件 / 领先几个」，
        // 主目录那边的变化（换了分支、有了改动）也在这时候反映到菜单里
        if self.sessions[i].worktree.is_some() {
            self.refresh_worktree(i, cx);
        }
        // 换会话了：上一条对话猜的灰字作废，输入框里的字留着，停顿一下会为新会话重猜
        self.suggestion.clear();
        // 猜的「下一句」也一样：那句话是接着另一个对话说的
        self.next_suggestion.clear();
        self.refresh_composer(cx);
        self.schedule_suggestion(cx);
        self.probe_context_window(i, cx);
        self.settle_undo();
        cx.notify();
    }

    /// 失败卡片上的「继续」：这一轮已经做了一部分，发一句「继续」让模型接着做
    /// （上下文里有做过的步骤和失败记录），不把整轮重来。
    pub(crate) fn resume_turn(&mut self, cx: &mut Context<Self>) {
        let idx = self.active;
        if self.sessions[idx].busy {
            return;
        }
        self.submit_to(
            idx,
            UserPrompt { text: "继续".into(), images: Vec::new(), attachments: Vec::new() },
            cx,
        );
    }

    /// 失败卡片上的「重试」：把第 `failed_idx` 行所在这一轮的用户消息原样再发一遍。
    pub(crate) fn retry_turn(&mut self, failed_idx: usize, cx: &mut Context<Self>) {
        let idx = self.active;
        let s = &self.sessions[idx];
        if s.busy {
            return;
        }
        let Some(user) = s.turn_prompt(failed_idx) else {
            return;
        };
        let Some(prompt) = self.resend_prompt(idx, user, cx) else {
            return;
        };
        self.submit_to(idx, prompt, cx);
    }

    /// 把对话里的一条用户消息还原成能再发一遍的提示词（重试 / 换内核重跑）。
    ///
    /// 气泡里那行附件名不是用户打的字，去掉；文本附件这次运行里还在就原样带上。
    /// 从日志恢复的会话里附件内容已经不在手里：照发用户的话，并提示一句。
    fn resend_prompt(
        &mut self,
        idx: usize,
        item_idx: usize,
        cx: &mut Context<Self>,
    ) -> Option<UserPrompt> {
        let Some(ChatItem::User { text, images, files }) =
            self.sessions[idx].transcript.get(item_idx)
        else {
            return None;
        };
        let typed = claudesktop_core::attach::strip_display(text);
        let lost = files.is_empty() && typed.len() != text.len();
        let prompt = UserPrompt {
            text: typed.to_string(),
            images: images.iter().filter_map(|p| load_prompt_image(p)).collect(),
            attachments: files.as_ref().clone(),
        };
        if lost {
            self.toast(
                Notification::warning(
                    "这条消息的文本附件已经不在了，只重发了文字和图片；需要的话请重新添加文件",
                ),
                cx,
            );
        }
        Some(prompt)
    }

    /// 同一任务换个内核跑：拿当前会话里第 `item_idx` 条用户消息，在 `kind` 上新开会话发出去。
    pub(crate) fn rerun_on(&mut self, item_idx: usize, kind: KernelChoice, cx: &mut Context<Self>) {
        let src = self.active;
        let Some(prompt) = self.resend_prompt(src, item_idx, cx) else {
            return;
        };
        let from = self.sessions[src].title.clone();
        let cwd = self.sessions[src].cwd.clone();
        let idx = self.open_session(kind.clone(), cwd);
        self.active = idx;
        self.sessions[idx].note(format!(
            "同一任务换到「{}」上跑（原会话：{from}）",
            kind.label(&self.settings.acp)
        ));
        // 内核不可用时 submit_to 会给出提示
        if self.sessions[idx].ready || self.kernel_of(&kind).is_none() {
            self.submit_to(idx, prompt, cx);
        } else {
            self.sessions[idx].autoprompt = Some(prompt);
        }
        cx.notify();
    }

    /// 压测钩子：往当前会话灌 `n` 行混合内容（用户消息 / markdown 回复 / 工具行 / 备注）。
    pub(crate) fn debug_stress(&mut self, n: usize, busy: bool, cx: &mut Context<Self>) {
        let s = &mut self.sessions[self.active];
        for i in 0..n {
            s.transcript.push(match i % 4 {
                0 => ChatItem::User {
                    text: format!("第 {i} 条：帮我看看这段代码为什么慢").into(),
                    images: Vec::new(),
                    files: Default::default(),
                },
                1 => ChatItem::Agent(format!(
                    "**第 {i} 条回复。** 先看热点：\n\n- 每帧重建整棵树\n- 列表没有虚拟化\n\n```rust\nfn hot(i: usize) -> usize {{ i * {i} }}\n```\n\n改完再测一次。"
                )),
                2 => ChatItem::Tool(ToolCard {
                    id: None,
                    kind: "read".into(),
                    name: "Read".into(),
                    arg: format!("src/module_{i}.rs").into(),
                    result: "120 行".into(),
                    output: SharedString::default(),
                    phase: ToolPhase::Done,
                    diffs: Vec::new(),
                    expanded: false,
                    task: false,
                    grant: None,
                }),
                _ => ChatItem::Note(format!("备注 {i}").into()),
            });
        }
        s.title = format!("压测 {n} 行").into();
        // 置忙：状态行和侧栏的转圈常驻（只看重绘频率，不会真的有内核事件来收尾），
        // 末尾再挂一段思考中（扫光）和一个跑着的工具（转圈）
        if busy {
            s.transcript.push(ChatItem::Thinking {
                text: "先看看热点在哪".into(),
                streaming: true,
                open: false,
                started: Some(std::time::Instant::now()),
                elapsed_ms: None,
            });
            s.transcript.push(ChatItem::Tool(ToolCard {
                id: None,
                kind: "execute".into(),
                name: "Bash".into(),
                arg: "cargo test".into(),
                result: SharedString::default(),
                output: SharedString::default(),
                phase: ToolPhase::Running,
                diffs: Vec::new(),
                expanded: false,
                task: false,
                grant: None,
            }));
            s.begin_turn();
        }
        cx.notify();
    }

    /// 压测钩子：逐帧滚动（先往上 150 帧再往下 150 帧，每帧 400px），走完出统计。
    /// 每帧画完（`on_next_frame`）才滚下一段，量到的帧间隔就是真实的一帧有多长。
    pub(crate) fn debug_scroll_frames(
        &mut self,
        step: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        const STEPS: usize = 300;
        if step >= STEPS {
            self.stress_report();
            return;
        }
        let dy = if step < STEPS / 2 { -400. } else { 400. };
        self.sessions[self.active].list.scroll_by(px(dy));
        cx.notify();
        let this = cx.entity().downgrade();
        window.on_next_frame(move |window, cx| {
            let _ = this.update(cx, |app, cx| app.debug_scroll_frames(step + 1, window, cx));
        });
    }

    /// 压测钩子：把内容区换成要压的页面（`CLAUDESKTOP_STRESS_VIEW`）。`settings:N` 是设置页第 N 个分类，
    /// `home` 是首页概览，`requests` 是首页「请求」页签（灌 `n` 轮假请求，列第一页），
    /// `requests-all` 同上但全部列出来。
    /// 首页要有内核才显示用量面板，没配过的就在内存里先当演示内核（不写盘）。
    pub(crate) fn debug_stress_view(
        &mut self,
        view: &str,
        n: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match view {
            "home" | "requests" | "requests-all" => {
                self.settings.kernel.get_or_insert(KernelChoice::Demo);
                self.overview_tab = 0;
                if view != "home" {
                    self.overview_tab = 2;
                    self.debug_fill_requests(n, view == "requests-all");
                }
            }
            _ => {
                let tab = view.strip_prefix("settings:").and_then(|t| t.parse().ok());
                self.open_settings_at(tab.unwrap_or(0), window, cx);
            }
        }
        cx.notify();
    }

    /// 压测钩子：逐帧往内容区发一格滚轮（先往下 150 帧再往上 150 帧），走完出统计。
    /// 走真实的滚轮路径：事件交给窗口分发，滚动容器自己改偏移、通知内容区重画。
    /// 滚到头不动了也让内容区照样重画一帧，量的始终是「滚一格要画多久」。
    /// 分发事件时不能占着 `ChatApp`（滚轮监听里会再去更新它），所以是关联函数、拿弱引用。
    pub(crate) fn debug_wheel_frames(
        this: WeakEntity<Self>,
        step: usize,
        window: &mut Window,
        cx: &mut App,
    ) {
        const STEPS: usize = 300;
        if step >= STEPS {
            let _ = this.update(cx, |app, _| app.stress_report());
            return;
        }
        let size = window.viewport_size();
        // 内容区中间偏右：设置页避开左侧分类导航，首页落在用量面板上
        let position = point(size.width * 0.65, size.height * 0.45);
        let lines = if step < STEPS / 2 { -3. } else { 3. };
        window.dispatch_event(
            PlatformInput::ScrollWheel(ScrollWheelEvent {
                position,
                delta: ScrollDelta::Lines(point(0., lines)),
                modifiers: Modifiers::default(),
                touch_phase: TouchPhase::Moved,
            }),
            cx,
        );
        let _ = this.update(cx, |app, cx| app.content_view.update(cx, |_, cx| cx.notify()));
        window
            .on_next_frame(move |window, cx| Self::debug_wheel_frames(this, step + 1, window, cx));
    }

    /// 压测钩子：逐帧往输入框里打一个字（300 帧，每 40 字换一行），走完清空输入框出统计。
    /// 和真打字走同一条路：输入框改内容 + 刷新补全 + 只重画输入区。
    pub(crate) fn debug_type_frames(
        &mut self,
        step: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        const STEPS: usize = 300;
        if step >= STEPS {
            self.stress_report();
            self.input.update(cx, |s, cx| s.set_value("", window, cx));
            return;
        }
        let ch = if step % 40 == 39 { "\n" } else { "字" };
        self.input.update(cx, |s, cx| s.insert(ch, window, cx));
        self.update_completion(cx);
        self.refresh_composer(cx);
        let this = cx.entity().downgrade();
        window.on_next_frame(move |window, cx| {
            let _ = this.update(cx, |app, cx| app.debug_type_frames(step + 1, window, cx));
        });
    }

    /// 压测钩子：把记下的帧时刻整理成「帧数、每秒帧数、帧间隔 p50 / p95 / max」，
    /// 写到 CLAUDESKTOP_STRESS_OUT（默认 stderr）。
    pub(crate) fn stress_report(&mut self) {
        let log = self.frame_log.take().unwrap_or_default();
        let mut work = std::mem::take(&mut *self.frame_work.borrow_mut());
        work.sort_by(|a, b| a.total_cmp(b));
        let wpct =
            |p: f64| work.get(((work.len() as f64 - 1.) * p) as usize).copied().unwrap_or(0.);
        let mut gaps: Vec<f64> =
            log.windows(2).map(|w| (w[1] - w[0]).as_secs_f64() * 1000.).collect();
        gaps.sort_by(|a, b| a.total_cmp(b));
        let pct = |p: f64| gaps.get(((gaps.len() as f64 - 1.) * p) as usize).copied().unwrap_or(0.);
        let span = match (log.first(), log.last()) {
            (Some(a), Some(b)) => (*b - *a).as_secs_f64(),
            _ => 0.,
        };
        let fps = if span > 0. { (log.len() as f64 - 1.) / span } else { 0. };
        let report = format!(
            "rows={} frames={} fps={fps:.1} p50={:.1}ms p95={:.1}ms max={:.1}ms work p50={:.2}ms p95={:.2}ms max={:.2}ms
",
            self.session().transcript.len(),
            log.len(),
            pct(0.5),
            pct(0.95),
            pct(1.0),
            wpct(0.5),
            wpct(0.95),
            wpct(1.0)
        );
        match std::env::var("CLAUDESKTOP_STRESS_OUT") {
            Ok(path) => {
                let _ = std::fs::write(path, report);
            }
            Err(_) => eprint!("{report}"),
        }
    }

    /// 调试钩子：把当前会话的首条提示词放到 `kind` 上重跑。
    pub(crate) fn rerun_first_prompt(&mut self, kind: KernelChoice, cx: &mut Context<Self>) {
        let first = self.sessions[self.active]
            .transcript
            .iter()
            .position(|i| matches!(i, ChatItem::User { .. }));
        if let Some(item) = first {
            self.rerun_on(item, kind, cx);
        }
    }

    /// 向内核要还没要过的会话（`session/new`），以及要续接的历史会话。连接未就绪的内核先跳过。
    pub(crate) fn request_pending_sessions(&mut self) {
        for idx in 0..self.sessions.len() {
            let Some(slot) = self.slot(&self.sessions[idx].kernel_key) else {
                continue;
            };
            let Some(kernel) = slot.kernel.clone().filter(|_| slot.ready) else {
                continue;
            };
            let s = &mut self.sessions[idx];
            if s.requested {
                continue;
            }
            if s.resuming {
                self.send_resume(idx, &kernel);
            } else if !s.restored && s.acp_id.is_none() {
                s.requested = true;
                let spec = SessionSpec { cwd: Some(s.cwd.clone()), kind: s.kind };
                if let Err(err) = kernel.new_session(s.token, spec) {
                    s.requested = false;
                    s.note(format!("创建会话失败：{err}"));
                }
            }
        }
    }

    /// 历史会话续接内核会话，就绪后再发待发的消息。同一个 ACP agent 才尝试；
    /// 自研内核用本地日志重建上下文，换了端点 / 模型也能续接。
    fn send_resume(&mut self, idx: usize, kernel: &Arc<dyn Kernel>) {
        let agent_cmd = self
            .slot(&self.sessions[idx].kernel_key)
            .map(|s| s.agent_cmd.clone())
            .unwrap_or_default();
        let s = &mut self.sessions[idx];
        s.requested = true;
        let builtin = s.kernel == KernelChoice::Builtin;
        let resumable = s.meta.as_ref().filter(|m| {
            if builtin {
                m.agent.starts_with("builtin:")
            } else {
                crate::settings::same_acp_agent(&m.agent, &agent_cmd)
                    && m.kernel_session_id.is_some()
            }
        });
        let cwd = Some(s.cwd.clone());
        let kind = s.kind;
        let sent = match resumable {
            Some(meta) => {
                let history = match &self.store {
                    Some(store) if builtin => store.load(&meta.id).unwrap_or_default(),
                    _ => Vec::new(),
                };
                let previous = meta.kernel_session_id.clone().unwrap_or_default();
                kernel.resume_session(s.token, &previous, SessionSpec { cwd, kind }, &history)
            }
            None => kernel.new_session(s.token, SessionSpec { cwd, kind }),
        };
        if let Err(err) = sent {
            s.requested = false;
            s.resuming = false;
            s.restored = true;
            s.autoprompt = None;
            s.note(format!("续接会话失败：{err}"));
        }
    }

    /// 调试钩子：CLAUDESKTOP_AUTODEMO=1 时自动发一条提示词
    /// （有内核则等会话就绪后经内核发送，否则播放演示脚本）。
    pub(crate) fn autodemo_hook(&mut self, cx: &mut Context<Self>) {
        if std::env::var("CLAUDESKTOP_AUTODEMO").ok().as_deref() != Some("1") {
            return;
        }
        // CLAUDESKTOP_AUTOPROMPT 指定提示词；未指定时只在不连真实 agent
        // （ACP_AGENT 未设置）时发默认演示词，避免对真实 agent 误发写文件任务。
        let text = match std::env::var("CLAUDESKTOP_AUTOPROMPT") {
            Ok(text) if !text.trim().is_empty() => text,
            _ if std::env::var_os("ACP_AGENT").is_none() => {
                "帮我配置一个有图形界面的动态壁纸软件".to_string()
            }
            _ => return,
        };
        // CLAUDESKTOP_AUTODEMO_SESSION 指定发往第几个会话（默认 0；历史会话从 1 起）
        let idx = std::env::var("CLAUDESKTOP_AUTODEMO_SESSION")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|i| *i < self.sessions.len())
            .unwrap_or(0);
        self.active = idx;
        // 历史会话直接走 submit_to：由它先续接内核会话
        if self.sessions[idx].ready || self.sessions[idx].restored {
            self.submit_to(idx, UserPrompt::text(text), cx);
        } else {
            self.sessions[idx].autoprompt = Some(UserPrompt::text(text));
        }
        cx.notify();
    }

    /// 发出输入框里的内容；这一轮还在跑时先排队（输入框上方的队列），这一轮正常结束后自动发出。
    pub(crate) fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().to_string();
        if text.trim().is_empty() && self.pending.is_empty() && self.pending_files.is_empty() {
            return;
        }
        // 还有图在后台转正 / 缩小：等处理完再发（缩略图上半透明占位，发送键也置灰）
        if self.pending.iter().any(|p| p.preparing) {
            self.toast(Notification::warning("图片还在处理，稍等一下再发"), cx);
            return;
        }
        // 这个模型明确看不了图：不发，图留在输入框里（换模型后再发）
        if let Some(why) = self.pending_images_unviewable() {
            self.toast(Notification::warning(why), cx);
            return;
        }
        let active = self.active;
        // 还没接模型：默认内核起不来，发出去也只会石沉大海。留着输入框里的字，先指去配置
        if self.settings.needs_setup() && self.sessions[active].kernel != KernelChoice::Demo {
            self.toast(
                Notification::warning("还没接上模型，先在上面选一种接法；刚写的内容还在输入框里"),
                cx,
            );
            return;
        }
        let queued =
            QueuedPrompt { text, images: self.pending.clone(), files: self.pending_files.clone() };
        // 发出去了：光标后面那句灰字、以及轮结束时猜的下一句都作废
        self.suggestion.clear();
        self.next_suggestion.clear();
        if self.sessions[active].busy || self.sessions[active].stopping {
            self.input.update(cx, |state, cx| state.set_value("", window, cx));
            self.pending.clear();
            self.pending_files.clear();
            self.history.push(&queued.text);
            self.sessions[active].queued.push(queued);
            cx.notify();
            return;
        }
        // worktree 还在后台建：先排队，建好了自动发出（见 `adopt_worktree`）
        if self.sessions[active].worktree_busy.is_some() {
            self.input.update(cx, |state, cx| state.set_value("", window, cx));
            self.pending.clear();
            self.pending_files.clear();
            self.history.push(&queued.text);
            self.sessions[active].queued.push(queued);
            cx.notify();
            return;
        }
        // 发不出去（内核不可用、会话还没就绪）就别清空，免得写好的内容丢了
        let sent_text = queued.text.clone();
        if self.submit_to(active, queued.into_prompt(), cx) {
            self.input.update(cx, |state, cx| state.set_value("", window, cx));
            self.pending.clear();
            self.pending_files.clear();
            // 真发出去了才进输入历史（排队的两处上面已经记过）；发失败的不记，
            // 免得 ↑ 翻出来一条从没发出去的消息
            self.history.push(&sent_text);
            self.refresh_composer(cx);
        }
    }

    /// 第 `idx` 个会话这一轮结束（正常结束，或用户停下后内核确认了）：发出排队的第一条，
    /// 其余等它这一轮结束再发。会话还没就绪时留着（`Started` 到来时再发，见 `apply` 里的事件处理）。
    fn dispatch_queued(&mut self, idx: usize, cx: &mut Context<Self>) {
        let s = &mut self.sessions[idx];
        if s.busy || s.stopping || s.queued.is_empty() || !s.ready {
            return;
        }
        let queued = s.queued.remove(0);
        self.submit_to(idx, queued.into_prompt(), cx);
    }

    /// 把当前会话排队的消息退回输入框：`only` 给了只退这一条，否则全部（按 Esc 停下时）。
    /// 文字按排队顺序接在一起，输入框里已经写的放在最后；附件放回待发送。
    pub(crate) fn unqueue(
        &mut self,
        only: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let active = self.active;
        let queue = &mut self.sessions[active].queued;
        let items = match only {
            Some(i) if i < queue.len() => vec![queue.remove(i)],
            Some(_) => return,
            None => std::mem::take(queue),
        };
        if items.is_empty() {
            return;
        }
        let mut parts = Vec::new();
        for item in items {
            if !item.text.trim().is_empty() {
                parts.push(item.text);
            }
            self.pending.extend(item.images);
            for file in item.files {
                if !self.pending_files.iter().any(|f| f.path == file.path) {
                    self.pending_files.push(file);
                }
            }
        }
        let draft = self.input.read(cx).value().to_string();
        if !draft.trim().is_empty() {
            parts.push(draft);
        }
        self.fill_prompt(
            &parts.join(
                "

",
            ),
            window,
            cx,
        );
        cx.notify();
    }

    /// 队列里的「现在发送」：上一轮没有正常结束（停了 / 出错）时队列不会自己走，由用户决定。
    pub(crate) fn send_queued(&mut self, i: usize, cx: &mut Context<Self>) {
        let active = self.active;
        let s = &mut self.sessions[active];
        if s.busy || i >= s.queued.len() {
            return;
        }
        let queued = s.queued.remove(i);
        self.submit_to(active, queued.into_prompt(), cx);
    }

    /// 队列里的「不发了」。
    pub(crate) fn remove_queued(&mut self, i: usize, cx: &mut Context<Self>) {
        let active = self.active;
        if i < self.sessions[active].queued.len() {
            self.sessions[active].queued.remove(i);
            cx.notify();
        }
    }

    // ---------- 图片附件 ----------

    /// 待发送图片张数上限（多了模型端也吃不下，界面先挡住）。
    const MAX_PENDING_IMAGES: usize = 8;

    /// 加入待发送图片（超出上限截断并提示）。
    pub(crate) fn add_pending_images(&mut self, images: Vec<PendingImage>, cx: &mut Context<Self>) {
        let room = Self::MAX_PENDING_IMAGES.saturating_sub(self.pending.len());
        if images.is_empty() {
            return;
        }
        if images.len() > room {
            self.toast(
                Notification::warning(format!(
                    "一轮最多附 {} 张图片，多出的没有加上",
                    Self::MAX_PENDING_IMAGES
                )),
                cx,
            );
        }
        self.pending.extend(images.into_iter().take(room));
        self.refresh_composer(cx);
        // 新来的图片在后台转正 / 缩小（粘贴、拖放、选文件都走这里）
        self.prepare_pending_images(cx);
    }

    /// 把刚收下的图片放到后台去转正 / 缩小 / 重编码（一张 1200 万像素的照片要一两百毫秒）。
    ///
    /// 处理期间缩略图先按原图半透明显示（不放转圈：输入区画在 `ChatApp` 里，循环动画会
    /// 带着内容区一起重画，见 AGENTS.md「动效」）；还有图没处理完时发送键置灰。
    pub(crate) fn prepare_pending_images(&mut self, cx: &mut Context<Self>) {
        if !self.pending.iter().any(|p| p.preparing) {
            return;
        }
        // 只处理还没处理过的那些，按编号找回（后台跑的时候列表可能被删过、挪过）
        let jobs: Vec<(u64, SharedString, Vec<u8>)> = self
            .pending
            .iter()
            .filter(|p| p.preparing)
            .filter_map(|p| {
                use base64::Engine as _;
                let raw =
                    base64::engine::general_purpose::STANDARD.decode(p.data.as_bytes()).ok()?;
                Some((p.id, p.name.clone(), raw))
            })
            .collect();
        if jobs.is_empty() {
            return;
        }
        let quality = claudesktop_core::image_prep::Quality::Standard;
        cx.spawn(async move |this, cx| {
            let results = cx
                .background_executor()
                .spawn(async move {
                    jobs.into_iter()
                        .map(|(id, name, raw)| {
                            let out = claudesktop_core::image_prep::prepare(&name, &raw, quality);
                            (id, name, out)
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            this.update(cx, |app, cx| {
                let mut failed: Vec<String> = Vec::new();
                for (id, name, out) in results {
                    // 处理期间被删了（或另一批已经处理好了）：跳过
                    let Some(slot) = app.pending.iter_mut().find(|p| p.id == id && p.preparing)
                    else {
                        continue;
                    };
                    match out {
                        Ok(prepared) => *slot = PendingImage::from_prepared(id, name, &prepared),
                        Err(err) => {
                            failed.push(err.message());
                            slot.preparing = false;
                        }
                    }
                }
                for why in failed {
                    app.toast(Notification::warning(why), cx);
                }
                // 处理失败的也留着（用户可以自己删），只是不再挂着「处理中」
                app.refresh_composer(cx);
            })
            .ok();
        })
        .detach();
    }

    /// 撤掉第 `i` 张待发送图片。
    pub(crate) fn remove_pending_image(&mut self, i: usize, cx: &mut Context<Self>) {
        if i < self.pending.len() {
            self.pending.remove(i);
            self.refresh_composer(cx);
        }
    }

    // ---------- 文本附件（只有聊天会话用：内容内联进消息） ----------

    /// 在当前会话的模型上，这些待发送图片有没有「看不了」的问题；返回提示里要说的那句话。
    ///
    /// 两种来源：能力表知道这个模型只吃文字（`vision_support == Some(false)`），或者用户
    /// 之前被端点回过 400、已经记进 `no_vision_models`（见设置）。
    pub(crate) fn pending_images_unviewable(&self) -> Option<String> {
        if self.pending.is_empty() || self.sessions[self.active].kernel != KernelChoice::Builtin {
            return None;
        }
        let model = &self.sessions[self.active].model_label;
        let protocol = self.settings.builtin.current().protocol;
        let known_bad =
            self.settings.no_vision_models.iter().any(|m| m.eq_ignore_ascii_case(model.as_ref()));
        let support = claudesktop_core::image_prep::vision_support(
            &serde_json::to_string(&protocol).unwrap_or_default(),
            model,
        );
        (known_bad || support == Some(false))
            .then(|| format!("当前模型 {model} 看不了图片，换一个能看图的模型再发"))
    }

    pub(crate) fn add_pending_files(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        use claudesktop_core::attach;
        let mut images = Vec::new();
        let mut refused: Vec<String> = Vec::new();
        for path in paths {
            // 图片还是图片附件（发送前按图片那样处理，见设计说明第七章）
            if let Some(image) = PendingImage::from_path(path) {
                images.push(image);
                continue;
            }
            // 已经带上了就不再读一遍
            if self.pending_files.iter().any(|f| &f.path == path) {
                continue;
            }
            match attach::read_text(path) {
                Ok(file) => {
                    if attach::fits_total(&self.pending_files, &file) {
                        self.pending_files.push(file);
                    } else {
                        let bytes = file.bytes();
                        refused.push(
                            attach::AttachError::TotalTooLarge { name: file.name, bytes }.message(),
                        );
                    }
                }
                // 读不了 / 不是文本：一句说明，不打断其余文件
                Err(err) => refused.push(err.message()),
            }
        }
        self.add_pending_images(images, cx);
        for why in refused {
            self.toast(Notification::warning(why), cx);
        }
        self.refresh_composer(cx);
    }

    /// 撤掉第 `i` 个待发送文本附件。
    pub(crate) fn remove_pending_file(&mut self, i: usize, cx: &mut Context<Self>) {
        if i < self.pending_files.len() {
            self.pending_files.remove(i);
            self.refresh_composer(cx);
        }
    }

    /// 把附件图片写进会话的资产目录，返回各自的绝对路径（仅文本消息返回空）。
    /// 演示回放没有会话存储，跳过（消息仍发送，只是不带图）。
    fn store_images(&mut self, idx: usize, images: &[PromptImage]) -> Vec<String> {
        use base64::Engine as _;
        if images.is_empty() || self.session_kernel(idx).is_none() {
            return Vec::new();
        }
        self.ensure_log(idx);
        let Some(store) = &self.store else {
            return Vec::new();
        };
        let Some(meta) = self.sessions[idx].meta.clone() else {
            return Vec::new();
        };
        images
            .iter()
            .filter_map(|image| {
                let bytes =
                    base64::engine::general_purpose::STANDARD.decode(image.data.as_bytes()).ok()?;
                let ext = ext_of(&image.media_type);
                store
                    .save_image(&meta.id, ext, &bytes)
                    .ok()
                    .map(|p| p.to_string_lossy().into_owned())
            })
            .collect()
    }

    /// 向第 `idx` 个会话发一轮用户消息：有内核走内核，演示回放会话播放演示脚本。
    /// 把一条消息发给第 `idx` 个会话。没发出去（会话忙、内核不可用、还没就绪）时返回 false，
    /// 原因已经告诉用户了。
    pub(crate) fn submit_to(
        &mut self,
        idx: usize,
        mut prompt: UserPrompt,
        cx: &mut Context<Self>,
    ) -> bool {
        let kind = self.sessions[idx].kernel.clone();
        if self.sessions[idx].busy {
            return false;
        }
        if !kind.is_demo() {
            // 历史会话的内核可能还没启动
            self.ensure_kernel(&kind);
            if self.kernel_of(&kind).is_none() {
                let status = self.slot(&kind.key()).map(|s| s.status.clone()).unwrap_or_default();
                let reason =
                    format!("{}不可用：{status}（可在设置里修改）", kind.label(&self.settings.acp));
                self.refuse_send(idx, reason, cx);
                return false;
            }
        }
        let kernel = self.session_kernel(idx);
        let mut start_demo_token = None;
        {
            let s = &mut self.sessions[idx];
            if kernel.is_some() && s.restored {
                // 历史会话：先续接内核会话（连接就绪后发出），就绪后再发这条消息
                s.restored = false;
                s.resuming = true;
                s.autoprompt = Some(prompt);
                self.request_pending_sessions();
                cx.notify();
                return true;
            }
            if kernel.is_some() && !s.ready {
                // 内核已经报错退出了（多半是命令找不到）：重新启动一次，用户可能已经把问题修好了
                // （比如刚装上 Node.js）；还是起不来的话内核会再报错，弹出原因
                let reason = if self.slot(&kind.key()).is_some_and(|s| s.fatal.is_some()) {
                    self.restart_kernel(&kind, cx);
                    format!("正在重新启动「{}」，稍等一下再发送", kind.label(&self.settings.acp))
                } else {
                    "会话还在连接内核，稍等一下再发送".into()
                };
                self.refuse_send(idx, reason, cx);
                return false;
            }
            s.steering = false;
            // 聊天会话的文本附件：内容拼在发给内核的全文里；气泡和日志里只写文件名
            let attachments = std::mem::take(&mut prompt.attachments);
            let has_attachments = !attachments.is_empty();
            let shown = claudesktop_core::attach::display_text(&prompt.text, &attachments);
            let full = claudesktop_core::attach::compose(&prompt.text, &attachments);
            if !s.has_user_message() && !s.renamed {
                // 只有附件时用第一个文件名当标题
                let basis = if prompt.text.trim().is_empty() {
                    attachments.first().map(|a| a.name.as_str()).unwrap_or("").to_string()
                } else {
                    prompt.text.clone()
                };
                s.title = crate::model::title_from_prompt(&basis).into();
            }
            // 图片附件先落盘（需要会话元信息），路径进记录与气泡
            let stored = self.store_images(idx, &prompt.images);
            if !prompt.images.is_empty() && stored.is_empty() && kind.is_demo() {
                self.sessions[idx].note("演示回放不保存图片附件，本轮只发送文字");
            }
            let image_paths = stored.iter().map(PathBuf::from).collect::<Vec<_>>();
            self.sessions[idx].transcript.push(ChatItem::User {
                text: SharedString::from(shown.clone()),
                images: image_paths,
                files: std::sync::Arc::new(attachments.clone()),
            });
            // 插件的斜杠命令：对话里显示原样的「/命令 参数」，发给内核的是展开后的模板
            let enabled = self.enabled_plugins();
            let expanded = claudesktop_core::plugin::find_command(&enabled, &prompt.text).map(
                |(_, command, args)| claudesktop_core::plugin::expand_command(&command.body, &args),
            );
            // 聊天附件的内容跟在（展开后的）文字后面一起发出去
            let mut sent = match &expanded {
                Some(text) => text.clone(),
                None => full,
            };
            // 上一轮在审查页里撤回过块：发给内核的文字前面附上说明，气泡里照常只显示用户写的
            let reviewed = match self.take_review_note(idx) {
                Some(note) => {
                    sent = format!("{note}\n\n{sent}");
                    true
                }
                None => false,
            };
            // 和用户写的一样就不必另存一份（`sent` 只在不一样时进日志）
            let sent = (reviewed || expanded.is_some() || has_attachments).then_some(sent);
            if kernel.is_some() {
                self.persist(
                    idx,
                    Record::User { text: shown.clone(), images: stored, sent: sent.clone() },
                );
            }
            let prompt = match sent {
                // 附件内容已经在 `sent` 里了，内核不必再拼一遍
                Some(text) => UserPrompt { text, images: prompt.images, attachments: Vec::new() },
                None => prompt,
            };
            let s = &mut self.sessions[idx];
            s.begin_turn();
            s.follow_tail();
            match (&kernel, s.acp_id.clone()) {
                (Some(kernel), Some(id)) => {
                    // ACP agent 经用量网关时，网关据此把这个会话接下来的请求归到这一轮
                    if matches!(s.kernel, KernelChoice::Acp(_)) {
                        let turn =
                            s.transcript.iter().filter(|i| matches!(i, ChatItem::User { .. }));
                        claudesktop_builtin::gateway::note_turn(&id, turn.count() as u64);
                    }
                    // 附件内容已经在 `sent` 里：内核只看文字，不必再拼一遍
                    if let Err(err) = kernel.send_prompt(&id, prompt) {
                        s.busy = false;
                        s.fail(format!("发送失败：{err}"));
                    }
                }
                (Some(_), None) => {
                    s.busy = false;
                    s.note("会话尚未就绪，请稍候再发送");
                }
                (None, _) => {
                    s.cancel_token += 1;
                    start_demo_token = Some(s.cancel_token);
                }
            }
        }
        if let Some(token) = start_demo_token {
            self.start_demo(idx, token, cx);
        }
        cx.notify();
        true
    }

    /// 消息没发出去：在对话流里说明原因。还没发过消息的会话显示的是首页、看不到对话流，
    /// 改用应用内提示，不然这句话写进去了也没人看见。
    fn refuse_send(&mut self, idx: usize, reason: String, cx: &mut Context<Self>) {
        if self.sessions[idx].has_user_message() {
            self.sessions[idx].note(reason);
        } else {
            self.toast(Notification::warning(reason), cx);
        }
        cx.notify();
    }

    /// 应答当前会话里的某条权限请求。
    pub(crate) fn resolve_permission(
        &mut self,
        item_idx: usize,
        choice: Option<(String, String)>,
        cx: &mut Context<Self>,
    ) {
        let active = self.active;
        let kernel = self.session_kernel(active);
        self.sessions[active].resolve_permission(item_idx, choice, kernel.as_ref());
        if let Some(ChatItem::Permission { id, resolved: Some(label), .. }) =
            self.sessions[active].transcript.get(item_idx)
        {
            let record = Record::PermissionResolved { id: *id, label: label.to_string() };
            self.persist(active, record);
        }
        cx.notify();
    }

    /// 权限卡上的「拒绝并说明」：按拒绝应答，停下这一轮，请用户在输入框里说要怎么改
    /// （对照 CLI 的 "No, and tell Claude what to do differently"）。输入框里已经写好了就直接发出。
    pub(crate) fn reject_and_steer(
        &mut self,
        item_idx: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let reject = match self.session().transcript.get(item_idx) {
            Some(ChatItem::Permission { options, resolved: None, .. }) => options
                .iter()
                .find(|o| o.kind == "reject_once")
                .or_else(|| options.iter().find(|o| o.kind.starts_with("reject")))
                .map(|o| (o.id.clone(), o.name.clone())),
            _ => return,
        };
        // 内核没给拒绝项时按取消应答
        self.resolve_permission(item_idx, reject, cx);
        if self.session().busy {
            self.stop(window, cx);
        }
        let active = self.active;
        self.sessions[active].steering = true;
        self.send(window, cx);
        self.focus_input(window, cx);
        cx.notify();
    }

    /// 排队的压缩（`Session::compact_queued`）：一轮正常结束后、发排队的消息**之前**压成一轮。
    /// 返回是不是这会儿接着压（调用方据此别再往下走）。和 [`Self::dispatch_queued`] 同样的前提：
    /// 会话忙着、正在停、还没就绪时不动它，留着等下一个时机（一轮结束 / `Started`）。
    fn queued_compaction(&mut self, idx: usize) -> bool {
        let s = &self.sessions[idx];
        if !s.compact_queued || s.busy || s.stopping || !s.ready {
            return false;
        }
        self.sessions[idx].compact_queued = false;
        self.start_compaction(idx, "这个会话还没接上内核：压缩没有做")
    }

    /// 让内核把第 `idx` 个会话的上下文压成摘要，自成一轮；返回压起来了没有。
    /// `unbound` 是会话还没接上内核时记进对话的说明。
    fn start_compaction(&mut self, idx: usize, unbound: &str) -> bool {
        let kernel = self.session_kernel(idx);
        let s = &mut self.sessions[idx];
        match (kernel, s.acp_id.clone()) {
            (Some(kernel), Some(id)) => match kernel.compact(&id) {
                Ok(()) => {
                    s.begin_turn();
                    s.compact_turn = true;
                    true
                }
                Err(err) => {
                    s.note(format!("压缩上下文失败：{err}"));
                    false
                }
            },
            _ => {
                s.note(unbound);
                false
            }
        }
    }

    /// 手动压缩当前会话的上下文（自研内核、空闲时）：自成一轮，进度和结果作为一行出现在对话里。
    /// 一轮跑着时排到这一轮后面（和排队的消息一样，见 `ChatApp::queued_compaction`）。
    pub(crate) fn compact_session(&mut self, cx: &mut Context<Self>) {
        let active = self.active;
        let s = &mut self.sessions[active];
        // 发出去了：光标后面那句灰字作废（它按上下文猜的，压完就不成立了）
        self.suggestion.clear();
        if s.busy || s.stopping {
            s.compact_queued = true;
            cx.notify();
            return;
        }
        self.start_compaction(active, "这个会话还没接上内核：先发一条消息，再压缩");
        cx.notify();
    }

    /// 停下当前会话这一轮。排队的消息退回输入框（对照 CLI：停下来通常是要改主意）。
    pub(crate) fn stop(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let active = self.active;
        let kernel = self.session_kernel(active);
        let s = &mut self.sessions[active];
        s.cancel_token += 1;
        s.busy = false;
        // 正在流式的思考块收起，不然停下后还挂着「思考中…」；压缩到一半的那行也收尾
        s.finish_thinking();
        s.interrupt_running_tools();
        s.settle_compaction(CompactState::Failed("已停止".into()));
        let thought = s.take_thought_done();
        s.cancel_pending_permissions(kernel.as_ref());
        match (&kernel, s.acp_id.clone()) {
            (Some(kernel), Some(id)) => {
                // 等内核回这一轮的结束（见 Session::stopping）；发取消失败就不等了
                s.stopping = kernel.cancel(&id).is_ok();
                s.note("已请求停止");
            }
            _ => s.note("已停止（演示回放）"),
        }
        // 演示回放不落盘
        if let (Some(ms), Some(_)) = (thought, &kernel) {
            self.persist(active, Record::Thought { elapsed_ms: ms });
        }
        // 停下通常是要改主意：排着的压缩也撤掉，说明一句
        self.sessions[active].drop_queued_compaction("已停下");
        self.unqueue(None, window, cx);
        cx.notify();
    }

    /// 内核事件分发：连接级事件自己处理，会话级事件在绑定这个内核的会话里按 sessionId / token 找。
    pub(crate) fn on_kernel_event(
        &mut self,
        key: String,
        routed: RoutedEvent,
        cx: &mut Context<Self>,
    ) {
        let RoutedEvent { session, event } = routed;
        let kernel = self.kernel_by_key(&key);
        let acp = self.settings.acp.clone();
        let kind = KernelChoice::from_key(&key, &acp);
        let Some(slot) = self.slot_mut(&key) else {
            return;
        };
        let slot = &mut *slot;
        match event {
            KernelEvent::Connected { agent_name } => {
                slot.ready = true;
                slot.error = false;
                slot.fatal = None;
                // 去掉 npm 作用域前缀（@scope/name → name），底栏空间有限
                let short = agent_name.rsplit('/').next().unwrap_or(&agent_name);
                slot.status = format!("{short} · 已连接").into();
                self.request_pending_sessions();
            }
            KernelEvent::SessionFailed { token, message } => {
                if let Some(s) =
                    self.sessions.iter_mut().find(|s| s.kernel == kind && s.token == token)
                {
                    s.requested = false;
                    s.resuming = false;
                    s.note(format!("创建会话失败：{message}"));
                }
            }
            KernelEvent::McpStatus { servers } => slot.mcp = servers,
            KernelEvent::MemorySuggested { candidates } => {
                // 只进设置页顶部的待确认区，不进对话流（误记的代价比漏记大）。
                // 来源会话用事件自己的 sessionId；第几轮按对话里的用户消息数算（和内核的轮次对齐）
                let sid = session.clone().unwrap_or_default();
                let turn = self
                    .sessions
                    .iter()
                    .find(|s| s.acp_id.as_deref() == Some(sid.as_str()))
                    .map(|s| {
                        s.transcript.iter().filter(|i| matches!(i, ChatItem::User { .. })).count()
                    })
                    .unwrap_or(0) as u64;
                self.memory_candidates.push(candidates, &sid, turn);
                cx.notify();
            }
            KernelEvent::BackgroundTasks { tasks } => {
                slot.tasks = tasks;
                cx.notify();
            }
            KernelEvent::Fatal(message) => {
                slot.ready = false;
                slot.error = true;
                slot.status = "内核错误".into();
                // 多半发生在刚启动时（命令找不到、agent 一起来就退出），这会儿用户常常还在首页，
                // 对话流里的说明看不到，所以另弹一条提示
                let text = crate::model::kernel_failure_text(
                    &kind.label(&acp),
                    crate::settings::agent_command(&slot.agent_cmd),
                    &message,
                );
                slot.fatal = Some(message.clone());
                for s in self.sessions.iter_mut().filter(|s| s.kernel == kind) {
                    s.on_fatal(&message, kernel.as_ref());
                }
                self.toast(Notification::error(text), cx);
            }
            event => {
                // 内核记下一条记忆时顺手刷新「记忆」页
                if matches!(&event, KernelEvent::MemoryChanged { .. })
                    && self.settings_open
                    && self.settings_form.tab == memory_view::MEMORY_TAB
                {
                    self.reload_memory_view();
                }
                // 会话级事件：Started 按令牌认领，其余按 sessionId
                let idx = match &event {
                    KernelEvent::Started { token, .. } => {
                        self.sessions.iter().position(|s| s.kernel == kind && s.token == *token)
                    }
                    _ => session.as_deref().and_then(|id| {
                        self.sessions
                            .iter()
                            .position(|s| s.kernel == kind && s.acp_id.as_deref() == Some(id))
                    }),
                };
                if let Some(idx) = idx {
                    // 检查点：内容存成 blob、日志里记哈希，不进对话
                    let event = match event {
                        KernelEvent::FileChanged { path, before, after } => {
                            self.record_file_change(idx, path, before, after);
                            return;
                        }
                        event => event,
                    };
                    let turn_over = matches!(
                        event,
                        KernelEvent::TurnEnded { .. } | KernelEvent::PromptFailed { .. }
                    );
                    // 正常结束才接着发排队的消息，出错、被取消时先停着，由用户决定；
                    // 用户停下以后才排的（停止期间发的、「拒绝并说明」的理由）等停稳了就发
                    let clean_end = matches!(
                        &event,
                        KernelEvent::TurnEnded { stop_reason } if stop_reason == "end_turn"
                    ) || (turn_over && self.sessions[idx].stopping);
                    // 先看（record_turn 会清掉手动压缩的标记）
                    let attention = self.sessions[idx].attention(&event);
                    if Record::worth_keeping(&event) {
                        self.persist(idx, Record::Event { event: event.clone() });
                    }
                    if turn_over {
                        self.record_turn(idx);
                        self.touch_meta(idx);
                        // 上一轮出错了：排着的压缩丢掉，说明一句；排队的消息不自动发，
                        // 由用户看着决定（见输入框上方的队列）。用户停下的在 `stop` 里已经撤掉了
                        if !clean_end {
                            self.sessions[idx].drop_queued_compaction("这一轮出错了");
                        }
                    }
                    let prompt = self.sessions[idx].apply_event(event.clone(), kernel.as_ref());
                    // 端点回了「这个模型看不了图片」：把这轮用过的模型记进设置，
                    // 下次附上图时直接按「看不了」提醒（设置页里能清掉）
                    if let KernelEvent::PromptFailed { message } = &event
                        && claudesktop_core::image_prep::image_rejection(message)
                    {
                        let model = self.sessions[idx].model_label.to_string();
                        if !model.is_empty()
                            && !self
                                .settings
                                .no_vision_models
                                .iter()
                                .any(|m| m.eq_ignore_ascii_case(&model))
                        {
                            let record = model.clone();
                            self.save_ui_pref(move |s| s.no_vision_models.push(record.clone()), cx);
                            self.push_no_vision_models();
                        }
                    }
                    // 网关又记了请求：用量页的费用、请求跟着刷新（不用等这一轮跑完再手动重读）。
                    // 工具循环每一步都会报用量，只在请求日志真看得见时才读（它自己攒 2 秒）
                    if turn_over
                        || (self.requests_shown()
                            && matches!(event, KernelEvent::UsageUpdated { .. }))
                    {
                        self.refresh_requests_soon(cx);
                    }
                    // 模型起了标题：元信息也跟着更新（TurnEnded 时写的还是旧标题）
                    if matches!(event, KernelEvent::TitleGenerated { .. }) {
                        self.touch_meta(idx);
                    }
                    // 这条事件收尾了一段思考：用时紧跟着写进日志，重放时挂回那段思考
                    if let Some(ms) = self.sessions[idx].take_thought_done() {
                        self.persist(idx, Record::Thought { elapsed_ms: ms });
                    }
                    if turn_over {
                        self.probe_context_window(idx, cx);
                        if attention.is_some() {
                            self.mark_unread(idx);
                        }
                    }
                    if let Some(body) = attention {
                        self.notify_attention(idx, body, cx);
                    }
                    if let Some(prompt) = prompt {
                        self.submit_to(idx, prompt, cx);
                    }
                    if clean_end {
                        // 排着的压缩先压（它自成一轮），压起来了排队的话等压完再发
                        if !self.queued_compaction(idx) {
                            self.dispatch_queued(idx, cx);
                            // 这一轮正常结束、输入框空着：猜用户接下来会发的那一句（见 suggest.rs）
                            if idx == self.active && !self.settings_open {
                                self.schedule_next_suggestion(cx);
                            }
                        }
                    }
                    // 一轮结束、worktree 会话：刷新分支状态（脏文件数、领先几个提交）
                    if turn_over {
                        self.refresh_worktree(idx, cx);
                    }
                    // 刚就绪（Started）：建 worktree 期间排队的话这会儿能发了
                    if matches!(event, KernelEvent::Started { .. }) {
                        self.flush_pending_thinking(idx, kernel.as_deref());
                        self.flush_pending_web(idx, kernel.as_deref());
                        if !self.queued_compaction(idx) {
                            self.dispatch_queued(idx, cx);
                        }
                    }
                }
            }
        }
        cx.notify();
    }

    /// 确保第 `idx` 个会话的日志已打开（首次写入时建档）；失败时提示并停用存储。
    fn ensure_log(&mut self, idx: usize) {
        if self.sessions[idx].log.is_some() {
            return;
        }
        // 存储可能已在别处停用（写盘失败）。这里取局部引用，不再 `unwrap`——
        // 那是运行期持久化路径，存储异常不该让整个界面 panic。
        let Some(store) = self.store.as_ref() else {
            return;
        };
        let agent_cmd = self
            .slot(&self.sessions[idx].kernel_key)
            .map(|s| s.agent_cmd.clone())
            .unwrap_or_default();
        let s = &mut self.sessions[idx];
        let opened = match &s.meta {
            Some(meta) => store.append_log(&meta.id),
            None => store.create(&s.title, s.cwd.clone(), &agent_cmd).map(|(mut meta, log)| {
                meta.set_kernel_session(s.acp_id.clone());
                // 是不是任务、跑在哪个 worktree 在建档时定下来（侧栏据此分组，见 sidebar.rs）；
                // 会话类型也在这里写死（建好就不再改）
                meta.task = s.task;
                meta.worktree = s.worktree.clone();
                meta.kind = s.kind;
                meta.web = (s.kind == SessionKind::Chat).then_some(s.web);
                let _ = store.save_meta(&meta);
                s.meta = Some(meta);
                log
            }),
        };
        match opened {
            Ok(log) => s.log = Some(log),
            Err(err) => {
                s.note(format!("会话未能保存：{err}"));
                self.store = None;
            }
        }
    }

    /// 往第 `idx` 个会话的日志追加一条记录；首次写入时建档。
    /// 写盘失败只提示一次，不影响对话本身。
    fn persist(&mut self, idx: usize, record: Record) {
        if self.store.is_none() {
            return;
        }
        self.ensure_log(idx);
        if let Some(log) = &mut self.sessions[idx].log {
            let _ = log.append(record);
        }
    }

    /// 内核改了文件：改动前后的内容存成 blob，日志里记哈希（回退时还原，见 rewind.rs）。
    /// 存储不可用时不记，回退时就没有这个文件。
    fn record_file_change(
        &mut self,
        idx: usize,
        path: String,
        before: Option<String>,
        after: Option<String>,
    ) {
        if self.store.is_none() {
            return;
        }
        self.ensure_log(idx);
        let (Some(store), Some(meta)) = (&self.store, &self.sessions[idx].meta) else {
            return;
        };
        let save = |text: Option<String>| text.map(|t| store.save_blob(&meta.id, &t)).transpose();
        match (save(before), save(after)) {
            (Ok(before), Ok(after)) => {
                self.persist(idx, Record::FileChanged { path, before, after, reverted: None })
            }
            (Err(err), _) | (_, Err(err)) => {
                self.sessions[idx].note(format!("检查点没存上（回退时还原不了 {path}）：{err}"))
            }
        }
    }

    /// 一轮结束：把时间、模型、token、用时记进日志和用量统计。
    fn record_turn(&mut self, idx: usize) {
        let s = &mut self.sessions[idx];
        // 手动压缩自成一轮但没有用户消息：不记（用量概览只数跟在用户消息后面的轮次），
        // 也不能挂到上一轮名下，把那一轮回复下面的用时和用量换掉
        if std::mem::take(&mut s.compact_turn) {
            s.turn_started = None;
            s.turn_usage = Default::default();
            return;
        }
        let summary = s.close_turn();
        let TurnUsage { input: input_tokens, output: output_tokens, .. } = summary.usage;
        let model = Some(s.model_label.to_string()).filter(|m| m != UNKNOWN_MODEL);
        let context_tokens = Some(s.context_tokens).filter(|n| *n > 0);
        let at_ms = now_ms();
        self.persist(
            idx,
            Record::Turn {
                at_ms,
                model: model.clone(),
                input_tokens,
                output_tokens,
                elapsed_ms: summary.elapsed_ms,
                context_tokens,
            },
        );
        let s = &self.sessions[idx];
        let session = s.meta.as_ref().map_or_else(|| format!("live-{}", s.token), |m| m.id.clone());
        self.usage.push(TurnStat { session, at_ms, model, tokens: input_tokens + output_tokens });
    }

    /// 用量统计：结束了的轮，加上还在跑的轮（按目前已知的用量算；跑完 `record_turn` 换成准数）。
    pub(super) fn usage_stats(&self) -> std::borrow::Cow<'_, [TurnStat]> {
        let live: Vec<TurnStat> = self
            .sessions
            .iter()
            .filter(|s| s.busy && !s.compact_turn && s.turn_started.is_some())
            .map(|s| TurnStat {
                session: s
                    .meta
                    .as_ref()
                    .map_or_else(|| format!("live-{}", s.token), |m| m.id.clone()),
                at_ms: now_ms(),
                model: Some(s.model_label.to_string()).filter(|m| m != UNKNOWN_MODEL),
                tokens: s.turn_usage.input + s.live_output_tokens(),
            })
            .collect();
        if live.is_empty() {
            return self.usage.as_slice().into();
        }
        let mut all = self.usage.clone();
        all.extend(live);
        all.into()
    }

    /// 一轮结束：刷新日志，更新元信息（标题 / 内核会话 ID / 更新时间）。
    fn touch_meta(&mut self, idx: usize) {
        let Some(store) = &self.store else {
            return;
        };
        let key = self.sessions[idx].kernel_key.clone();
        let agent_cmd = self.slot(&key).map(|slot| slot.agent_cmd.clone()).unwrap_or_default();
        let s = &mut self.sessions[idx];
        if let Some(log) = &mut s.log {
            let _ = log.flush();
        }
        if let Some(meta) = &mut s.meta {
            // 续接到别的 agent 后，会话就归新的 agent 了
            meta.agent = agent_cmd;
            meta.title = s.title.to_string();
            meta.set_kernel_session(s.acp_id.clone());
            meta.task = s.task;
            meta.worktree = s.worktree.clone();
            meta.kind = s.kind;
            meta.web = (s.kind == SessionKind::Chat).then_some(s.web);
            meta.updated_ms = now_ms();
            let _ = store.save_meta(meta);
        }
    }

    /// 第 `idx` 个会话这一轮结束了：不在眼前就标成待查看（侧栏标点，切过去时清掉，见 `sync_sidebar`）。
    fn mark_unread(&mut self, idx: usize) {
        if idx != self.active || self.settings_open {
            self.sessions[idx].unread = true;
        }
    }

    pub(crate) fn start_demo(&mut self, idx: usize, token: u64, cx: &mut Context<Self>) {
        let script = build_demo_script();
        // 按会话令牌找（中途删了别的会话，下标会变）
        let session = self.sessions[idx].token;
        cx.spawn(async move |this, cx| {
            for step in script {
                cx.background_executor().timer(Duration::from_millis(step.0)).await;
                let mut cancelled = false;
                let _ = this.update(cx, |app, cx| {
                    match app.sessions.iter_mut().find(|s| s.token == session) {
                        Some(s) if s.cancel_token == token => s.apply_mock(&step.1),
                        _ => {
                            cancelled = true;
                            return;
                        }
                    }
                    cx.notify();
                });
                if cancelled {
                    return;
                }
            }
            let _ = this.update(cx, |app, cx| {
                if let Some(idx) = app.sessions.iter().position(|s| s.token == session) {
                    let s = &mut app.sessions[idx];
                    s.busy = false;
                    s.close_turn();
                    app.mark_unread(idx);
                    app.dispatch_queued(idx, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for ChatApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        static FIRST_FRAME: std::sync::atomic::AtomicBool =
            std::sync::atomic::AtomicBool::new(false);
        if !FIRST_FRAME.swap(true, std::sync::atomic::Ordering::Relaxed)
            && let Some(t0) = crate::STARTUP_T0.get()
            && std::env::var("CLAUDESKTOP_BENCH_STARTUP").is_ok()
        {
            let elapsed = t0.elapsed();
            eprintln!(
                "[startup_bench] main -> first_frame:     {:>6.2} ms",
                elapsed.as_secs_f64() * 1000.0
            );
            if std::env::var("CLAUDESKTOP_BENCH_EXIT").is_ok() {
                std::process::exit(0);
            }
        }
        // 压测：记帧时刻，并在根上最后画一个 canvas，量这一帧 render → paint 的工作量
        let frame_probe = self.frame_log.as_mut().map(|log| {
            let t0 = std::time::Instant::now();
            log.push(t0);
            let work = self.frame_work.clone();
            canvas(
                |_, _, _| {},
                move |_, _, _, _| work.borrow_mut().push(t0.elapsed().as_secs_f64() * 1000.),
            )
            .absolute()
            .size_0()
        });

        // 顶栏高度跟字号缩放，系统红绿灯要跟着挪（字号没变就不动）
        #[cfg(target_os = "macos")]
        {
            let rem = window.rem_size();
            if self.traffic_lights_rem != Some(rem) {
                self.traffic_lights_rem = Some(rem);
                window.set_traffic_light_position(chrome::traffic_light_origin(rem));
            }
        }

        // 侧栏是缓存 view：它用到的状态变了才让它重画（每帧比一次指纹，很便宜）
        self.sync_sidebar(cx);
        self.sync_placeholder(window, cx);

        // 输入框上方是缓存的内容区（见 content.rs）；设置页没有输入框
        let body =
            v_flex().flex_1().min_h_0().child(self.content_slot()).when(!self.settings_open, |d| {
                d.child(div().px_6().pb_4().pt_2().child(self.render_composer(window, cx)))
            });
        let main = v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .relative()
            .child(self.render_header(window, cx))
            .child(body)
            .when(!self.settings_open, |d| self.with_file_drop(d, cx));

        let mut root = div();
        // 中文回退字体：根上设一次，子元素换 font_family 时继承
        root.text_style().font_fallbacks = Some(cjk_fallbacks());
        // 不用系统标题栏：侧栏顶到窗口顶，会话区顶栏兼做拖拽区与窗口按钮（见 chrome.rs）
        let root = root
            .h_flex()
            .size_full()
            .font_family(sans())
            .text_size(TEXT_LARGE)
            .bg(rgb(PAPER))
            .text_color(rgb(INK))
            .relative()
            .children(self.render_sidebar_slot(window, cx))
            .child(main)
            .child(self.sidebar_spinners_view.clone())
            .children(frame_probe);
        // 快捷键（见 keys.rs）
        self.with_key_actions(root, cx)
    }
}

/// 工作目录的显示名：取目录名，没有（如盘符根目录）就用整个路径。
pub(crate) fn project_label(dir: &Path) -> SharedString {
    dir.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir.display().to_string())
        .into()
}

/// 菜单里显示的完整路径：用户目录缩写成 `~`，免得长路径撑出面板。
pub(crate) fn short_path(dir: &Path) -> String {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"));
    match home.and_then(|h| dir.strip_prefix(h).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".into(),
        Some(rest) => format!("~{}{}", std::path::MAIN_SEPARATOR, rest.display()),
        None => dir.display().to_string(),
    }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// MIME 类型 → 存盘扩展名。
fn ext_of(media_type: &str) -> &'static str {
    match media_type {
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        "image/bmp" => "bmp",
        _ => "png",
    }
}

/// 扩展名 → MIME 类型。
pub(crate) fn mime_of(path: &std::path::Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        _ => "image/png",
    }
}

/// 从磁盘读一张附件图片 → 发给内核的 base64（文件丢了返回 None，跳过）。
fn load_prompt_image(path: &std::path::Path) -> Option<PromptImage> {
    use base64::Engine as _;
    let bytes = std::fs::read(path).ok()?;
    Some(PromptImage {
        media_type: mime_of(path).to_string(),
        data: base64::engine::general_purpose::STANDARD.encode(bytes),
    })
}

#[cfg(test)]
mod copy_tests {
    use super::*;

    /// 复制到 Code 会话的只有对话本身：文件改动 / 检查点 / 回退不抄。
    #[core::prelude::v1::test]
    fn only_the_conversation_is_copied() {
        assert!(copyable(&Record::user("你好")));
        assert!(copyable(&Record::Event {
            event: KernelEvent::AgentMessageChunk { text: "在".into() }
        }));
        assert!(copyable(&Record::PermissionResolved { id: 1, label: "Yes".into() }));
        assert!(copyable(&Record::Thought { elapsed_ms: 1200 }));
        // 文件改动、检查点、回退不抄
        assert!(!copyable(&Record::FileChanged {
            path: "a.rs".into(),
            before: None,
            after: None,
            reverted: None,
        }));
        assert!(!copyable(&Record::Rewind {
            turn: 0,
            conversation: true,
            files: 0,
            prompt: "x".into(),
        }));
    }
}

//! M1 UI 外壳（内核未接入 · 演示回放）
//!
//! 目标界面：Claude 桌面风（暖纸底 + 赤陶橙），含
//! - 自绘标题栏 + 侧栏（工作区分组 / 任务列表 / 内核卡）
//! - 会话视图：左缘头像轨道、思考块、工具卡、计划、流式 Markdown
//! - 空会话首页：问候语 + 用量概览（统计卡 + 热力图）
//! - 底部输入区：工作区 / 模式 chips、圆角输入条、权限模式栏

use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::text::{TextView, TextViewStyle};
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::time::Duration;

// ---------- 色板（Claude 品牌基准：暖纸底 + 赤陶橙） ----------
const PAPER: u32 = 0xFAF9F5;
const SAND: u32 = 0xF0EEE6;
const CORAL: u32 = 0xD97757;
const SAND_ACTIVE: u32 = 0xE8E3D6;
const INK: u32 = 0x3D3929;
const INK_SOFT: u32 = 0x87847C;
const INK_FAINT: u32 = 0xB5B0A4;
const LINE: u32 = 0xE5E1D5;
const CARD: u32 = 0xFFFFFF;
const GREEN: u32 = 0x7A9A65;
const BLUE: u32 = 0x5B6FA8;
const BLUE_TINT: u32 = 0xE9EEF7;
// 字体：官方是自家的 Sans / Serif / Mono，无法分发，取 Windows 自带的近似款。
const SANS: &str = "Segoe UI Variable Text";
const MONO: &str = "Cascadia Mono";
const DIFF_ADD_BG: u32 = 0xE4F3DE;
const DIFF_ADD_FG: u32 = 0x3C6B2E;
const DIFF_DEL_BG: u32 = 0xFBE3DF;
const DIFF_DEL_FG: u32 = 0xA4443A;
// Claude 的标题使用衬线体；Windows 自带 Georgia 作近似，中文回退到系统衬线。
const SERIF: &str = "Georgia";

// ---------- 数据模型 ----------
#[derive(Clone, Copy, PartialEq)]
enum ToolPhase {
    Running,
    Done,
}

/// 工具调用：`● Name arg` + `└ result`（Claude Code 的紧凑样式）
struct ToolCard {
    name: &'static str,
    arg: SharedString,
    result: SharedString,
    phase: ToolPhase,
}

/// diff 行：(' ' 上下文 / '+' 新增 / '-' 删除, 文本)
type DiffRows = &'static [(char, &'static str)];

struct PlanRow {
    text: &'static str,
    done: bool,
}

enum ChatItem {
    User(SharedString),
    Thinking {
        text: String,
        streaming: bool,
        open: bool,
    },
    Tool(ToolCard),
    Diff {
        path: &'static str,
        rows: DiffRows,
    },
    Plan(Vec<PlanRow>),
    Agent(String),
    Note(SharedString),
}

struct SessionEntry {
    title: SharedString,
    age: SharedString,
    dot: Option<u32>, // 部分会话带在线小圆点
}

struct Workspace {
    name: SharedString,
    sessions: Vec<SessionEntry>,
}

struct StatCard {
    label: &'static str,
    value: SharedString,
}

// ---------- 演示回放脚本 ----------
enum MockAction {
    BeginThinking,
    ThinkChunk(&'static str),
    EndThinking,
    BeginTool(&'static str, &'static str),
    ToolDone(&'static str),
    Diff(&'static str, DiffRows),
    BeginPlan,
    PlanRowAdd(&'static str),
    PlanRowDone(usize),
    BeginAgent,
    AgentChunk(&'static str),
    EndAgent,
    Note(&'static str),
}

struct MockStep(u64, MockAction);

/// 把长文本按固定字数切成流式 chunk 步骤。
fn stream_steps(
    text: &'static str,
    per: usize,
    delay: u64,
    make: fn(&'static str) -> MockAction,
) -> Vec<MockStep> {
    let mut out = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let n = rest
            .char_indices()
            .nth(per)
            .map(|(i, _)| i)
            .unwrap_or(rest.len());
        let (chunk, tail) = rest.split_at(n);
        out.push(MockStep(delay, make(chunk)));
        rest = tail;
    }
    out
}

const DEMO_DIFF: DiffRows = &[
    (' ', "pub trait Kernel: Send {"),
    ('-', "    fn send(&mut self, text: String);"),
    ('+', "    fn send(&mut self, text: String) -> Result<()>;"),
    ('+', "    fn cancel(&mut self);"),
    (' ', "}"),
];

fn build_demo_script() -> Vec<MockStep> {
    let mut s = Vec::new();

    // 1) 思考块
    s.push(MockStep(300, MockAction::BeginThinking));
    s.extend(stream_steps(
        "用户在验收 UI 外壳。先展示工具卡与计划，再输出流式正文，最后给回合小结。",
        5,
        36,
        MockAction::ThinkChunk,
    ));
    s.push(MockStep(200, MockAction::EndThinking));

    // 2) 读文件
    s.push(MockStep(
        450,
        MockAction::BeginTool("Read", "crates/core/src/lib.rs"),
    ));
    s.push(MockStep(
        700,
        MockAction::ToolDone("Read 128 lines · Kernel trait 与 KernelEvent 事件模型"),
    ));

    // 3) 改文件 + diff
    s.push(MockStep(
        450,
        MockAction::BeginTool("Edit", "crates/core/src/lib.rs"),
    ));
    s.push(MockStep(
        700,
        MockAction::ToolDone("Added 2 lines, removed 1 line"),
    ));
    s.push(MockStep(
        150,
        MockAction::Diff("crates/core/src/lib.rs", DEMO_DIFF),
    ));

    // 4) 跑构建
    s.push(MockStep(
        500,
        MockAction::BeginTool("Bash", "cargo build -p claudesktop"),
    ));
    s.push(MockStep(
        950,
        MockAction::ToolDone("Finished `dev` profile in 1.8s · 0 errors, 0 warnings"),
    ));

    // 5) 计划卡
    s.push(MockStep(400, MockAction::BeginPlan));
    for row in [
        "UI 外壳验收（布局 / 消息类型 / IME）",
        "接入 ACP 内核（session/update → 消息流）",
        "InProcess 内核 + Provider trait",
        "权限审批与 MCP 配置统一到 UI 侧",
    ] {
        s.push(MockStep(260, MockAction::PlanRowAdd(row)));
    }
    for i in 0..4 {
        s.push(MockStep(300, MockAction::PlanRowDone(i)));
    }

    // 6) 流式 Markdown 正文
    s.push(MockStep(350, MockAction::BeginAgent));
    s.extend(stream_steps(
        "## UI 外壳验收清单\n\n这轮回复来自**内置演示回放**——UI 已就位，内核按计划下一步接入 ACP。\n\n你在上面看到的消息类型：\n\n- **思考块**：可折叠，流式追加\n- **工具卡片**：读文件 / 跑构建，带「运行中 → 完成」状态\n- **计划**：左缘赤陶色标记的清单，逐项点亮\n- **Markdown 正文**：就是本块，`stream_fade` 流式渲染\n\n接入后，这里的事件流将直接映射自 ACP 的 `session/update` 通知，UI 不再需要任何演示脚本。\n\n> 布局 / 配色 / 交互有任何不顺眼的地方，现在改最便宜。",
        4,
        30,
        MockAction::AgentChunk,
    ));
    s.push(MockStep(250, MockAction::EndAgent));

    // 7) 回合小结
    s.push(MockStep(
        400,
        MockAction::Note("演示回合结束 · 内核未连接 · 下一步：ACP 接入"),
    ));
    s
}

/// 权限模式（名称沿用官方桌面版）：(名称, 说明, 图标)
const PERMISSION_MODES: [(&str, &str, IconAll); 4] = [
    ("Manual", "每次编辑和命令都先询问", IconAll::ShieldCheck),
    ("Accept edits", "自动接受文件编辑，命令仍询问", IconAll::SquarePen),
    ("Plan", "只出计划，不改动文件", IconAll::ListTodo),
    ("Auto", "由分类器判断，少打扰", IconAll::Zap),
];

// ---------- 应用状态 ----------
pub struct ChatApp {
    input: Entity<InputState>,
    transcript: Vec<ChatItem>,
    workspaces: Vec<Workspace>,
    /// None = 首页（用量概览）；Some(i) = 打开第 i 个会话
    selected: Option<usize>,
    busy: bool,
    cancel_token: u64,
    scroll: ScrollHandle,
    follow_scroll: bool,
    overview_tab: usize, // 0 概览 / 1 模型
    overview_range: usize, // 0 全部 / 1 30 天 / 2 7 天
    sidebar_open: bool,
    mode_menu_open: bool,
    permission_mode: usize, // PERMISSION_MODES 下标
}

impl ChatApp {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("How can I help you today?")
        });

        cx.subscribe_in(
            &input,
            window,
            |this: &mut ChatApp,
             _entity: &Entity<InputState>,
             event: &InputEvent,
             window: &mut Window,
             cx: &mut Context<ChatApp>| {
                if let InputEvent::PressEnter { .. } = event {
                    this.send(window, cx);
                }
            },
        )
        .detach();

        let workspaces = vec![
            Workspace {
                name: "claudesktop".into(),
                sessions: vec![
                    SessionEntry { title: "新会话".into(), age: "".into(), dot: None },
                    SessionEntry { title: "gpui 高性能 Claude 风格桌…".into(), age: "28 分钟".into(), dot: None },
                ],
            },
            Workspace {
                name: "ubuntu".into(),
                sessions: vec![
                    SessionEntry { title: "服务器能否调用 deepseek 搜索…".into(), age: "1 天".into(), dot: Some(GREEN) },
                    SessionEntry { title: "将 Clash for Windows 订阅导入…".into(), age: "1 天".into(), dot: Some(GREEN) },
                    SessionEntry { title: "探查当前目录".into(), age: "2 天".into(), dot: Some(GREEN) },
                ],
            },
            Workspace {
                name: "news_collector_integration".into(),
                sessions: vec![
                    SessionEntry { title: "帮我搜索 dsh 插件，能够不在工…".into(), age: "3 天".into(), dot: None },
                ],
            },
            Workspace {
                name: "work_station".into(),
                sessions: vec![
                    SessionEntry { title: "看一下我电脑上 antigravity 安装…".into(), age: "9 分钟".into(), dot: None },
                    SessionEntry { title: "Investigating zcode codingplan…".into(), age: "1 天".into(), dot: None },
                ],
            },
        ];

        Self {
            input,
            transcript: Vec::new(),
            workspaces,
            selected: Some(0),
            busy: false,
            cancel_token: 0,
            scroll: ScrollHandle::new(),
            follow_scroll: true,
            overview_tab: 0,
            overview_range: 0,
            sidebar_open: true,
            mode_menu_open: false,
            permission_mode: 0,
        }
    }

    /// 调试钩子：CLAUDESKTOP_AUTODEMO=1 时自动触发一次演示回放，
    /// 用于无人工交互地复现回放链路的崩溃。
    fn autodemo_hook(&mut self, cx: &mut Context<Self>) {
        if std::env::var("CLAUDESKTOP_AUTODEMO").ok().as_deref() != Some("1") {
            return;
        }
        self.transcript
            .push(ChatItem::User("帮我配置一个有图形界面的动态壁纸软件".into()));
        self.selected = Some(0);
        self.busy = true;
        self.follow_scroll = true;
        self.cancel_token += 1;
        self.start_demo(cx);
        cx.notify();
    }

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let text = self.input.read(cx).value().to_string();
        if text.trim().is_empty() {
            return;
        }
        // 从首页发消息 → 自动进入会话视图
        if self.selected.is_none() {
            self.selected = Some(0);
        }
        self.transcript.push(ChatItem::User(SharedString::from(text)));
        self.input
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.busy = true;
        self.follow_scroll = true;
        self.cancel_token += 1;
        self.start_demo(cx);
        cx.notify();
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        self.cancel_token += 1;
        self.busy = false;
        self.transcript
            .push(ChatItem::Note("已停止（演示回放）".into()));
        cx.notify();
    }

    fn start_demo(&mut self, cx: &mut Context<Self>) {
        let script = build_demo_script();
        let token = self.cancel_token;
        cx.spawn(async move |this, cx| {
            for step in script {
                cx.background_executor()
                    .timer(Duration::from_millis(step.0))
                    .await;
                let mut cancelled = false;
                let _ = this.update(cx, |app, cx| {
                    if app.cancel_token != token {
                        cancelled = true;
                        return;
                    }
                    app.apply_mock(&step.1);
                    cx.notify();
                });
                if cancelled {
                    return;
                }
            }
            let _ = this.update(cx, |app, cx| {
                app.busy = false;
                cx.notify();
            });
        })
        .detach();
    }

    fn apply_mock(&mut self, action: &MockAction) {
        match action {
            MockAction::BeginThinking => {
                self.transcript.push(ChatItem::Thinking {
                    text: String::new(),
                    streaming: true,
                    open: true,
                });
            }
            MockAction::ThinkChunk(chunk) => {
                if let Some(ChatItem::Thinking { text, .. }) = self.transcript.last_mut() {
                    text.push_str(chunk);
                }
            }
            MockAction::EndThinking => {
                if let Some(ChatItem::Thinking { streaming, open, .. }) =
                    self.transcript.last_mut()
                {
                    *streaming = false;
                    *open = false;
                }
            }
            MockAction::BeginTool(name, arg) => {
                self.transcript.push(ChatItem::Tool(ToolCard {
                    name,
                    arg: (*arg).into(),
                    result: SharedString::default(),
                    phase: ToolPhase::Running,
                }));
            }
            MockAction::ToolDone(result) => {
                if let Some(ChatItem::Tool(card)) = self.transcript.last_mut() {
                    card.result = (*result).into();
                    card.phase = ToolPhase::Done;
                }
            }
            MockAction::Diff(path, rows) => {
                self.transcript.push(ChatItem::Diff { path, rows });
            }
            MockAction::BeginPlan => {
                self.transcript.push(ChatItem::Plan(Vec::new()));
            }
            MockAction::PlanRowAdd(row) => {
                if let Some(ChatItem::Plan(rows)) = self.transcript.last_mut() {
                    rows.push(PlanRow {
                        text: row,
                        done: false,
                    });
                }
            }
            MockAction::PlanRowDone(i) => {
                if let Some(ChatItem::Plan(rows)) = self.transcript.last_mut() {
                    if let Some(row) = rows.get_mut(*i) {
                        row.done = true;
                    }
                }
            }
            MockAction::BeginAgent => {
                self.transcript.push(ChatItem::Agent(String::new()));
            }
            MockAction::AgentChunk(chunk) => {
                if let Some(ChatItem::Agent(text)) = self.transcript.last_mut() {
                    text.push_str(chunk);
                }
            }
            MockAction::EndAgent => {}
            MockAction::Note(text) => {
                self.transcript.push(ChatItem::Note((*text).into()));
            }
        }
    }

    // ---------- 小组件 ----------
    fn chip(icon: IconAll, label: impl Into<SharedString>) -> Div {
        div()
            .h_flex()
            .gap_1p5()
            .px_2p5()
            .py_1p5()
            .rounded_md()
            .border_1()
            .border_color(rgb(LINE))
            .bg(rgb(CARD))
            .child(
                Icon::new(icon)
                    .size_3p5()
                    .text_color(rgb(INK_SOFT)),
            )
            .child(
                div()
                    .text_size(px(13.))
                    .text_color(rgb(INK))
                    .child(label.into()),
            )
    }

    /// 标题栏中间的 Chat / Cowork / Code 分段切换。
    fn top_tab(text: &'static str, active: bool) -> Div {
        div()
            .px_3p5()
            .py_1()
            .rounded_md()
            .text_size(px(14.))
            .bg(if active { rgb(CARD) } else { rgba(0x00000000) })
            .text_color(if active { rgb(INK) } else { rgb(INK_SOFT) })
            .child(text)
    }

    fn seg(text: &'static str, active: bool) -> Div {
        div()
            .px_3()
            .py_1()
            .rounded_md()
            .text_size(px(13.))
            .bg(if active { rgb(SAND_ACTIVE) } else { rgba(0x00000000) })
            .text_color(if active { rgb(INK) } else { rgb(INK_SOFT) })
            .child(text)
    }

    // ---------- 消息渲染 ----------
    fn render_thinking(
        idx: usize,
        text: &str,
        streaming: bool,
        open: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let caret = if streaming { " ▍" } else { "" };
        let header = div()
            .id(("think-toggle", idx))
            .h_flex()
            .gap_1p5()
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(ChatItem::Thinking { open, .. }) = this.transcript.get_mut(idx) {
                    *open = !*open;
                    cx.notify();
                }
            }))
            .child(
                Icon::new(IconAll::Brain)
                    .size_3p5()
                    .text_color(rgb(INK_SOFT)),
            )
            .child(
                div().text_size(px(12.5)).text_color(rgb(INK_SOFT)).child(if streaming {
                    SharedString::from("思考中…")
                } else {
                    SharedString::from("已思考")
                }),
            )
            .child(
                Icon::new(if open {
                    IconAll::ChevronDown
                } else {
                    IconAll::ChevronRight
                })
                .size_3()
                .text_color(rgb(INK_SOFT)),
            );

        let mut col = div().v_flex().gap_1().child(header);
        if open {
            col = col.child(
                div()
                    .text_size(px(13.))
                    .text_color(rgb(INK_SOFT))
                    .line_height(relative(1.6))
                    .child(format!("{text}{caret}")),
            );
        }
        col
    }

    fn render_tool(card: &ToolCard) -> Div {
        let dot = match card.phase {
            ToolPhase::Running => CORAL,
            ToolPhase::Done => GREEN,
        };
        let mut col = div().v_flex().gap(px(2.)).child(
            div()
                .h_flex()
                .items_start()
                .gap_2()
                .child(
                    div()
                        .size(px(7.))
                        .mt(px(7.))
                        .flex_shrink_0()
                        .rounded_full()
                        .bg(rgb(dot)),
                )
                .child(
                    div()
                        .text_size(px(14.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(INK))
                        .child(card.name),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(13.5))
                        .font_family(MONO)
                        .text_color(rgb(INK_SOFT))
                        .line_height(relative(1.7))
                        .child(card.arg.clone()),
                ),
        );
        if !card.result.is_empty() {
            col = col.child(
                div()
                    .h_flex()
                    .items_start()
                    .gap_2()
                    .pl_3()
                    .child(
                        div()
                            .text_size(px(13.5))
                            .font_family(MONO)
                            .text_color(rgb(INK_FAINT))
                            .child("└"),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(13.5))
                            .font_family(MONO)
                            .text_color(rgb(INK_SOFT))
                            .child(card.result.clone()),
                    ),
            );
        }
        col
    }

    fn render_diff(path: &'static str, rows: DiffRows) -> Div {
        let mut old_no = 1usize;
        let mut new_no = 1usize;
        let mut body = div().v_flex();
        for (kind, text) in rows {
            let (bg, fg, no) = match kind {
                '+' => {
                    let n = new_no;
                    new_no += 1;
                    (Some(DIFF_ADD_BG), DIFF_ADD_FG, n)
                }
                '-' => {
                    let n = old_no;
                    old_no += 1;
                    (Some(DIFF_DEL_BG), DIFF_DEL_FG, n)
                }
                _ => {
                    let n = new_no;
                    old_no += 1;
                    new_no += 1;
                    (None, INK_SOFT, n)
                }
            };
            body = body.child(
                div()
                    .h_flex()
                    .items_start()
                    .when_some(bg, |d, c| d.bg(rgb(c)))
                    .font_family(MONO)
                    .text_size(px(13.5))
                    .line_height(relative(1.6))
                    .child(
                        div()
                            .w(px(36.))
                            .flex_shrink_0()
                            .pr_2()
                            .flex()
                            .justify_end()
                            .text_color(rgb(INK_FAINT))
                            .child(format!("{no}")),
                    )
                    .child(
                        div()
                            .w(px(14.))
                            .flex_shrink_0()
                            .text_color(rgb(fg))
                            .child(kind.to_string()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_color(rgb(if bg.is_some() { fg } else { INK }))
                            .child(*text),
                    ),
            );
        }
        div()
            .v_flex()
            .rounded_lg()
            .border_1()
            .border_color(rgb(LINE))
            .bg(rgb(CARD))
            .overflow_hidden()
            .child(
                div()
                    .h_flex()
                    .gap_1p5()
                    .px_3()
                    .py_1p5()
                    .bg(rgb(SAND))
                    .border_b_1()
                    .border_color(rgb(LINE))
                    .child(
                        Icon::new(IconAll::FileText)
                            .size_3p5()
                            .text_color(rgb(INK_SOFT)),
                    )
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_family(MONO)
                            .text_color(rgb(INK_SOFT))
                            .child(path),
                    ),
            )
            .child(body)
    }

    fn render_plan(rows: &[PlanRow]) -> Div {
        div()
            .v_flex()
            .gap_1p5()
            .p_3()
            .rounded_lg()
            .bg(rgb(CARD))
            .border_1()
            .border_color(rgb(LINE))
            .child(
                div()
                    .h_flex()
                    .gap_1p5()
                    .child(
                        Icon::new(IconAll::ListTodo)
                            .size_4()
                            .text_color(rgb(CORAL)),
                    )
                    .child(div().text_size(px(14.)).text_color(rgb(INK)).child("计划")),
            )
            .children(rows.iter().enumerate().map(|(i, row)| {
                let marker = if row.done {
                    div()
                        .size(px(15.))
                        .rounded_full()
                        .bg(rgb(GREEN))
                        .flex()
                        .justify_center()
                        .items_center()
                        .child(
                            Icon::new(IconAll::Check)
                                .size_3()
                                .text_color(rgb(0xFFFFFF)),
                        )
                } else {
                    div()
                        .size(px(15.))
                        .rounded_full()
                        .border_1()
                        .border_color(rgb(LINE))
                };
                div()
                    .id(("plan-row", i))
                    .h_flex()
                    .gap_2()
                    .items_center()
                    .child(marker)
                    .child(
                        div()
                            .text_size(px(13.5))
                            .text_color(if row.done {
                                rgb(INK_SOFT)
                            } else {
                                rgb(INK)
                            })
                            .child(row.text),
                    )
            }))
    }

    /// 助手回合行：官方界面没有头像轨道，内容直接顶格。
    fn railed(content: impl IntoElement) -> Div {
        div().min_w_0().child(content)
    }

    fn render_item(&self, idx: usize, item: &ChatItem, cx: &mut Context<Self>) -> Div {
        match item {
            ChatItem::User(text) => div()
                .w_full()
                .bg(rgb(SAND))
                .text_color(rgb(INK))
                .rounded(px(12.))
                .px_4()
                .py_3()
                .text_size(px(15.))
                .line_height(relative(1.6))
                .child(text.clone()),
            ChatItem::Thinking {
                text,
                streaming,
                open,
            } => Self::railed(Self::render_thinking(idx, text, *streaming, *open, cx)),
            ChatItem::Tool(card) => Self::railed(Self::render_tool(card)),
            ChatItem::Diff { path, rows } => Self::railed(Self::render_diff(path, rows)),
            ChatItem::Plan(rows) => Self::railed(Self::render_plan(rows)),
            ChatItem::Agent(text) => Self::railed(
                div().text_size(px(15.5)).line_height(relative(1.7)).child(
                    TextView::markdown(("agent", idx), SharedString::from(text.clone()))
                        .style(
                            TextViewStyle::default()
                                .paragraph_gap(rems(0.9))
                                .heading_font_size(|level, _| match level {
                                    1 => px(22.),
                                    2 => px(19.),
                                    3 => px(17.),
                                    _ => px(15.5),
                                }),
                        )
                        .stream_fade(true),
                ),
            ),
            ChatItem::Note(text) => {
                div().h_flex().justify_center().pt_1().child(
                    div()
                        .text_size(px(12.))
                        .text_color(rgb(INK_FAINT))
                        .child(text.clone()),
                )
            }
        }
    }

    // ---------- 首页（用量概览） ----------
    fn heat_color(v: u8) -> u32 {
        match v {
            0 => 0xE7E2D6,   // 空白格：沙色
            1 => 0xD8DFF0,   // 浅蓝
            2 => 0xA9B9DC,   // 中蓝
            3 => 0x7C92C4,   // 深蓝
            _ => 0x4C62A6,   // 最深
        }
    }

    fn render_overview(&self, cx: &mut Context<Self>) -> Div {
        // 热力图假数据：6 行 × 26 列，0..=4 强度
        let mut heat: Vec<u8> = Vec::with_capacity(6 * 26);
        let mut seed: u32 = 0x9E3779B9;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed % 100) as u8
        };
        for i in 0..(6 * 26) {
            let r = next();
            // 稀疏分布：大多数为 0
            let v = if r < 78 {
                0
            } else if r < 88 {
                1
            } else if r < 94 {
                2
            } else if r < 98 {
                3
            } else {
                4
            };
            // 末尾几天制造最近活跃
            let v = if i >= 6 * 26 - 3 && v == 0 { (i % 3 + 2) as u8 } else { v };
            heat.push(v);
        }

        let stats = [
            StatCard { label: "会话数", value: "87".into() },
            StatCard { label: "消息数", value: "4,194".into() },
            StatCard { label: "Token 总量", value: "299.4M".into() },
            StatCard { label: "活跃天数", value: "9".into() },
            StatCard { label: "高峰时段", value: "下午 12 点".into() },
            StatCard { label: "最常用模型", value: "GLM-5.3".into() },
        ];

        let mut grid = div().v_flex().gap(px(3.));
        for row in 0..6 {
            let mut line = div().h_flex().gap(px(3.));
            for col in 0..26 {
                let v = heat[row * 26 + col];
                line = line.child(
                    div()
                        .size(px(14.))
                        .rounded(px(3.))
                        .bg(rgb(Self::heat_color(v))),
                );
            }
            grid = grid.child(line);
        }

        div()
            .v_flex()
            .flex_1()
            .min_h_0()
            .items_center()
            .justify_center()
            .gap_6()
            .child(
                // 问候语
                div()
                    .h_flex()
                    .gap_2p5()
                    .items_center()
                    .child(
                        Icon::new(IconAll::Sparkles)
                            .size_5()
                            .text_color(rgb(CORAL)),
                    )
                    .child(
                        div()
                            .font_family(SERIF)
                            .text_size(px(34.))
                            .text_color(rgb(INK))
                            .child("What's up next, WYT?"),
                    ),
            )
            .child(
                div()
                    .v_flex()
                    .gap_4()
                    .p_5()
                    .rounded_xl()
                    .bg(rgb(SAND))
                    .child(
                        // 顶行：左侧 tabs，右侧范围
                        div()
                            .h_flex()
                            .items_center()
                            .child(
                                div()
                                    .h_flex()
                                    .gap_1()
                                    .child(
                                        div()
                                            .id("tab-overview")
                                            .cursor_pointer()
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.overview_tab = 0;
                                                cx.notify();
                                            }))
                                            .child(Self::seg("概览", self.overview_tab == 0)),
                                    )
                                    .child(
                                        div()
                                            .id("tab-model")
                                            .cursor_pointer()
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.overview_tab = 1;
                                                cx.notify();
                                            }))
                                            .child(Self::seg("模型", self.overview_tab == 1)),
                                    ),
                            )
                            .child(div().flex_1())
                            .child(
                                div()
                                    .h_flex()
                                    .gap_1()
                                    .child(
                                        div()
                                            .id("range-all")
                                            .cursor_pointer()
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.overview_range = 0;
                                                cx.notify();
                                            }))
                                            .child(Self::seg("全部", self.overview_range == 0)),
                                    )
                                    .child(
                                        div()
                                            .id("range-30")
                                            .cursor_pointer()
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.overview_range = 1;
                                                cx.notify();
                                            }))
                                            .child(Self::seg("30 天", self.overview_range == 1)),
                                    )
                                    .child(
                                        div()
                                            .id("range-7")
                                            .cursor_pointer()
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.overview_range = 2;
                                                cx.notify();
                                            }))
                                            .child(Self::seg("7 天", self.overview_range == 2)),
                                    ),
                            ),
                    )
                    .child(
                        // 统计卡：2 行 × 3 列
                        div()
                            .v_flex()
                            .gap_3()
                            .child(
                                div()
                                    .h_flex()
                                    .gap_3()
                                    .children(stats[..3].iter().map(|c| {
                                        div()
                                            .v_flex()
                                            .gap_1()
                                            .w(px(220.))
                                            .p_3()
                                            .rounded_lg()
                                            .bg(rgb(CARD))
                                            .child(
                                                div()
                                                    .text_size(px(12.5))
                                                    .text_color(rgb(INK_SOFT))
                                                    .child(c.label),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(17.))
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .text_color(rgb(INK))
                                                    .child(c.value.clone()),
                                            )
                                    })),
                            )
                            .child(
                                div()
                                    .h_flex()
                                    .gap_3()
                                    .children(stats[3..].iter().map(|c| {
                                        div()
                                            .v_flex()
                                            .gap_1()
                                            .w(px(220.))
                                            .p_3()
                                            .rounded_lg()
                                            .bg(rgb(CARD))
                                            .child(
                                                div()
                                                    .text_size(px(12.5))
                                                    .text_color(rgb(INK_SOFT))
                                                    .child(c.label),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(17.))
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .text_color(rgb(INK))
                                                    .child(c.value.clone()),
                                            )
                                    })),
                            ),
                    )
                    .child(grid)
                    .child(
                        div()
                            .pt_1()
                            .text_size(px(13.))
                            .text_color(rgb(INK_SOFT))
                            .child("你的 Token 用量相当于 3,333 本《善恶的彼岸》。"),
                    ),
            )
    }

    // ---------- 侧栏 ----------
    fn render_sidebar(&self, cx: &mut Context<Self>) -> Div {
        let mut body = v_flex().gap_0().flex_1().min_h_0().overflow_hidden();
        let mut flat_idx = 0usize;

        // 工作区头部行
        body = body.child(
            div()
                .h_flex()
                .items_center()
                .px_3()
                .pt_3()
                .pb_1()
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(rgb(INK_SOFT))
                        .child("工作区"),
                )
                .child(div().flex_1())
                .child(
                    Icon::new(IconAll::ArrowUpDown)
                        .size_3p5()
                        .text_color(rgb(INK_SOFT)),
                )
                .child(
                    div().pl_2().child(
                        Icon::new(IconAll::ExternalLink)
                            .size_3p5()
                            .text_color(rgb(INK_SOFT)),
                    ),
                ),
        );

        for ws in &self.workspaces {
            // 工作区名
            body = body.child(
                div()
                    .px_3()
                    .pt_3()
                    .pb_1()
                    .text_size(px(13.5))
                    .text_color(rgb(INK))
                    .child(ws.name.clone()),
            );
            for s in &ws.sessions {
                let active = self.selected == Some(flat_idx);
                let this_idx = flat_idx;
                let mut row = div()
                    .id(("session", flat_idx))
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .mx_2()
                    .px_2()
                    .py(px(5.))
                    .rounded_md()
                    .bg(if active { rgb(SAND_ACTIVE) } else { rgba(0x00000000) })
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected = Some(this_idx);
                        cx.notify();
                    }))
                    .child(
                        div()
                            .size(px(5.))
                            .flex_shrink_0()
                            .rounded_full()
                            .bg(if let Some(c) = s.dot { rgb(c) } else { rgb(LINE) }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(px(14.))
                            .text_color(if active { rgb(INK) } else { rgb(INK_SOFT) })
                            .child(s.title.clone()),
                    );
                if !s.age.is_empty() {
                    row = row.child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(12.))
                            .text_color(rgb(INK_FAINT))
                            .child(s.age.clone()),
                    );
                }
                body = body.child(row);
                flat_idx += 1;
            }
        }

        // 任务分组
        body = body.child(
            div()
                .px_3()
                .pt_4()
                .pb_1()
                .text_size(px(12.))
                .text_color(rgb(INK_SOFT))
                .child("任务"),
        );
        for (i, (title, age)) in [
            ("看一下我电脑上 antigravity to…", "9 分钟"),
            ("Investigating zcode codingpla…", "1 天"),
            ("检查一下我的 dsh 的插件，有一…", "2 天"),
        ]
        .iter()
        .enumerate()
        {
            body = body.child(
                div()
                    .id(("task", i))
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .mx_2()
                    .px_2()
                    .py(px(5.))
                    .rounded_md()
                    .cursor_pointer()
                    .child(
                        div().size(px(5.)).flex_shrink_0().rounded_full().bg(rgb(LINE)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(px(14.))
                            .text_color(rgb(INK_SOFT))
                            .child(SharedString::from(*title)),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(12.))
                            .text_color(rgb(INK_FAINT))
                            .child(SharedString::from(*age)),
                    ),
            );
        }

        v_flex()
            .w(px(248.))
            .flex_shrink_0()
            .h_full()
            .bg(rgb(SAND))
            .border_r_1()
            .border_color(rgb(LINE))
            .child(
                // 品牌行
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .pt_3()
                    .pb_1()
                    .child(
                        Icon::new(IconAll::Sparkles)
                            .size_4()
                            .text_color(rgb(CORAL)),
                    )
                    .child(
                        div()
                            .font_family(SERIF)
                            .text_size(px(18.))
                            .text_color(rgb(INK))
                            .child("Claude"),
                    )
                    .child(div().flex_1())
                    .child(self.sidebar_toggle("sidebar-toggle", cx)),
            )
            .child(
                div()
                    .id("new-chat")
                    .h_flex()
                    .gap_2()
                    .mx_2()
                    .mt_2()
                    .px_2()
                    .py_1p5()
                    .rounded_md()
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.selected = None;
                        this.transcript.clear();
                        cx.notify();
                    }))
                    .child(
                        Icon::new(IconAll::SquarePen)
                            .size_3p5()
                            .text_color(rgb(INK)),
                    )
                    .child(div().text_size(px(14.)).text_color(rgb(INK)).child("新会话")),
            )
            .child(
                div()
                    .id("plugins")
                    .h_flex()
                    .gap_2()
                    .mx_2()
                    .px_2()
                    .py_1p5()
                    .rounded_md()
                    .cursor_pointer()
                    .child(
                        Icon::new(IconAll::Blocks)
                            .size_3p5()
                            .text_color(rgb(INK_SOFT)),
                    )
                    .child(div().text_size(px(14.)).text_color(rgb(INK_SOFT)).child("插件")),
            )
            .child(div().flex_1().min_h_0().child(body))
            .child(
                // 底部：更多 + 内核状态
                div()
                    .v_flex()
                    .gap_2()
                    .p_3()
                    .child(
                        div()
                            .h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                Icon::new(IconAll::Ellipsis)
                                    .size_3p5()
                                    .text_color(rgb(INK_SOFT)),
                            )
                            .child(
                                div()
                                    .text_size(px(13.5))
                                    .text_color(rgb(INK_SOFT))
                                    .child("更多"),
                            ),
                    )
                    .child(
                        div()
                            .h_flex()
                            .gap_1p5()
                            .items_center()
                            .child(
                                div().size(px(6.)).rounded_full().bg(rgb(CORAL)),
                            )
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(rgb(INK_SOFT))
                                    .child("演示回放（未连接内核）"),
                            ),
                    ),
            )
    }

    // ---------- 会话区顶栏 ----------
    fn render_top_bar(&self) -> Div {
        div()
            .h_flex()
            .items_center()
            .gap_2p5()
            .px_5()
            .py_2p5()
            .border_b_1()
            .border_color(rgb(LINE))
            .child(
                div()
                    .h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .size(px(18.))
                            .rounded_md()
                            .bg(rgb(0xC2694E))
                            .flex()
                            .justify_center()
                            .items_center()
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(rgb(0xFFFFFF))
                                    .child("甲"),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(14.5))
                            .text_color(rgb(INK))
                            .child("帮我配置一个有图形界面的动态壁纸软件"),
                    ),
            )
            .child(div().flex_1())
            .child(
                div()
                    .h_flex()
                    .gap_1p5()
                    .px_2p5()
                    .py_1()
                    .rounded_full()
                    .bg(rgb(BLUE_TINT))
                    .child(
                        Icon::new(IconAll::Folder)
                            .size_3p5()
                            .text_color(rgb(BLUE)),
                    )
                    .child(
                        div()
                            .text_size(px(12.5))
                            .text_color(rgb(BLUE))
                            .child("claudesktop"),
                    ),
            )
            .child(
                div()
                    .h_flex()
                    .gap_1()
                    .px_2p5()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(LINE))
                    .bg(rgb(CARD))
                    .child(
                        Icon::new(IconAll::Bot)
                            .size_3p5()
                            .text_color(rgb(INK_SOFT)),
                    )
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(rgb(INK))
                            .child("GLM-5.3"),
                    )
                    .child(
                        Icon::new(IconAll::ChevronDown)
                            .size_3()
                            .text_color(rgb(INK_SOFT)),
                    ),
            )
            .child(
                div()
                    .h_flex()
                    .gap_1p5()
                    .px_2p5()
                    .py_1()
                    .child(
                        Icon::new(IconAll::Share)
                            .size_3p5()
                            .text_color(rgb(INK_SOFT)),
                    )
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(rgb(INK_SOFT))
                            .child("分享"),
                    ),
            )
    }

    // ---------- 输入区 ----------
    /// 侧栏开关。Windows 上标题栏整条是系统拖拽区，里面的按钮收不到点击，
    /// 所以开关放在侧栏头部（展开）和左侧窄栏（收起）。
    fn sidebar_toggle(&self, id: &'static str, cx: &mut Context<Self>) -> Stateful<Div> {
        div()
            .id(id)
            .size(px(28.))
            .flex_shrink_0()
            .rounded_md()
            .flex()
            .justify_center()
            .items_center()
            .cursor_pointer()
            .hover(|d| d.bg(rgb(SAND_ACTIVE)))
            .on_click(cx.listener(|this, _, _, cx| {
                this.sidebar_open = !this.sidebar_open;
                cx.notify();
            }))
            .child(Icon::new(IconAll::PanelLeft).size_4().text_color(rgb(INK_SOFT)))
    }

    /// 权限模式弹层：锚在输入卡片左下，向上展开。
    fn render_mode_menu(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let mut menu = div()
            .id("mode-menu")
            .occlude()
            .absolute()
            .left(px(44.))
            .bottom(px(104.))
            .w(px(300.))
            .v_flex()
            .p_1()
            .gap_0p5()
            .bg(rgb(CARD))
            .border_1()
            .border_color(rgb(LINE))
            .rounded_lg()
            .shadow_md();
        for (i, (name, desc, icon)) in PERMISSION_MODES.iter().enumerate() {
            let active = self.permission_mode == i;
            menu = menu.child(
                div()
                    .id(("mode-item", i))
                    .h_flex()
                    .items_center()
                    .gap_2p5()
                    .px_2p5()
                    .py_2()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|d| d.bg(rgb(SAND)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.permission_mode = i;
                        this.mode_menu_open = false;
                        cx.notify();
                    }))
                    .child(Icon::new(*icon).size_4().text_color(rgb(INK_SOFT)))
                    .child(
                        div()
                            .v_flex()
                            .flex_1()
                            .child(
                                div()
                                    .text_size(px(14.))
                                    .text_color(rgb(INK))
                                    .child(*name),
                            )
                            .child(
                                div()
                                    .text_size(px(12.5))
                                    .text_color(rgb(INK_SOFT))
                                    .child(*desc),
                            ),
                    )
                    .when(active, |d| {
                        d.child(Icon::new(IconAll::Check).size_4().text_color(rgb(CORAL)))
                    }),
            );
        }
        menu
    }

    fn render_composer(&self, cx: &mut Context<Self>) -> Div {
        let send_bg = if self.busy { rgb(INK_SOFT) } else { rgb(CORAL) };
        let (mode_name, _, mode_icon) = PERMISSION_MODES[self.permission_mode];
        let card = div()
            .id("composer")
            .relative()
            .v_flex()
            .w_full()
            .gap_2()
            .bg(rgb(CARD))
            .border_1()
            .border_color(rgb(LINE))
            .rounded(px(22.))
            .shadow_sm()
            .px_4()
            .pt_3()
            .pb_3()
            .child(Input::new(&self.input).appearance(false).bordered(false))
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .size(px(30.))
                            .flex_shrink_0()
                            .rounded_full()
                            .border_1()
                            .border_color(rgb(LINE))
                            .flex()
                            .justify_center()
                            .items_center()
                            .child(Icon::new(IconAll::Plus).size_4().text_color(rgb(INK_SOFT))),
                    )
                    .child(
                        div()
                            .id("mode-chip")
                            .h_flex()
                            .gap_1p5()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .cursor_pointer()
                            .when(self.mode_menu_open, |d| d.bg(rgb(SAND)))
                            .hover(|d| d.bg(rgb(SAND)))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.mode_menu_open = !this.mode_menu_open;
                                cx.notify();
                            }))
                            .child(
                                Icon::new(mode_icon)
                                    .size_3p5()
                                    .text_color(rgb(INK_SOFT)),
                            )
                            .child(
                                div()
                                    .text_size(px(13.5))
                                    .text_color(rgb(INK_SOFT))
                                    .child(mode_name),
                            )
                            .child(
                                Icon::new(IconAll::ChevronDown)
                                    .size_3()
                                    .text_color(rgb(INK_SOFT)),
                            ),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .h_flex()
                            .gap_1()
                            .px_2()
                            .py_1()
                            .child(
                                div()
                                    .text_size(px(13.5))
                                    .text_color(rgb(INK_SOFT))
                                    .child("GLM-5.3"),
                            )
                            .child(
                                Icon::new(IconAll::ChevronDown)
                                    .size_3()
                                    .text_color(rgb(INK_SOFT)),
                            ),
                    )
                    .child(
                        div()
                            .id("send")
                            .size(px(32.))
                            .flex_shrink_0()
                            .rounded_full()
                            .bg(send_bg)
                            .flex()
                            .justify_center()
                            .items_center()
                            .cursor_pointer()
                            .child(
                                Icon::new(if self.busy {
                                    IconAll::CircleStop
                                } else {
                                    IconAll::ArrowUp
                                })
                                .size_4()
                                .text_color(rgb(0xFFFFFF)),
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                if this.busy {
                                    this.stop(cx);
                                } else {
                                    this.send(window, cx);
                                }
                            })),
                    ),
            )
            .when(self.mode_menu_open, |d| d.child(self.render_mode_menu(cx)));

        div()
            .v_flex()
            .max_w(px(760.))
            .mx_auto()
            .w_full()
            .gap_2p5()
            .child(card)
            .child(
                // 卡片下方：环境 + 项目
                div()
                    .h_flex()
                    .gap_2()
                    .child(Self::chip(IconAll::Monitor, "Local"))
                    .child(Self::chip(IconAll::Folder, "claudesktop")),
            )
    }
}

impl Render for ChatApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title_bar = TitleBar::new()
            .bg(rgb(SAND))
            .child(
                div()
                    .h_flex()
                    .ml_2()
                    .gap_3()
                    .items_center()
                    .child(Icon::new(IconAll::PanelLeft).size_4().text_color(rgb(INK_SOFT)))
                    .child(Icon::new(IconAll::ArrowLeft).size_4().text_color(rgb(INK_FAINT)))
                    .child(Icon::new(IconAll::ArrowRight).size_4().text_color(rgb(INK_FAINT))),
            )
            .child(
                div()
                    .flex_1()
                    .h_flex()
                    .justify_center()
                    .child(
                        div()
                            .h_flex()
                            .gap_0p5()
                            .p(px(3.))
                            .rounded_lg()
                            .bg(rgb(SAND_ACTIVE))
                            .child(Self::top_tab("Chat", false))
                            .child(Self::top_tab("Cowork", false))
                            .child(Self::top_tab("Code", true)),
                    ),
            );

        // 自动跟随滚动：在渲染后把滚动钉到底部
        if self.follow_scroll {
            self.scroll.scroll_to_bottom();
        }

        let sidebar = if self.sidebar_open {
            self.render_sidebar(cx)
        } else {
            v_flex()
                .w(px(48.))
                .flex_shrink_0()
                .h_full()
                .items_center()
                .pt_3()
                .bg(rgb(SAND))
                .border_r_1()
                .border_color(rgb(LINE))
                .child(self.sidebar_toggle("sidebar-toggle-collapsed", cx))
        };
        let composer_area = div().px_6().pb_4().pt_1().child(self.render_composer(cx));

        let main = if self.selected.is_none() {
            // 首页：问候 + 用量概览
            v_flex()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .id("home-scroll")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .child(div().h_full().v_flex().child(self.render_overview(cx))),
                )
                .child(composer_area)
        } else {
            let mut list = v_flex()
                .max_w(px(760.))
                .mx_auto()
                .w_full()
                .py_6()
                .px_6()
                .gap_4();
            for (idx, item) in self.transcript.iter().enumerate() {
                list = list.child(self.render_item(idx, item, cx));
            }

            let scroll = div()
                .id("transcript")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .child(list);

            v_flex()
                .flex_1()
                .min_w_0()
                .child(self.render_top_bar())
                .child(scroll)
                .child(composer_area)
        };

        div()
            .v_flex()
            .size_full()
            .font_family(SANS)
            .text_size(px(15.))
            .bg(rgb(PAPER))
            .text_color(rgb(INK))
            .child(title_bar)
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(sidebar)
                    .child(main.h_full()),
            )
    }
}

fn main() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::AllAssets)
        .run(move |cx| {
            gpui_kit::init(cx);

            let mut options = TitleBar::window_options();
            options.window_bounds = Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(1280.), px(820.)),
                cx,
            )));
            if let Some(tb) = options.titlebar.as_mut() {
                tb.title = Some("claudesktop · M1 UI 预览".into());
            }

            gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| {
                    let app = ChatApp::new(window, cx);
                    // 延迟 1.5s 触发自动演示，避开启动阶段的窗口初始化。
                    cx.spawn(async move |this, cx| {
                        cx.background_executor()
                            .timer(Duration::from_millis(1500))
                            .await;
                        let _ = this.update(cx, |app, cx| app.autodemo_hook(cx));
                    })
                    .detach();
                    app
                })
            })
            .expect("Failed to open window");
            cx.activate(true);
        });
}

//! M1 UI 外壳（内核未接入 · 演示回放）
//!
//! 目标界面：Claude 桌面风（暖纸底 + 赤陶橙），含
//! - 自绘标题栏 + 侧栏（工作区分组 / 任务列表 / 内核卡）
//! - 会话视图：左缘头像轨道、思考块、工具卡、计划、流式 Markdown
//! - 空会话首页：问候语 + 用量概览（统计卡 + 热力图）
//! - 底部输入区：工作区 / 模式 chips、圆角输入条、权限模式栏

use gpui_kit::assets::IconName as IconAll;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::text::TextView;
use gpui_kit::component::*;
use gpui_kit::*;
use std::time::Duration;

// ---------- 色板（Claude 品牌基准：暖纸底 + 赤陶橙） ----------
const PAPER: u32 = 0xFAF9F5;
const SAND: u32 = 0xF0EEE6;
const CORAL: u32 = 0xD97757;
const CORAL_DEEP: u32 = 0xBF6449;
const CORAL_TINT: u32 = 0xFBEFE9;
const SAND_ACTIVE: u32 = 0xE8E3D6;
const INK: u32 = 0x3D3929;
const INK_SOFT: u32 = 0x87847C;
const INK_FAINT: u32 = 0xB5B0A4;
const LINE: u32 = 0xE5E1D5;
const CARD: u32 = 0xFFFFFF;
const GREEN: u32 = 0x7A9A65;
const BLUE: u32 = 0x5B6FA8;
const BLUE_TINT: u32 = 0xE9EEF7;
const MONO: &str = "Consolas";

// ---------- 数据模型 ----------
#[derive(Clone, Copy, PartialEq)]
enum ToolPhase {
    Running,
    Done,
}

struct ToolCard {
    icon: IconAll,
    title: &'static str,
    detail: SharedString,
    phase: ToolPhase,
}

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
    BeginTool(IconAll, &'static str, &'static str),
    ToolDone(&'static str),
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

    // 2) 工具卡：读文件
    s.push(MockStep(
        450,
        MockAction::BeginTool(
            IconAll::FileText,
            "Read  crates/core/src/lib.rs",
            "读取内核抽象定义",
        ),
    ));
    s.push(MockStep(
        850,
        MockAction::ToolDone("128 行 · Kernel trait 与 KernelEvent 事件模型"),
    ));

    // 3) 工具卡：构建
    s.push(MockStep(
        500,
        MockAction::BeginTool(
            IconAll::SquareTerminal,
            "$ cargo build -p claudesktop",
            "增量编译桌面应用",
        ),
    ));
    s.push(MockStep(
        950,
        MockAction::ToolDone("Finished in 1.8s · 0 错误 0 警告"),
    ));

    // 4) 计划卡
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

    // 5) 流式 Markdown 正文
    s.push(MockStep(350, MockAction::BeginAgent));
    s.extend(stream_steps(
        "## UI 外壳验收清单\n\n这轮回复来自**内置演示回放**——UI 已就位，内核按计划下一步接入 ACP。\n\n你在上面看到的消息类型：\n\n- **思考块**：可折叠，流式追加\n- **工具卡片**：读文件 / 跑构建，带「运行中 → 完成」状态\n- **计划**：左缘赤陶色标记的清单，逐项点亮\n- **Markdown 正文**：就是本块，`stream_fade` 流式渲染\n\n接入后，这里的事件流将直接映射自 ACP 的 `session/update` 通知，UI 不再需要任何演示脚本。\n\n> 布局 / 配色 / 交互有任何不顺眼的地方，现在改最便宜。",
        4,
        30,
        MockAction::AgentChunk,
    ));
    s.push(MockStep(250, MockAction::EndAgent));

    // 6) 回合小结
    s.push(MockStep(
        400,
        MockAction::Note("演示回合结束 · 内核未连接 · 下一步：ACP 接入"),
    ));
    s
}

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
        }
    }

    /// 调试钩子：CLAUDESKTOP_AUTODEMO=1 时自动触发一次演示回放，
    /// 用于无人工交互地复现回放链路的崩溃。
    fn autodemo_hook(&mut self, cx: &mut Context<Self>) {
        if std::env::var("CLAUDESKTOP_AUTODEMO").ok().as_deref() != Some("1") {
            return;
        }
        if let Some(first) = self.workspaces.first() {
            if let Some(s) = first.sessions.first() {
                self.transcript.push(ChatItem::User(s.title.clone()));
            }
        }
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
            MockAction::BeginTool(icon, title, detail) => {
                self.transcript.push(ChatItem::Tool(ToolCard {
                    icon: *icon,
                    title,
                    detail: (*detail).into(),
                    phase: ToolPhase::Running,
                }));
            }
            MockAction::ToolDone(detail) => {
                if let Some(ChatItem::Tool(card)) = self.transcript.last_mut() {
                    card.detail = (*detail).into();
                    card.phase = ToolPhase::Done;
                }
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
    fn avatar_mark() -> Div {
        div()
            .size(px(22.))
            .flex_shrink_0()
            .rounded_full()
            .bg(rgb(CORAL))
            .flex()
            .justify_center()
            .items_center()
            .child(
                Icon::new(IconAll::Sparkles)
                    .size(px(11.))
                    .text_color(rgb(0xFFFFFF)),
            )
    }

    fn chip(icon: IconAll, label: impl Into<SharedString>) -> Div {
        div()
            .h_flex()
            .gap_1_5()
            .px_2_5()
            .py_1_5()
            .rounded_md()
            .border_1()
            .border_color(rgb(LINE))
            .bg(rgb(CARD))
            .child(
                Icon::new(icon)
                    .size_3_5()
                    .text_color(rgb(INK_SOFT)),
            )
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(rgb(INK))
                    .child(label.into()),
            )
    }

    fn seg(text: &'static str, active: bool) -> Div {
        div()
            .px_3()
            .py_1()
            .rounded_md()
            .text_size(px(12.))
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
            .gap_1_5()
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(ChatItem::Thinking { open, .. }) = this.transcript.get_mut(idx) {
                    *open = !*open;
                    cx.notify();
                }
            }))
            .child(
                Icon::new(IconAll::Brain)
                    .size_3_5()
                    .text_color(rgb(INK_SOFT)),
            )
            .child(
                div().text_size(px(11.5)).text_color(rgb(INK_SOFT)).child(if streaming {
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
                    .text_size(px(12.))
                    .text_color(rgb(INK_SOFT))
                    .line_height(relative(1.6))
                    .child(format!("{text}{caret}")),
            );
        }
        col
    }

    fn render_tool(card: &ToolCard) -> Div {
        let (status_icon, status_text, status_color) = match card.phase {
            ToolPhase::Running => (IconAll::LoaderCircle, "运行中", INK_SOFT),
            ToolPhase::Done => (IconAll::Check, "完成", GREEN),
        };
        div()
            .h_flex()
            .gap_2_5()
            .p_2_5()
            .rounded_lg()
            .bg(rgb(CARD))
            .border_1()
            .border_color(rgb(LINE))
            .child(
                div()
                    .size(px(28.))
                    .flex_shrink_0()
                    .rounded_md()
                    .bg(rgb(CORAL_TINT))
                    .flex()
                    .justify_center()
                    .items_center()
                    .child(Icon::new(card.icon).size_4().text_color(rgb(CORAL))),
            )
            .child(
                div()
                    .v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.))
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(rgb(INK))
                            .font_family(MONO)
                            .child(card.title),
                    )
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(rgb(INK_SOFT))
                            .child(card.detail.clone()),
                    ),
            )
            .child(
                div()
                    .h_flex()
                    .gap_1()
                    .flex_shrink_0()
                    .child(
                        Icon::new(status_icon)
                            .size_3_5()
                            .text_color(rgb(status_color)),
                    )
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(rgb(status_color))
                            .child(status_text),
                    ),
            )
    }

    fn render_plan(rows: &[PlanRow]) -> Div {
        div()
            .v_flex()
            .gap_1_5()
            .p_3()
            .rounded_lg()
            .bg(rgb(CARD))
            .border_1()
            .border_color(rgb(LINE))
            .child(
                div()
                    .h_flex()
                    .gap_1_5()
                    .child(
                        Icon::new(IconAll::ListTodo)
                            .size_4()
                            .text_color(rgb(CORAL)),
                    )
                    .child(div().text_size(px(13.)).text_color(rgb(INK)).child("计划")),
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
                            .text_size(px(12.5))
                            .text_color(if row.done {
                                rgb(INK_SOFT)
                            } else {
                                rgb(INK)
                            })
                            .child(row.text),
                    )
            }))
    }

    /// 助手回合行：左侧头像轨道 + 内容。
    fn railed(content: impl IntoElement) -> Div {
        div()
            .h_flex()
            .items_start()
            .gap_3()
            .child(Self::avatar_mark())
            .child(div().flex_1().min_w_0().child(content))
    }

    fn render_item(&self, idx: usize, item: &ChatItem, cx: &mut Context<Self>) -> Div {
        match item {
            ChatItem::User(text) => div().h_flex().justify_end().child(
                div()
                    .max_w(relative(0.78))
                    .bg(rgb(SAND_ACTIVE))
                    .text_color(rgb(INK))
                    .rounded(px(12.))
                    .px_3_5()
                    .py_2()
                    .text_size(px(14.))
                    .line_height(relative(1.55))
                    .child(text.clone()),
            ),
            ChatItem::Thinking {
                text,
                streaming,
                open,
            } => Self::railed(Self::render_thinking(idx, text, *streaming, *open, cx)),
            ChatItem::Tool(card) => Self::railed(Self::render_tool(card)),
            ChatItem::Plan(rows) => Self::railed(Self::render_plan(rows)),
            ChatItem::Agent(text) => Self::railed(
                TextView::markdown(("agent", idx), SharedString::from(text.clone()))
                    .stream_fade(true),
            ),
            ChatItem::Note(text) => {
                div().h_flex().justify_center().pt_1().child(
                    div()
                        .text_size(px(11.))
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
                    .gap_2_5()
                    .items_center()
                    .child(
                        Icon::new(IconAll::Sparkles)
                            .size_5()
                            .text_color(rgb(CORAL)),
                    )
                    .child(
                        div()
                            .text_size(px(22.))
                            .font_weight(FontWeight::SEMIBOLD)
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
                                                    .text_size(px(11.5))
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
                                                    .text_size(px(11.5))
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
                            .text_size(px(12.))
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
                        .text_size(px(11.))
                        .text_color(rgb(INK_SOFT))
                        .child("工作区"),
                )
                .child(div().flex_1())
                .child(
                    Icon::new(IconAll::ArrowUpDown)
                        .size_3_5()
                        .text_color(rgb(INK_SOFT)),
                )
                .child(
                    div().pl_2().child(
                        Icon::new(IconAll::ExternalLink)
                            .size_3_5()
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
                    .text_size(px(12.5))
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
                            .text_size(px(13.))
                            .text_color(if active { rgb(INK) } else { rgb(INK_SOFT) })
                            .child(s.title.clone()),
                    );
                if !s.age.is_empty() {
                    row = row.child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(11.))
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
                .text_size(px(11.))
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
                            .text_size(px(13.))
                            .text_color(rgb(INK_SOFT))
                            .child(SharedString::from(*title)),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(11.))
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
                            .text_size(px(15.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(INK))
                            .child("Claude"),
                    ),
            )
            .child(
                div()
                    .id("new-chat")
                    .h_flex()
                    .gap_2()
                    .mx_2()
                    .mt_2()
                    .px_2()
                    .py_1_5()
                    .rounded_md()
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.selected = None;
                        this.transcript.clear();
                        cx.notify();
                    }))
                    .child(
                        Icon::new(IconAll::SquarePen)
                            .size_3_5()
                            .text_color(rgb(INK)),
                    )
                    .child(div().text_size(px(13.)).text_color(rgb(INK)).child("新会话")),
            )
            .child(
                div()
                    .id("plugins")
                    .h_flex()
                    .gap_2()
                    .mx_2()
                    .px_2()
                    .py_1_5()
                    .rounded_md()
                    .cursor_pointer()
                    .child(
                        Icon::new(IconAll::Blocks)
                            .size_3_5()
                            .text_color(rgb(INK_SOFT)),
                    )
                    .child(div().text_size(px(13.)).text_color(rgb(INK_SOFT)).child("插件")),
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
                                    .size_3_5()
                                    .text_color(rgb(INK_SOFT)),
                            )
                            .child(
                                div()
                                    .text_size(px(12.5))
                                    .text_color(rgb(INK_SOFT))
                                    .child("更多"),
                            ),
                    )
                    .child(
                        div()
                            .h_flex()
                            .gap_1_5()
                            .items_center()
                            .child(
                                div().size(px(6.)).rounded_full().bg(rgb(CORAL)),
                            )
                            .child(
                                div()
                                    .text_size(px(11.))
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
            .gap_2_5()
            .px_5()
            .py_2_5()
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
                                    .text_size(px(10.))
                                    .text_color(rgb(0xFFFFFF))
                                    .child("甲"),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(13.5))
                            .text_color(rgb(INK))
                            .child("帮我配置一个有图形界面的动态壁纸软件"),
                    ),
            )
            .child(div().flex_1())
            .child(
                div()
                    .h_flex()
                    .gap_1_5()
                    .px_2_5()
                    .py_1()
                    .rounded_full()
                    .bg(rgb(BLUE_TINT))
                    .child(
                        Icon::new(IconAll::Folder)
                            .size_3_5()
                            .text_color(rgb(BLUE)),
                    )
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(rgb(BLUE))
                            .child("claudesktop"),
                    ),
            )
            .child(
                div()
                    .h_flex()
                    .gap_1()
                    .px_2_5()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(LINE))
                    .bg(rgb(CARD))
                    .child(
                        Icon::new(IconAll::Bot)
                            .size_3_5()
                            .text_color(rgb(INK_SOFT)),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
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
                    .gap_1_5()
                    .px_2_5()
                    .py_1()
                    .child(
                        Icon::new(IconAll::Share)
                            .size_3_5()
                            .text_color(rgb(INK_SOFT)),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(INK_SOFT))
                            .child("分享"),
                    ),
            )
    }

    // ---------- 输入区 ----------
    fn render_composer(&self, cx: &mut Context<Self>) -> Div {
        div()
            .v_flex()
            .max_w(px(760.))
            .mx_auto()
            .w_full()
            .gap_2()
            .child(
                // chips 行
                div()
                    .h_flex()
                    .gap_2()
                    .child(Self::chip(IconAll::Folder, "claudesktop"))
                    .child(Self::chip(IconAll::Gauge, "标准模式")),
            )
            .child(
                // 输入条
                div()
                    .id("composer")
                    .w_full()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .bg(rgb(CARD))
                    .border_1()
                    .border_color(rgb(LINE))
                    .rounded(px(16.))
                    .px_4()
                    .py_2()
                    .child(
                        Icon::new(IconAll::MessageSquare)
                            .size_4()
                            .text_color(rgb(INK_SOFT)),
                    )
                    .child(Input::new(&self.input).appearance(false).bordered(false))
                    .child(
                        div()
                            .id("send")
                            .size(px(26.))
                            .flex_shrink_0()
                            .rounded_full()
                            .bg(if self.busy { rgb(INK_SOFT) } else { rgb(CORAL) })
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
                                .size_3_5()
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
            .child(
                // 权限模式行
                div()
                    .h_flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(INK_SOFT))
                            .child("Read"),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(INK_SOFT))
                            .child("Edit"),
                    )
                    .child(
                        div()
                            .px_2_5()
                            .py_1()
                            .rounded_md()
                            .bg(rgb(CORAL_TINT))
                            .text_size(px(12.))
                            .text_color(rgb(CORAL_DEEP))
                            .child("Yolo"),
                    )
                    .child(
                        Icon::new(IconAll::Plus)
                            .size_3_5()
                            .text_color(rgb(INK_SOFT)),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(INK_SOFT))
                            .child("GLM-5.3"),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(INK_FAINT))
                            .child("Default"),
                    ),
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
                    .gap_1_5()
                    .items_center()
                    .child(div().size(px(9.)).rounded_full().bg(rgb(CORAL)))
                    .child(
                        div()
                            .text_size(px(12.5))
                            .text_color(rgb(INK))
                            .child("claudesktop"),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .h_flex()
                    .justify_center()
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(rgb(INK_SOFT))
                            .child("M1 · UI 外壳预览（未接内核）"),
                    ),
            );

        // 自动跟随滚动：在渲染后把滚动钉到底部
        if self.follow_scroll {
            self.scroll.scroll_to_bottom();
        }

        let sidebar = self.render_sidebar(cx);
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
            .bg(rgb(PAPER))
            .text_color(rgb(INK))
            .child(title_bar)
            .child(h_flex().flex_1().min_h_0().child(sidebar).child(main))
    }
}

fn main() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
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

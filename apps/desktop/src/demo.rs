//! 演示回放脚本（CLAUDESKTOP_KERNEL=demo 或内核启动失败时使用）。

use crate::model::*;

pub enum MockAction {
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

pub struct MockStep(pub u64, pub MockAction);

/// 把长文本按固定字数切成流式 chunk 步骤。
pub fn stream_steps(
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

pub const DEMO_DIFF: DiffRows = &[
    (' ', "pub trait Kernel: Send {"),
    ('-', "    fn send(&mut self, text: String);"),
    ('+', "    fn send(&mut self, text: String) -> Result<()>;"),
    ('+', "    fn cancel(&mut self);"),
    (' ', "}"),
];

pub fn build_demo_script() -> Vec<MockStep> {
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

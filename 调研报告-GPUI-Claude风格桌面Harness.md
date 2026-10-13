# 用 GPUI 构建高性能 Claude 风格桌面 Harness（内核可更换）· 调研报告

> 调研时间：2026 年 10 月 · 目标：在 Windows 上用 Rust + GPUI 构建一个 Claude 风格的桌面 AI harness，聊天 UI 固定，底层 agent 内核（引擎/模型后端）可更换。
>
> 说明：标注 ✅ 的为一手验证事实（直接查阅 crates.io / 官方站点 / 官方仓库），标注 🔶 的为合理推断或二手信息。

---

## 执行摘要（TL;DR）

1. **技术可行性：中上，有直接先例。** GPUI 已发布到 crates.io（0.2.2，Apache-2.0），Zed for Windows 已于 2025-10-15 发布稳定版（DirectX 11 + DirectWrite，2026-04 Zed 1.0 时 Windows 为完全支持平台）。与本项目同类的先例：Zed 自家 **Delta**（AI/PR 审查应用）、第三方 **waku.sh**（coding-agent GUI，直接对标 Electron 版 Claude 桌面端）、**zedra.dev**（多 agent 客户端）。✅
2. **不用从零造轮子。** 第三方组件库 **gpui-kit**（原 gpui-component，Longbridge 出品，17k stars，v0.7.3，Apache-2.0，0.7.0→0.7.3 三周一波）提供了 Claude 风格聊天 UI 需要的几乎全部基础设施：原生 Markdown/HTML 渲染、虚拟列表（动态高度）、Dock 布局、代码编辑器（Tree-sitter 高亮 + LSP）、输入框、主题系统，且经过商业桌面产品（Longbridge Pro）生产验证。✅
3. **「内核可更换」有现成标准答案：ACP（Agent Client Protocol）**。由 Zed + JetBrains 联合推动，JSON-RPC over stdio，专为「客户端 ↔ coding agent」解耦设计，官方 Rust crate `agent-client-protocol`（3.3.0，Apache-2.0，518 万+ 下载）。走 ACP 路线意味着你的 UI 天然能接 Claude Code（经 claude-code-acp 适配器）、Gemini CLI、Goose、opencode 等，同时用 Rust trait 内置一个自研内核。✅
4. **许可证无障碍（有一条红线）。** gpui crate 与 gpui-kit、agent-client-protocol 均为 Apache-2.0，闭源商业应用可用。红线：Zed 仓库整体 GPL-3.0-or-later，其 markdown/ui/editor 等应用层 crate 不可抄进闭源项目（Markdown 渲染等能力用 gpui-kit 的 Apache-2.0 等价物）。另有 Claude Agent SDK 计费红线（见 3.4）。✅
5. **主要风险（按严重度）：** ① **上游依赖 Zed 的优先级**：2026-02 Zed 官宣 GPUI「刹车」——只做 Zed 编辑器需要的事，crates.io 官方发布已停滞一年（0.2.2 = 2025-10），生态靠 gpui-pre 快照绕行；② **Windows IME**：核心修复（2025-11-05）晚于 0.2.2 发布，crates.io 版本含已知 IME bug，微软拼音大概率正常、搜狗拼音被 Zed 贡献者点名「无能为力」；③ 0.x 阶段 breaking changes 是常态。**对策：主干走 gpui-kit（pin 了较新 gpui-pre 快照）、锁死版本、第一个里程碑实测 IME。** ✅

**推荐技术栈（一句话）：`gpui-kit 0.7`（UI 层） + `agent-client-protocol`（内核边界） + 内置自研 Rust 内核（provider 层先做 OpenAI 兼容 API，再加 Anthropic 原生 SSE）+ 可选外挂 Claude Code 等 ACP agent 子进程。**

---

## 一、GPUI 框架调研

### 1.1 是什么

GPUI 是 Zed 团队开发的 Rust GPU 加速 UI 框架，hybrid immediate/retained mode（保留模式状态树 + 每帧重建元素树），自绘 UI、不嵌浏览器、不用系统原生控件。渲染后端：macOS 用 Metal、**Windows 用 DirectX 11**、Linux 用 wgpu；Linux 窗口层支持 X11/Wayland。✅（来源：[gpui.rs](https://gpui.rs)）

### 1.2 现状与成熟度（关键事实）

| 事实 | 数据 | 来源 |
| --- | --- | --- |
| crates.io 发布 | `gpui` 最新 **0.2.2**，发布于 2025-10-22（Zed 团队成员发布）；**此后一年无任何新发布** | [crates.io/gpui](https://crates.io/crates/gpui) ✅ |
| 下载量 | 总计 35.3 万；近 90 天 19.7 万 | 同上 ✅ |
| 版本史 | 仅 7 个版本：2022 年第三方 8 行占位 0.1.0（MIT，已 yank）→ 官方 2025-10 密集发布 0.2.0/0.2.1/0.2.2 → 停滞 | 同上 ✅ |
| API 稳定性 | 官方 README 原话："still pre-1.0. There will often be breaking changes between versions" | [gpui README](https://github.com/zed-industries/zed/blob/main/crates/gpui/README.md) ✅ |
| 发布滞后 | main 分支已拆出 `gpui_platform` crate（窗口/文本后端），但 crates.io 上**不存在**（404）；Zed 自己周更（stable 1.18.x），gpui 演进只跟随 Zed 需求 | 同上 ✅ |
| 2026-02「刹车」 | Zed 团队成员 Discord 原话："GPUI development is getting some major brakes... pushing off anything that isn't directly related to Zed's use case"；社区 PR、custom shaders 被搁置。GPUI 本体仍随 Zed 活跃，但只服务编辑器 | [alt HN 存档](https://althacker.news/item?id=47003569) ✅ |
| 平台里程碑 | Zed for Windows 稳定版 2025-10-15；**Zed 1.0 于 2026-04-29 发布，Windows 为完全支持平台** | [Zed is 1.0](https://zed.dev/blog/zed-1-0) ✅ |

**独立应用先例（均非 Zed 编辑器）** ✅：

1. **Delta**（[delta.dev](https://delta.dev)）— Zed Industries 自家第二款 GPUI 产品，AI/PR 审查应用，2026-09-16 公测 —— **与本项目最接近的官方先例**（gpui.rs 官网已与 Zed 并列展示 "From the creators of Zed & Delta"）。
2. **waku.sh** — Rust+GPUI 的 coding-agent GUI，作者明言对标 Electron 版 ChatGPT/Claude 桌面端（[Show HN](https://hn.nuxt.dev/item/49315709)：用户实测 "super smooth"、~2ms 输入延迟、150–300MB 内存 vs Electron 650MB+）。
3. **zedra.dev** — GPUI 构建的多 agent 客户端（同上讨论区提及）。
4. **Longbridge Pro** — 券商商业桌面（gpui-kit 来源）。
5. crates.io 上 gpui 有 **138 个反向依赖**：gpui-router、gpui-terminal、gpui-video-player、fluix、swr-gpui、gpui-tokio-bridge 等。

**解读：** gpui 是「Zed 优先、外部可用」的策略，且 2026-02 后明确收窄为只服务 Zed 需求。crates.io 官方渠道事实上已废弃，**生态靠 gpui-pre 快照绕行** —— 用 gpui-kit 这类 pin 快照的组件库做主干，比自己裸依赖 gpui 更稳（见 1.6）。

### 1.3 Windows 支持（用户主平台，重点：IME）

- **Zed for Windows 稳定版 2025-10-15 发布**：DirectX 11 渲染 + DirectWrite 文本渲染，设全职 Windows 团队（Windows platform lead @localcc）；2026-04-29 Zed 1.0 发布时 Windows 为完全支持平台。✅ 来源：[Windows When? Windows Now](https://zed.dev/blog/zed-for-windows-is-here)、[Zed is 1.0](https://zed.dev/blog/zed-1-0)
- **中文 IME 时间线（对聊天应用至关重要）** ✅：
  - 核心问题：拼音组词时 **Enter/Backspace 被 Zed 截获**而非交给 IME —— issue [#36736](https://github.com/zed-industries/zed/issues/36736)（2025-08-22）；根因是 `platform/windows/events.rs` 对 off-the-spot IME 的 `TranslateMessage`/`VK_PROCESSKEY` 处理错误
  - 修复：commit `2ead8c4` "Improve Windows text input for international keyboard layouts and IMEs"，**2025-11-05** 关闭该 issue；2025-12、2026-05 仍有跟进
  - ⚠️ **crates.io 的 0.2.2 发布于 2025-10-22，早于修复两周** —— 即 crates.io 版本含已知 IME bug；**走 gpui-pre 快照（gpui-kit 路线）才有修复**
  - 搜狗拼音被 Zed 贡献者点名「不遵循标准 Windows IME 流程，我们无能为力」—— **微软拼音应正常，搜狗用户可能仍有残留问题**
  - gpui-kit 仓库另有多个 IME 组合窗口/候选框定位修复 PR（[#2613](https://github.com/longbridge/gpui-kit/pull/2613)、[#3297](https://github.com/longbridge/gpui-kit/pull/3297)）
  - **结论：M0 里程碑必须实测（微软拼音 + 搜狗双测）**
- **高 DPI / 多显示器**：API 有 Pixels/ScaledPixels/scale_factor 区分；GA 博客把「多显示器、120–144Hz」列为待反馈项（2025-10 时点）。✅

### 1.4 核心 API 心智模型（基于官方 examples 站）

来源：[gpui.rs/examples](https://gpui.rs/examples) ✅

- **View + Render**：app 打开窗口 → 根 view 是实现 `Render` trait 的 Rust struct；`render(&mut self, window, cx)` 每次构建一棵 element 树。类似 React 的组件模型。
- **状态更新**：事件处理器改状态后必须调 `cx.notify()` 请求重渲染（改字段本身不触发）。
- **样式**：Tailwind 风格的链式方法：`div().flex().flex_col().gap_3().justify_center().items_center()`，支持 grid、container query 等。不是完整 CSS 兼容，但足够构建真实应用。
- **交互**：鼠标事件直接跑 Rust 闭包（无浏览器-RPC 边界）；快捷键用 `actions!` 宏 + `KeyBinding` + `key_context` 作用域。
- **异步**：慢 I/O 不放 UI 线程，用 background executor / `cx.spawn`。
- **无障碍**：基于 AccessKit，自定义控件需自己暴露 a11y 树。
- Hello world 大约 50 行（见官网首页完整样例）。

最小 Counter 示例（官方）：

```rust
impl Render for Counter {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .child(format!("Count: {}", self.count))
            .child(
                div()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.count += 1;
                        cx.notify();
                    }))
                    .child("Add"),
            )
    }
}
```

### 1.5 Zed 仓库作为「参考架构」（重要旁证）

Zed workspace（200+ crates）本身就包含一个完整的 Claude 风格 agent harness，其 crate 划分值得照抄思路：✅

```
agent / agent_ui / agent_settings / agent_servers / agent_skills   ← agent 编排与 UI
acp_thread / acp_tools                                             ← ACP 协议接入层
anthropic / open_ai / ollama / deepseek / google_ai / open_router /
lmstudio / bedrock / opencode                                      ← provider 适配层
markdown / markdown_preview                                        ← Markdown 渲染
editor / terminal / terminal_view / git_ui / project               ← 本地能力
theme / ui / icons                                                 ← 设计系统
```

注意：这些应用层 crate **不发布到 crates.io** 且遵循 Zed 编辑器整体许可（GPL 系），只能当「架构参考」读，不能抄代码进闭源项目；但同样的能力在 gpui-kit 里几乎都有 Apache-2.0 等价物。✅/🔶

### 1.6 生态与组件库：gpui-kit（本项目的最大杠杆）

[longbridge/gpui-kit](https://github.com/longbridge/gpui-kit)（17k stars，v0.7.3，Apache-2.0，文档站 [gpui-kit.com](https://gpui-kit.com) 有中文版）✅：

- **维护活跃度**：0.7.0→0.7.3 三周一波（0.7.3 发布于 2026-10-12）；crates.io 总下载 23.4 万/近 90 天 16.5 万；维护者 huacnlee（Jason Lee，[Zed 官方社区 champion](https://zed.dev/blog/community-champion-jason-lee)）。

- **75+ 组件**：表单、导航、覆盖层、数据展示、编辑、反馈、布局；shadcn/ui 风格的完整视觉系统 + 语义主题。
- **对本项目最关键的能力**：
  - **原生 Markdown 渲染**（含语法高亮），还有 HTML 渲染、图表 —— 聊天消息渲染直接可用；
  - **虚拟列表支持动态高度 item** —— 流式聊天长会话的核心；
  - **Dock 布局**（可调面板、拖拽标签页、嵌套分割、可序列化）—— IDE 式 harness 布局；
  - **代码编辑器**（Tree-sitter 高亮、LSP 诊断/补全，20 万行稳定）—— diff 查看/artifact 编辑；
  - 多行输入、下拉、通知、Tooltip 等常规组件。
- **生产验证**：Longbridge Pro（公开商业桌面产品）第一天起基于它构建。
- **工程质量**：AccessKit 无障碍、headless 窗口 UI 集成测试、WebAssembly 支持、120FPS 性能目标、可选 `gpui-fast` 保留式渲染引擎（longbridge 的 GPUI fork）。
- **分发配套**：Packaging / Auto Update / WebView / i18n / System Notification 文档齐全。
- **对 AI 编程友好**：官方提供 Claude Code 等 agent 可安装的 skill（`npx skills add longbridge/gpui-kit`）—— 你可以直接让 Claude Code 带着组件目录知识来写这个项目。
- **GPUI 版本策略**：gpui-kit pin 上游 GPUI 快照（`gpui-pre` 系列，当前 =0.3.9）并 re-export，应用只需依赖 `gpui-kit = "0.7"` 一个 crate。若要裸依赖 GPUI，写法为 `gpui = { package = "gpui-pre", version = "=0.3.9" }`。✅

### 1.7 许可证结论

| 组件 | 许可证 | 闭源商业可用 |
| --- | --- | --- |
| gpui（crates.io） | Apache-2.0 | ✅ |
| gpui-kit / gpui-base / gpui-shell | Apache-2.0（文档 CC BY 4.0） | ✅ |
| agent-client-protocol（Rust crate） | Apache-2.0 | ✅ |
| Zed 编辑器应用层代码（agent_ui、markdown crate 等） | 随 Zed 仓库（GPL 系） | ❌ 只可参考思路 |

### 1.8 已知痛点（社区反馈汇总）

1. **2026-02「刹车」事件（最重要）**：Zed 团队成员 Discord 原话 —— "GPUI development is getting some major brakes... pushing off anything that isn't directly related to Zed's use case"。社区 PR、custom shaders 等非 Zed 需求被搁置。GPUI 本体仍随 Zed 周更活跃，但只服务编辑器。✅（[alt HN 存档](https://althacker.news/item?id=47003569)）
2. **官方发布渠道基本废弃**：crates.io 一年未更新；`gpui_platform` 拆分未发布且被指「为 Zed 编译时间服务」；README 自认 "the best way to learn about these APIs is to read the Zed source code"。✅
3. **生态碎片化**：gpui（官方 crates.io，停滞）/ gpui-pre（gpui-kit 的快照）/ gpui-fast（longbridge fork）/ gpui-ce（社区分叉）/ gpui-standalone 并存，社区甚至做了 Fork Map 对比 API 漂移。gpui-ce 由前员工建立（"mostly API compatible... but that is changing"），但发起人公开表示前景存疑、合并活动少。✅
4. **Windows IME**：见 1.3 —— 修复在快照里不在已发布 crate；搜狗拼音存疑。✅
5. **正反声音都有**：正面 —— waku.sh 用户实测 "super smooth, code super easy to understand"、~2ms 输入延迟、150–300MB 内存（vs Electron 650MB+）、Syntax 播客好评；负面 —— dev.to（2026-10）结论 "Most people should not build a product on GPUI right now"（针对 API 稳定性而非性能），作者因 pinned commit 漂移被迫迁 Iced。✅
6. **文档**：无完整官方 reference；主要靠 examples + gpui-kit 文档站（有中文）。✅

**备选方案（若最终更看重 API 长期稳定与文档）**：Tauri（Web UI + Rust 后端，成熟度最高）/ Iced / Dioxus。建议读完 [GPUI: The Electron Killer That Never Left Home](https://dev.to/dishant0406/gpui-by-zed-the-electron-killer-that-never-left-home-209o) 这篇降温文后再做最终决定。本报告推荐 GPUI 路线的前提是：接受「带组件库的框架」预期 + 用 gpui-kit 做主干 + 锁版本。

### 1.9 学习路径与资源（按 usefulness 排序）

1. [gpui.rs](https://gpui.rs) 首页 hello world → [examples 教程](https://gpui.rs/examples)（Views/样式/事件/Actions）
2. [gpui README](https://github.com/zed-industries/zed/blob/main/crates/gpui/README.md) + [官方 examples 目录](https://github.com/zed-industries/zed/tree/main/crates/gpui/examples)（24 个：hello_world、input、text、svg、image、tree、uniform_list、list_example、grid_layout、a11y、shadow、system_notifications…）
3. [docs.rs/gpui](https://docs.rs/gpui/latest/gpui/) API 参考 + [contexts.md](https://github.com/zed-industries/zed/blob/main/crates/gpui/docs/contexts.md) / [key_dispatch.md](https://github.com/zed-industries/zed/blob/main/crates/gpui/docs/key_dispatch.md) 专题文档
4. [hedge-ops/gpui-tutorial](https://github.com/hedge-ops/gpui-tutorial) — 社区手把手教程
5. [gpui-kit.com](https://gpui-kit.com/docs/getting-started)（中文版）→ 组件目录 → Coding Guides；AI skill：`npx skills add longbridge/gpui-kit`
6. Zed 博客：[GPUI ownership](https://zed.dev/blog/gpui-ownership)、[Zed is 1.0](https://zed.dev/blog/zed-1-0)、[Windows GA](https://zed.dev/blog/zed-for-windows-is-here)
7. 同类应用参考：[Delta](https://delta.dev)（Zed 自家 AI 应用）、[waku.sh Show HN](https://hn.nuxt.dev/item/49315709)
8. 批判视角（必读降温文）：[GPUI: The Electron Killer That Never Left Home](https://dev.to/dishant0406/gpui-by-zed-the-electron-killer-that-never-left-home-209o)
9. 读 Zed 源码中的 acp_thread / agent_ui 做架构参考（注意 GPL，只读思路）

⚠️ 提示：网上能搜到的 `guide.gpui.zed.dev` **不存在**（DNS 无法解析）；[hippietrail/GPUI-Framework-Guide](https://github.com/hippietrail/GPUI-Framework-Guide) 是过时的旧版 README 镜像（仍写「仅 macOS/Linux」），慎用。要求最新 stable Rust（edition 2024）。

---

## 二、Claude 风格 Harness：功能与 UI 模式

> 本章基于对官方文档与一手资料的全量抓取（Claude Code Docs / claude.com Docs / support.claude.com / zed.dev / opencode.ai / docs.cline.bot / cursor.com / charmbracelet GitHub / Goose 代码库评审）。

### 2.1 三个总纲性结论

1. **Claude Desktop 已给出「桌面 harness」的标准形态**：三 Tab（Chat / Cowork / Code），其中 Code Tab = 会话侧栏 + 可拖拽多 Pane（chat/diff/browser/terminal/file/plan/tasks/subagent）+ 输入区四件套（环境/项目文件夹/模型/权限模式）。这是最直接的竞品蓝本（[Desktop application](https://code.claude.com/docs/en/desktop)）。
2. **Claude Code 的 CLI 交互词汇表就是 agent UI 的通用词汇表**：权限模式循环（Shift+Tab）、Plan Mode、/compact、checkpoint/rewind、subagent、斜杠命令、消息队列、/btw 侧问 —— 每个主流竞品都实现了其中一部分（[Interactive mode](https://code.claude.com/docs/en/interactive-mode)、[Permission modes](https://code.claude.com/docs/en/permission-modes)）。
3. **Zed 是唯一与本项目同技术栈（Rust + GPUI）的成熟参照物**：Agent Panel / Threads Sidebar / Review Changes 的交互与实现文档完全公开，可直接作为 GPUI 布局与渲染的实现参考（[Zed Agent Panel](https://zed.dev/docs/ai/agent-panel)）。

### 2.2 Claude Desktop 应用（标准形态）

**结构**：Chat（常规对话 + Artifacts）/ Cowork（agentic 工作区）/ Code（软件开发）三 Tab；Code Tab 每个「会话 = 独立聊天历史 + 项目文件夹」，侧栏列出会话且可并行运行。

**Code Tab 核心功能**：
- 发首条消息前配置四件事：Environment（Local/Cloud/SSH/WSL）、项目文件夹、模型下拉、权限模式
- **权限模式五档**：Manual / Accept edits / Plan / Auto / Bypass permissions，发送键旁的模式选择器切换，按文件夹记忆
- **Diff 视图**：文件列表 + 逐文件 diff，可点击任意行加评论、批量提交评论让 Claude 修改
- **Browser Pane**：内置浏览器，Claude 自动起 dev server 并「自我验证」—— 截图、查 DOM、点元素、填表单、自己修 bug；外部站点首次操作弹 Allow once / Always / Deny 卡片（按站点授权）
- **CI/PR 监控**：基于 gh CLI 轮询 check 结果，可开 Auto-fix CI 与 Auto-merge
- 视图模式三档：Normal（工具调用折叠）/ Thinking（+思考过程）/ Verbose（全部展开），Ctrl+O 循环
- 输入框：@mention 文件（带补全）、附件/拖放图片 PDF、灰色 ghost 建议下一句 prompt（Tab 接受）
- 快捷键全套（Cmd+N 新会话、Ctrl+Tab 切会话、Cmd+Shift+D diff、Ctrl+` 终端…）；usage ring 显示会话上下文占用 + 套餐用量
- 与 CLI 互通：`/desktop` 把终端会话搬进桌面 App；共用 CLAUDE.md、settings.json、MCP 配置
- 企业管控：managed settings（禁 bypass/auto、SSH 白名单、managedMcpServers 推送）

**Cowork Tab**：复用 agentic 架构但不开终端 —— 直接读写本地文件、配合浏览器扩展操作网页、subagent 并行分工、产出文档/表格/幻灯；侧栏 Customize 管理 connectors/skills/plugins；Dispatch 提供后台长任务（[Cowork Overview](https://claude.com/docs/cowork/overview)）。

**Artifacts（Chat Tab）**（[官方支持文档](https://support.claude.com/en/articles/17153992-what-are-artifacts-and-how-do-i-use-them)）：
- 触发条件：内容显著自包含（通常 >15 行）、会被编辑/复用、可脱离对话独立存在
- 在对话旁独立面板打开；侧栏 Artifacts Tab 集中管理 + 模板起点
- 对话式修改 or 直接编辑（划选文字点 "Edit with Claude" 就地改）；**编辑历史消息会生成带独立 artifacts 的新版本分支**；可导出
- 高级：artifacts 可内置调用 Claude、可连已授权 connector、个人/共享存储

**MCP/连接器**：远程连接器（GitHub/Slack/Linear…）在 Connectors UI 管理；本地 MCP server 走 claude_desktop_config.json；Code Tab 本地会话同时读 ~/.claude.json 与 .mcp.json。

### 2.3 Claude Code CLI 交互词汇表（重点）

**流式输出与工具展示**：回复流式打印；文件读取/grep/命令执行只显示一行摘要（如 "Read auth.ts"），完整内容只有模型看到；Ctrl+O 打开 transcript viewer 看每个工具调用细节；MCP 调用折叠成单行。官方明确了「hidden / one-liner / full text」三档展示哲学（[Context window](https://code.claude.com/docs/en/context-window)）。

**权限确认**（[Permission modes](https://code.claude.com/docs/en/permission-modes)）：
- 提示支持 Yes / No，且 **Tab 可在答案上加注释**（引导模型为什么拒绝）
- 六种模式：`default` 只读免批 / `acceptEdits` 自动接受文件编辑 / `plan` / `auto`（分类器审阅）/ `dontAsk` / `bypassPermissions`（仅隔离环境）
- Shift+Tab 循环切换；受保护路径（.git/.vscode/.claude/shell rc）任何模式都不自动批；`rm -rf` 针对关键路径是硬熔断

**Plan Mode**：只读探索产出计划，审批后再切回执行；Desktop 最佳实践是「复杂任务先 Plan 再 Accept edits」。

**TodoWrite 任务清单**：模型自维护 checklist；一次最多显示 5 条；任务跨 compact 保留；可跨会话共享。

**上下文压缩（compact）**：
- `/compact` 可带聚焦指令；auto-compact 接近阈值自动跑、阈值可调
- 压缩后保留/重载规则：系统提示词与 CLAUDE.md 重注入、最近编辑的最多 5 个文件重读、已调用 skill 正文重注入
- `/rewind` 菜单里还有 Summarize from here / up to here 定向压缩
- `/context` 实时显示按类别的 token 占用分解 + 优化建议

**Checkpoint 与回退（rewind）**（[Checkpointing](https://code.claude.com/docs/en/checkpointing)）：
- 每轮 prompt 前自动快照；每会话保留最近 100 个、约 30 天
- `/rewind` 六个动作：**Restore code and conversation / Restore conversation / Restore code / Summarize from here / Summarize up to here / Never mind** —— 代码与会话可独立回滚是核心设计
- 边界：Bash 造成的改动不追踪；后台 subagent 的编辑不恢复；非版本控制替代品

**斜杠命令**：`/` 菜单聚合内建命令、skills、插件与 MCP prompts；支持句中补全；`!` 前缀直接跑 shell；`@` 文件引用。

**Subagent**（[Sub-agents](https://code.claude.com/docs/en/sub-agents)）：
- 内置 Explore（只读）/ Plan（研究）/ general-purpose；自定义 = `.claude/agents/` 下带 YAML frontmatter 的 Markdown，改文件热生效
- **独立上下文窗口，只把最终摘要带回主会话**（上下文节省是首要卖点）；并发默认 20、嵌套 3 层
- **Fork**（/subtask）：继承全部对话历史的子代理，输入框下方面板运行，可观察、可打断、可续聊
- 输出扫描防注入：模仿 system-reminder 等指令形状的文本会被打标

**其他值得抄的交互**：消息队列（生成中输入即排队，可插队，Zed 称 steer）；`/btw` 不入历史的侧问；长命令转后台；session recap（离开回来给一行摘要）；vim 输入模式；PR/MR 状态徽章。

### 2.4 其他 agent UI 可借鉴模式

**Zed Agent Panel**（GPUI 实现，[docs](https://zed.dev/docs/ai/agent-panel)）：
1. Threads Sidebar 按项目分组管理并行 agent 线程，Ctrl+Tab 切换；worktree 选择器把会话隔离进独立 git worktree
2. 消息队列 + 每条可开 "Steer"（下个工具调用边界打断插入）；任意已发消息可编辑重发
3. 每次模型编辑后消息顶部出现 "Restore Checkpoint" 按钮
4. Review Changes：手风琴显示改动统计，多 buffer 审查页逐 hunk keep/reject
5. @ 上下文体系（文件/目录/符号/线程/skills/诊断/URL）；agent 运行时防系统休眠、后台完成时通知

**opencode TUI**（[docs](https://opencode.ai/docs/tui/)）：
1. leader 键体系全键盘操作；`/undo` `/redo` 基于 Git 同时回滚消息与文件改动 —— 简洁的 checkpoint 实现
2. `/thinking` 只切思考块显示；tui.json 细粒度 UI 配置（diff 双栏/单栏自适应、attention 通知）
3. `/export` 导出 Markdown、`/editor` 调外部编辑器写长 prompt

**Cline**（[Plan & Act](https://docs.cline.bot/core-workflows/plan-and-act)、[Checkpoints](https://docs.cline.bot/core-workflows/checkpoints)）：
1. Plan/Act 双模式显式切换，**两模式可配不同模型**（Plan 强模型、Act 快模型）
2. Checkpoints 用影子 Git 仓库（与项目历史隔离）；恢复三选：Restore Files / Restore Task Only / Both
3. Auto Approve 按类目的开关矩阵；命令安全性由模型标注决定而非固定白名单

**Cursor**（[3.0 changelog](https://cursor.com/changelog/3-0)）：
1. Agents Window：跨 repo/环境并行跑多个 agent；Agent Tabs 多聊天并排/网格同屏
2. Design Mode：浏览器里直接标注 UI 元素给 agent 精确反馈
3. `/worktree`（隔离 worktree）与 `/best-of-n`（同任务多模型并行再比较）

**Goose**（Block，Rust 核心）：Rust 9-crate 内核 + Electron 桌面（Rust 核心 + 多前端先例）；MCP-first 扩展系统（70+）；四档模式 + 五层工具安全管线；SQLite 会话 + 分支 + 80% 自动压缩；Recipes（可分享任务模板）+ cron 调度。

**Crush**（charmbracelet，[GitHub](https://github.com/charmbracelet/crush)）：`crushrc`（Bash 语法配置，可从 1Password 取密钥）；主题实时预览；MCP 三传输 + OAuth 流程；同 cwd 多客户端自动共享 workspace；Agent Skills 开放标准（agentskills.io）。

### 2.5 聊天流式 UI 通用最佳实践

- **流式 Markdown 增量渲染（关键技术点）**：每 chunk 全量重解析会让成本二次方增长。成熟解法是**增量 AST 解析**：CommonMark 块级解析按行进行，把已定块冻结（保留最后 1-2 块为不稳定前沿），只重解析尾部；冻结块缓存渲染节点、用源偏移做 key reconcile；流结束后做一次全量解析修复引用/脚注错排。GPUI 下同样适用：按块分 element、只重建尾部。参考：[DSH 的增量 mdast 渲染器设计笔记](https://github.com/deepseek-ai/deepseek-harness/blob/master/.agents/notes/archived/architecture/2026-08-06-web-markdown-incremental-ast-renderer.md)、flutter_markdown_stream 等流式 markdown 库。**从第一天就按此设计，避免后期从全量重解析迁移。**
- **代码块**：语言标签 + 语法高亮 + 一键复制是标配。
- **diff 展示**：文件列表带 +/- 行数统计；单/双栏依宽度自适应；逐 hunk accept/reject；行级评论；按轮次查看。
- **thinking 折叠**：默认折叠，Normal/Thinking/Verbose 三档；仅切显示不切能力；流式进行中也要能展开/收起。
- **错误处理**：区分 HTTP 错误与**流中途错误**（finish_reason 里）；瞬态错误指数退避重试；错误卡片 + 「把错误交给 Claude 修」。
- **停止生成**：Esc 即停且保留已完成部分；停止后排队消息照发。
- **重新生成/编辑/分支**：任意消息可改可重发；编辑历史 = 新版本分支；checkpoint 支持代码与会话独立回滚。
- **消息内交互元素**：权限卡（多选项 + 加注释）、ghost 建议 prompt、@mention chip、计划审批卡、todo checklist、PR 状态徽章。

### 2.6 桌面 harness 特有功能

- **工作区**：会话绑定项目文件夹（独立 cwd）；额外目录授权；worktree 隔离（Claude Desktop/Cursor/Zed 三家都有）
- **内嵌终端**：与会话同 cwd 同环境，可多 tab（Zed 把终端线程直接放进 Agent Panel）
- **Git 集成**：diff 审查、PR/CI 状态、checkpoint 影子 Git、worktree、commit 归属
- **模型/provider 切换**：会话中途可换；Plan/Act 各配模型；自定义 OpenAI/Anthropic 兼容 provider；本地模型自动发现（Ollama）；fallback 链
- **API key 管理**：环境变量 + 系统 keyring + MCP OAuth
- **命令面板 + 全快捷键**：ctrl+p 承载一切命令（Zed/Crush/opencode 通例）
- **系统通知/托盘**：失焦时完成/待权限才通知；agent 运行时防系统休眠（Zed）
- **多会话并行**：侧栏多会话 + Ctrl+Tab；并行 agent 窗口；跨会话消息
- **调度**：定时任务 + 后台队列；**代理/网络**：CDN 白名单、自定义 CA

### 2.7 功能优先级清单（P0/P1/P2）

**P0 —— MVP 必须（12 项）**

| # | 功能 | 说明 |
|---|---|---|
| 1 | 多会话 + 侧栏 + 恢复 | 每会话独立历史与 cwd，可关闭重开 |
| 2 | 流式回复 + 增量 markdown | 块级冻结 + 尾部重解析；代码块高亮 + 复制 |
| 3 | 工具调用折叠展示 | 一行摘要可展开；hidden/one-liner/full 三档 |
| 4 | 权限确认卡 + 工具规则 | Allow once / always / deny + 加注释；规则持久化 |
| 5 | 停止/中断 | Esc 停止且保留已完成工作 |
| 6 | 错误处理与重试 | 区分流中途错误；瞬态退避重试 |
| 7 | Provider/模型/API key 设置 | OpenAI-compat + Anthropic-compat；会话中可切 |
| 8 | 本地文件读写 + shell | 命令输出只回摘要防上下文爆炸 |
| 9 | 任务清单（todo）展示 | 模型自报进度，状态化 |
| 10 | token 用量 + /compact + auto | 用量常显；压缩可带指令；自动压缩留标记 |
| 11 | 斜杠命令框架 | `/` 触发、过滤、补全；留扩展位 |
| 12 | 项目记忆文件 | 读 AGENTS.md/CLAUDE.md 层级；`/init` 生成 |

**P1 —— 差异化体验（15 项）**：Plan Mode + 计划审批卡；Checkpoint/rewind（代码与会话独立回滚）；Diff 审查面板（逐 hunk）；Thinking 折叠；消息队列 + steer；编辑重发/重生成/分支；Subagent（含 fork）；@mention 上下文；内嵌终端 Pane；MCP 客户端（stdio + HTTP）；多会话并行 + worktree 隔离；通知 + 防休眠；主题 + 命令面板；/context 用量分解；会话导出。

**P2 —— 完整版（11 项）**：Browser Pane 自我验证；Computer Use；Artifacts 侧面板；PR/CI 监控；云/SSH 远程会话；定时任务；Skills/插件生态；Agent teams；语音输入/recap；企业管控；安全管线（工具检查器链 + 输出扫描）。

### 2.8 GPUI 落地提示

Zed 即 Rust+GPUI 上最成熟的 agent UI —— 其 pane/dock 布局、threads sidebar、多 buffer diff 审查的实现文档全公开（[agent-panel](https://zed.dev/docs/ai/agent-panel)、[parallel-agents](https://zed.dev/blog/parallel-agents)），可作为布局架构与渲染管线的直接参照；**流式 markdown 请从第一天就按「冻结块 + 增量解析」设计**。

---

## 三、「内核可更换」架构调研

### 3.1 「内核」的两个层次

- **(a) Agent Runtime 内核**：工具调用循环、权限审批、文件操作、上下文管理 —— 决定「这个 agent 怎么干活」。这正是 Claude Agent SDK 的定义："same tools, agent loop, and context management that power Claude Code"。
- **(b) Model Provider 内核**：Anthropic / OpenAI 兼容 / OpenRouter / Ollama 等 LLM 端点抽象 —— 决定「脑子是谁」。

**「引擎可换」与「provider 可换」是两个正交自由度**。各项目分层情况：

| 项目 | 引擎/provider 分层 | 证据 |
|---|---|---|
| Goose | ✅ core crate 里 `agents/` 与 `providers/`（Provider trait + anthropic.rs/openai.rs + 声明式 JSON provider）分目录 | 架构文档【确认】 |
| Cline | ✅ 包级强制分层：`@cline/core`（编排）→ `@cline/agents`（AgentRuntime 循环）→ `@cline/llms`（provider 网关/模型目录），严格单向依赖 | SDK 架构文档【确认】 |
| opencode | ✅ server 内独立 provider 层（`/provider`、`/config/providers`、OAuth），agent/会话是另一组资源 | server 文档【确认】 |
| Crush | ⚠️ 单体二进制内部分模块（多 provider + LSP/MCP/Skills 配置层），正把逻辑抽到 server 侧（client/server 分离重构中） | 仓库提交记录【确认+推断】 |
| Claude Code/Agent SDK | ⚠️ 耦合最深：引擎即产品本体，provider 基本锁定 Anthropic（可换 Bedrock/Vertex 通道），agent loop 不作为可替换件，只以库（SDK）或子进程整体输出 | 官方文档【确认】 |
| Zed | 特殊双轨：自家 Zed Agent（in-process）与 External Agents（ACP 子进程，模型/auth/计费全归 agent）并存 | external-agents 文档【确认】 |

**业界共识：provider 抽象留在引擎内部（trait/interface），「引擎整体」作为 UI 的可换单元。**

### 3.2 ACP：Agent Client Protocol（核心推荐）

**动机**【确认】：agent 与编辑器紧耦合导致每个 editor×agent 组合都要定制集成。ACP 类比 LSP 解绑语言智能的方式解绑 agent（起源：Google 主动找 Zed 谈 Gemini CLI 集成，Zed 借此把 agent 面板做成可扩展）。来源：[agentclientprotocol.com](https://agentclientprotocol.com/get-started/introduction)、[Bring Your Own Agent to Zed](https://zed.dev/blog/bring-your-own-agent-to-zed)。

**传输**【确认】：本地 agent = 编辑器子进程按需启动，JSON-RPC 2.0 over stdin/stdout；一个连接支持多个并发 session；大量使用 notification 做流式更新，**双向 request** 让 agent 反向请求编辑器（权限审批）；远程 agent（HTTP/WebSocket）为进行中特性（v2 草案）。

**核心 method（v1）**【确认】：
- 初始化：`initialize`（+ 可选 `authenticate`）
- 会话：`session/new` / `session/load`（恢复）/ `session/list` / `session/delete`
- 回合：`session/prompt`（请求-响应，turn 结束返回 stop reason）；期间 agent 发 `session/update` 通知流；`session/cancel` 中断
- **`session/update` 变体全集**：`user_message_chunk`、`agent_message_chunk`、`agent_thought_chunk`、`tool_call`、`tool_call_update`、`plan`、`available_commands_update`、`current_mode_update`、`usage_update`（上下文用量 + 累计成本）、`notice`、`compaction_update`/`compaction_summary_chunk` 等 —— **这几乎就是一份「agent UI 事件总线」的现成规格说明**（[protocol v1](https://agentclientprotocol.com/protocol/v1/prompt-turn)）
- **权限审批**：agent 反向调用 `session/request_permission`，带 `toolCall` 与 `options[]`（`allow_once / allow_always / reject_once / reject_always`），client 回 `outcome: selected/cancelled`；客户端可按用户设置自动放行（[tool-calls](https://agentclientprotocol.com/protocol/v1/tool-calls)）
- 工具调用内容块：普通 content、**diff**（oldText/newText/path）、**terminal**（引用 live 终端输出）；toolCall 带 `kind`（read/edit/delete/move/search/execute/think/fetch/switch_mode/other）与 `locations`（path+line，支持 follow-along）

**MCP 关系**【确认】：编辑器把用户配置的 MCP server 转发给 agent 直连；编辑器自身想暴露工具时不与 ACP 混跑同一连接，而是起一个 MCP proxy 隧道回自己。一句话：**MCP 给 agent 授束能力，ACP 给 agent 授予界面**。

**Zed 的实践与教训**【确认】：Zed 把自家 in-process agent 改成与外部 agent 走相同代码路径 —— 原话 "Cleanly separating UI from the core of our agent has already improved our shipping velocity"。被弃用的只是「extension 分发自带 agent」的旧机制（已自动迁移到 2026-01 上线的 ACP Registry）；自研 Zed Agent 仍是一等公民。
- 接 Claude Code 的方式：Zed 开源了 **claude-code-acp 适配器**（Apache-2.0，[zed-industries/claude-code-acp](https://github.com/zed-industries/claude-code-acp)），"wraps Claude Code's SDK and translates its interactions into ACP's JSON RPC format"——Claude Code 运行为独立进程而 Zed 提供 UI。
- 生态现状：客户端有 Zed、Neovim（CodeCompanion/avante）、Emacs、marimo，JetBrains 在途；agent 侧 Gemini CLI 原生、Claude Code 经适配器、Goose 原生、opencode `opencode acp`、Codex 适配器在途。

**Rust 支持**【确认】：官方 crate [`agent-client-protocol`](https://crates.io/crates/agent-client-protocol)（3.3.0，Apache-2.0，累计下载 518 万+，83 个版本），feature 含 `process`（子进程管理）、`stdio`、`unstable_llm_providers`、`unstable_mcp_over_acp`、`unstable_session_fork`、`unstable_subagents` 等。**它是现成的 serde 类型库 —— 内部事件模型可以直接复用其类型定义。**

### 3.3 其他分离式架构案例

**opencode（HTTP server 范本）**【确认】
- "运行 `opencode` 时，它启动一个 TUI 和一个 server，TUI 就是与 server 通信的 client"；server 暴露 OpenAPI 3.1 spec，SDK 由 spec 生成；`opencode serve` 可无头独立运行（[docs](https://opencode.ai/docs/server/)）
- API 面：`POST /session/:id/message`（同步）与 `prompt_async`；`/session/:id/abort`；`/session/:id/permissions/:id`（应答权限）；`GET /event` SSE 事件流；`/config/providers`、provider OAuth
- 同时实现 ACP：`opencode acp` 作为 ACP 子进程供 Zed/JetBrains 等使用 —— **一个引擎同时暴露 HTTP API 和 ACP 两种面**（仓库已迁至 anomalyco/opencode，v2 推进中）

**Goose（Rust 库 + 服务化双形态）**【确认】
- crate 布局：`goose`（core：agents/providers/conversation/config）、`goose-cli`、`goose-server`（**goosed**："HTTP/WebSocket server for the desktop UI"，REST + WS 流式）、`goose-mcp`（内置扩展走 `rmcp::ServerHandler`）、`goose-acp`
- 桌面端是 **Electron**（不是 Flutter）；即 Goose 同时是：进程内库（crate 复用）+ 独立服务（goosed）+ ACP agent（goose-acp）—— 与本报告推荐的双轨架构完全同构

**Cline（纯进程内分层的范本）**【确认】：`@cline/core`（会话/工具/审批/插件）→ `@cline/agents`（浏览器兼容 AgentRuntime）→ `@cline/llms`（provider 网关 + 运行时注册）→ `@cline/shared`（契约）；展示纯进程内分层如何支撑多宿主复用，代价是宿主必须是 JS 运行时。

**Continue**【确认+推断】：agent 循环在 IDE 扩展宿主内，配置统一 config.yaml；宿主绑定深（VS Code 转向 server-based 运行时后部分功能失效）—— 分离度低于 opencode/Goose 路线。

### 3.4 Claude Code 的嵌入方式与 Claude Agent SDK（重要约束）

**VS Code 官方扩展的做法**【确认】（[vs-code](https://code.claude.com/docs/en/vs-code)）：
- "The extension **bundles its own copy of the CLI**" —— 聊天面板就是启动一个 claude 子进程；`claudeProcessWrapper` 设置可替换启动的可执行文件；扩展与 CLI 共享会话历史
- **内置 IDE MCP server**：扩展激活时跑一个本地 MCP server（名 `ide`），CLI 自动连接（ws://127.0.0.1 随机端口 + lock 文件 token 鉴权）；对模型可见的工具仅 getDiagnostics/executeCode，其余是扩展内部 RPC（开 diff、读选区）。这就是「**UI 宿主以 MCP server 身份反向服务引擎**」的模式（与 ACP 的 MCP proxy 同源）

**headless / stream-json**【确认】（[headless](https://code.claude.com/docs/en/headless)）：
- `claude -p --output-format stream-json --verbose --include-partial-messages` 得逐 token 流（NDJSON）；末行 `result` 含最终文本/成本/session 元数据；`--input-format stream-json` 支持流式输入；`--permission-mode`、`--allowedTools`、`--json-schema` 结构化输出
- 首事件 `system/init` 带模型/工具/MCP 状态 + **capabilities 数组**（feature-detect 用）；`SIGINT` = 结束当前 turn、`SIGTERM` = 硬停
- **非 TS/Py 语言嵌入 Claude Code 的官方姿势就是子进程消费 stream-json** —— 对 Rust 应用同样适用

**Claude Agent SDK**【确认】（[overview](https://code.claude.com/docs/en/agent-sdk/overview)）：
- 定位："A library that runs the Claude Code binary"，Python/TS 两个包；能力 = 内置工具/权限/hooks/subagents/MCP/sessions/skills/plugins
- ⚠️ **计费红线（对本项目关键）**：官方原文 —— "Anthropic does not allow third party developers to offer claude.ai login or rate limits for their products, including agents built on the Claude Agent SDK. Use the API key authentication methods"。即**第三方 UI 嵌入 SDK 默认只能走 API key 计费**；把 Claude 订阅（Pro/Max）登录暴露给你的用户需 Anthropic 事先批准。另有品牌限制（不得称 "Claude Code Agent"）。
- SDK 与 CLI 历史上存在功能差（plan mode、部分命令），Zed 曾公开呼吁对等

### 3.5 协议层细节

- **OpenAI-compatible API 已是 provider 层事实标准**【确认+推断】：Crush 明示自定义 provider 走 "OpenAI- or Anthropic-compatible APIs"；Goose 有 declarative providers；opencode 有 provider 配置 + 模型目录；Ollama/vLLM/LM Studio 本地生态默认暴露 OpenAI 兼容端点。**自研内核先实现 OpenAI-compatible 一个 provider 即可覆盖一大片。**
- **Anthropic Messages API SSE 事件全集**【确认】（[streaming](https://platform.claude.com/docs/en/build-with-claude/streaming)）：
  - 流程：`message_start`（初始 usage）→ 每内容块 `content_block_start` → n 个 `content_block_delta` → `content_block_stop` → `message_delta`（stop_reason、累计 usage）→ `message_stop`
  - delta 类型：`text_delta`、`input_json_delta`（工具入参的部分 JSON，收齐后整体解析）、`thinking_delta`、`signature_delta`；另有 `ping`、`error`（如 529 overloaded）
  - 版本策略允许新增事件类型，客户端须优雅处理未知事件
  - **这套「块-增量-停止」三层结构与 ACP 的 `*_message_chunk`/`tool_call` 事件模型同构 —— 内核适配几乎是直接映射**（需写一个 message accumulator）

### 3.6 两种解耦方式对比

| 维度 | (a) 进程内 trait | (b) 子进程 + 协议（ACP/HTTP） |
| --- | --- | --- |
| 崩溃隔离 | ❌ panic 可带崩 UI | ✅ 只丢子进程，UI 存活可重连 |
| 内核语言自由度 | ❌ 必须 Rust | ✅ 任意（TS/Go/Rust…） |
| 延迟 | ✅ 零序列化直调 | ⚠️ 每事件一次 IPC；本地 stdio 足够支撑逐 token 流 |
| 打包分发 | ✅ 单二进制最简 | ❌ 需发现/安装外部 agent、版本矩阵管理 |
| 权限/安全边界 | ⚠️ 同地址空间靠自律 | ✅ 进程级边界：OS kill、沙箱、独立凭据 |
| 测试难度 | ✅ mock trait | ⚠️ 起真子进程或录制回放协议流 |
| 升级独立性 | ❌ 一起重编译 | ✅ agent 独立升级 |
| 业界用户 | Zed 自研 agent（与外部同 UI 路径）、Goose core、Cline | Zed↔所有 ACP agent、opencode、Goose Desktop↔goosed、VS Code↔Claude CLI |

**业界实际答案是混合**：Goose = crate 复用（a）+ goosed 服务（b）+ ACP（b）；Zed = 自研 in-process（a）+ ACP 外部（b）；opencode = 引擎 server（b）+ 自己也是 ACP agent（b）。

### 3.7 候选「内核」清单（可更换的目标）

| 内核 | 接入方式 | 语言 | 许可/成本 | 备注 |
| --- | --- | --- | --- | --- |
| **自研 builtin** | 进程内 trait | Rust | 自有 | 完全可控；provider 走 OpenAI-compat + Anthropic + Ollama |
| **Claude Code** | ACP 子进程（经 [claude-code-acp](https://github.com/zed-industries/claude-code-acp) 适配器）或 headless stream-json 子进程 | Node | 商业（**默认仅 API key 计费**，见 3.4 红线） | 能力最强的现成 coding agent；Zed 官方同款接法 |
| **opencode** | `opencode acp`（ACP）或 HTTP server | Go/TS | 开源 | 一个引擎双面暴露；client/server 架构范本 |
| **Goose** | 原生 ACP（goose-acp）或 Rust crate 进程内 | Rust | Apache-2.0（部分） | 与本项目最同构的架构；MCP-first |
| **Gemini CLI** | 原生 ACP 子进程 | Node | Apache-2.0 + Google 配额 | Google 官方支持 ACP |
| **Crush** | 参考（client/server 重构中） | Go | 开源 | crushrc 配置与主题系统值得借鉴 |
| **任意 ACP agent** | ACP 子进程 | 任意 | 各异 | ACP Registry 持续增长，即插即用 |

---

## 四、推荐架构与 MVP 路线

### 4.1 推荐分层架构（Rust workspace）

```
┌─────────────────────────────────────────────────────────┐
│ app (bin)      gpui-kit UI 层                            │
│   ├─ 会话侧栏 / 聊天流(虚拟列表+流式 Markdown) / 输入区   │
│   ├─ 工具调用卡片 / 权限审批弹层 / diff 查看器            │
│   └─ 设置(provider/API key/模型选择/内核切换)             │
├─────────────────────────────────────────────────────────┤
│ core (lib)     会话编排层（UI 无关，可独立测试）           │
│   ├─ 统一事件流模型 KernelEvent（流式文本/工具调用/权限/   │
│   │  状态/用量…）—— UI 只消费这一种抽象                   │
│   └─ Kernel trait: create_session / prompt / cancel /     │
│      request_permission 回调 / list_kernels               │
├──────────────────┬──────────────────────────────────────┤
│ kernels/builtin  │ kernels/acp                           │
│ 自研内核(进程内)   │ ACP 适配器(子进程)                     │
│ ├─ agent loop    │ ├─ agent-client-protocol crate         │
│ ├─ provider 适配 │ ├─ Claude Code / Gemini CLI / 任意     │
│ │  (Anthropic /  │ │  ACP agent 自动发现与生命周期         │
│ │  OpenAI 兼容 /  │ └─ JSON-RPC over stdio                │
│ │  Ollama)       │                                       │
│ └─ 内置工具集     │                                       │
│    (fs/bash/…)   │                                       │
└──────────────────┴──────────────────────────────────────┘
```

要点：

1. **UI 层只认识 `KernelEvent` 流**，不认识任何具体内核 —— 这就是「内核可更换」的全部秘密。事件模型建议直接抄 ACP 的 `session/update` 事件形状（agent_message_chunk / tool_call / tool_call_update / plan / request_permission / session_status…），这样 ACP 适配器几乎是透传，自研内核对齐同一形状。
2. **双轨内核**：`builtin`（进程内 trait 实现，零外部依赖，MVP 默认）+ `acp`（子进程，能力上限）。切换内核 = 换一个会话的 Kernel 实例，UI 无感。
3. **provider 与 runtime 分离**：自研内核内部再做 provider 抽象（Anthropic 原生 SSE + OpenAI 兼容端点 + Ollama），参考 Zed 的 anthropic / open_ai / ollama crate 划分。
4. **会话持久化**放在 core 层（JSONL 追加式，Claude Code 同款思路），内核重启/UI 重启都能恢复。

### 4.2 MVP 路线（建议 4 个里程碑）

> 顺序上采纳「ACP 先行」策略（业界调研结论）：先用现成 agent 验证 UI 设计（零引擎成本），再写自研内核。自研内核与 ACP 适配器共用同一 `KernelEvent` 模型，先后顺序可按需对调。

**M0 · 走通框架（约 1 周）**
- Windows + Rust(MSVC，edition 2024) 环境；`gpui-kit = "0.7"` 起项目，跑通窗口
- 三栏骨架：会话侧栏 / 消息流 / 输入区
- **IME 冒烟测试（微软拼音 + 搜狗双测）** + 高 DPI + 多显示器；确认 gpui-pre 快照含 2025-11 IME 修复
- 验收：能顺畅输入中文、虚拟列表滚动 1 万条消息不掉帧

**M1 · 事件模型 + ACP 先行（1–2 周）**
- core 层定义 `Kernel` trait + `KernelEvent`（**直接复用 ACP 的 SessionUpdate 类型**，`agent-client-protocol` crate 即现成 serde 类型库）+ 权限审批 sink
- `AcpKernel`：用官方 crate 拉起 claude-code-acp 或 `opencode acp` 子进程，跑通 initialize → session/new → session/prompt → session/update 流式渲染
- 流式 Markdown（增量解析：冻结块 + 尾部重建）、tool_call 折叠卡片、usage 条
- 验收：**零行引擎代码**，同一 UI 驱动一个真实 coding agent 完成任务 —— 「内核可更换」的核心卖点在此刻成立

**M2 · 自研内核（2–4 周）**
- `InProcessKernel`：`Provider` trait 先做 **OpenAI 兼容**一个实现（同时覆盖 OpenRouter/Ollama/vLLM/LM Studio），再加 Anthropic 原生 SSE（写「块-增量-停止」三层 accumulator 映射到 KernelEvent）
- 工具调用循环：fs 读写 / bash / 权限规则（allow once/always/deny + 持久化）
- 会话持久化（JSONL/SQLite）、侧栏管理、/compact + auto-compact、todo 展示、项目记忆文件（AGENTS.md/CLAUDE.md）
- 验收：等价于「GUI 版 Claude Code（自研内核）」，无需安装任何外部 CLI

**M3 · 收口与打磨（2–3 周）**
- 权限系统与 MCP 配置统一收到 UI 侧（审批规则；MCP server 转发给两个内核）
- 设置页内核切换（每会话选内核）；子进程生命周期管理（崩溃重启重连、Windows job object 防孤儿进程）
- 验收：同一 UI 分别用「自研内核」「Claude Code 内核」完成同一个真实编码任务

**M4+（P1/P2 功能池，见第二章 2.7 清单）**：checkpoint 回退、会话分支、diff 审查面板、thinking 折叠、消息队列+steer、内嵌终端、git 集成、多会话并行、系统托盘、自动更新……

### 4.3 技术选型速查

| 关注点 | 选择 | 理由 |
| --- | --- | --- |
| UI 框架 | gpui-kit 0.7（内含 pin 版 GPUI） | Apache-2.0、生产验证、自带 Markdown/虚拟列表/Dock/输入框 |
| 内核边界协议 | ACP（agent-client-protocol 3.x） | 官方 Rust crate、Zed/JetBrains 背书、直接接 Claude Code 等 |
| 自研内核 HTTP | reqwest + SSE（eventsource-stream） | Rust 生态标准做法 |
| Markdown 解析 | pulldown-cmark（gpui-kit 内置使用同源方案） | Zed 同款 |
| 持久化 | JSONL 追加 + sled/SQLite（后续） | 简单起步 |
| 打包/更新 | gpui-kit Packaging/Auto-Update 文档方案 | 现成 |

---

## 五、风险与开放问题

### 5.1 技术风险

| 风险 | 等级 | 缓解 |
| --- | --- | --- |
| GPUI 上游「只服务 Zed」（2026-02 刹车 + crates.io 停滞一年） | 高 | 主干走 gpui-kit（三周一度活跃发版，pin gpui-pre 快照）；锁死版本；把升级当季度维护成本；core 层与 UI 层解耦，最坏情况可换渲染层保留 core |
| 0.x breaking changes | 中 | 同上锁版本策略；避免直接用 gpui 私有 API，尽量经 gpui-kit 组件 |
| Windows IME（0.2.2 含已知 bug；搜狗拼音存疑） | 中 | 走 gpui-pre 快照（含 2025-11 修复）；M0 双测微软拼音+搜狗；必要时给 gpui-kit 提 PR（该仓库对 IME 修复响应活跃） |
| 流式 Markdown 增量渲染性能 | 中 | 从第一天按「冻结块 + 增量解析」设计（见 2.5）；虚拟列表 + 段落级缓存 |
| Claude Agent SDK 计费红线（第三方 UI 默认只能 API key） | 中 | 产品设计即按「用户自带 API key / 自装 CLI」定位；若要提供订阅登录需先获 Anthropic 批准 |
| ACP v1→v2 演进、远程 agent 未完成 | 低 | 用官方 Rust crate；unknown 事件类型兜底 + capabilities feature-detect |
| 外部内核（Claude Code 等）的安装/版本管理 | 中 | 设置页 CLI 探测 + 安装引导；参考 Zed 的 agent 发现与 ACP Registry |
| 单人项目范围失控 | 高 | 严格按 M0–M3 里程碑推进，每个里程碑都有「可用」验收标准；P1/P2 功能进池不进排期 |

### 5.2 许可证与商务注意事项

1. ✅ gpui（crate 本体）/ gpui-kit / agent-client-protocol 均 Apache-2.0：闭源商业发布无障碍（保留 NOTICE 即可）。
2. ✅ **Zed 仓库整体是 GPL-3.0-or-later**（README 原话 "licensed primarily under GPL-3.0-or-later, with Apache-2.0 components where marked"）：agent_ui、markdown、ui、editor 等应用层 crate **只能读作参考，复制代码会触发 GPL 传染**。Markdown 渲染等能力用 gpui-kit 的 Apache-2.0 等价物。
3. ✅ **Claude Agent SDK 计费红线**：官方原文 "Anthropic does not allow third party developers to offer claude.ai login or rate limits for their products... Use the API key authentication methods" —— 第三方 UI 默认只能 API key 计费；把 Claude 订阅登录暴露给用户需 Anthropic 事先批准。另有品牌限制（不得称 "Claude Code Agent"）。若产品仅自用则无此约束。
4. 🔶 Claude Code CLI 是 Anthropic 商业产品：经 ACP/headless 接入是官方支持的用法（Zed/VS Code 同款），但分发 app 时应让用户自装 CLI、自备凭据。

### 5.3 待验证的开放问题（建议 M0–M1 期间实测）

1. gpui-pre =0.3.9（gpui-kit 0.7.3 所 pin 快照）在 Windows 上的 IME 实测：微软拼音 + 搜狗（已知修复在 2025-11 之后的主线，需确认快照包含）。
2. 流式 Markdown 在高速 token 流下的 CPU/GPU 占用与代码块部分高亮闪烁。
3. Windows 上 ACP 子进程 stdio 长连接稳定性（Windows 管道行为与 Unix 不同）；claude-code-acp（Node）在 Windows 的拉起与生命周期。
4. gpui-kit Dock 布局 + 虚拟列表组合下的聊天流布局稳定性（动态高度 + 底部锚定滚动）。
5. waku.sh / zedra 的实际体验（装来试用，作为直接对标竞品）。

---

## 附录：主要来源

**GPUI / Zed**
- 官方站与教程：<https://gpui.rs> · <https://gpui.rs/examples> · [gpui README](https://github.com/zed-industries/zed/blob/main/crates/gpui/README.md) · [docs.rs/gpui](https://docs.rs/gpui/latest/gpui/)
- crate 状态：<https://crates.io/crates/gpui>（0.2.2 / Apache-2.0 / 2025-10-22 后停滞）· [反向依赖](https://crates.io/api/v1/crates/gpui/reverse_dependencies)
- Windows GA：<https://zed.dev/blog/zed-for-windows-is-here> · Zed 1.0：<https://zed.dev/blog/zed-1-0> · IME：[issue #36736](https://github.com/zed-industries/zed/issues/36736)
- 刹车事件存档：<https://althacker.news/item?id=47003569> · 批判视角：<https://dev.to/dishant0406/gpui-by-zed-the-electron-killer-that-never-left-home-209o>
- 同类先例：Delta <https://delta.dev>（[公测博客](https://zed.dev/blog/delta-public-beta)）· waku.sh [Show HN](https://hn.nuxt.dev/item/49315709)
- 教程：<https://github.com/hedge-ops/gpui-tutorial>

**组件库**
- gpui-kit：<https://github.com/longbridge/gpui-kit> · <https://gpui-kit.com/docs/getting-started>（中文版 / Packaging / Auto-Update / WebView 文档）

**ACP 与内核架构**
- ACP 官网：<https://agentclientprotocol.com>（Introduction / Architecture / [Protocol v1](https://agentclientprotocol.com/protocol/v1/overview) / [Prompt Turn](https://agentclientprotocol.com/protocol/v1/prompt-turn) / [Tool Calls](https://agentclientprotocol.com/protocol/v1/tool-calls)）
- Rust crate：<https://crates.io/crates/agent-client-protocol> · [GitHub](https://github.com/agentclientprotocol/agent-client-protocol)
- Zed 系列博客：[Bring Your Own Agent](https://zed.dev/blog/bring-your-own-agent-to-zed) · [Claude Code via ACP](https://zed.dev/blog/claude-code-via-acp) · [ACP Progress Report](https://zed.dev/blog/acp-progress-report) · [External Agents 文档](https://zed.dev/docs/ai/external-agents) · [Agent Panel](https://zed.dev/docs/ai/agent-panel)
- claude-code-acp 适配器：<https://github.com/zed-industries/claude-code-acp>
- opencode：<https://opencode.ai/docs/server/> · <https://opencode.ai/docs/acp/> · Goose 架构：<https://mintlify.wiki/block/goose/development/architecture> · Crush：<https://github.com/charmbracelet/crush> · Cline SDK：<https://docs.cline.bot/sdk/architecture/overview>

**Claude / Anthropic 官方**
- Claude Desktop：<https://code.claude.com/docs/en/desktop> · Interactive mode：<https://code.claude.com/docs/en/interactive-mode> · Permission modes：<https://code.claude.com/docs/en/permission-modes> · Checkpointing：<https://code.claude.com/docs/en/checkpointing> · Context window：<https://code.claude.com/docs/en/context-window> · Sub-agents：<https://code.claude.com/docs/en/sub-agents>
- Agent SDK：<https://code.claude.com/docs/en/agent-sdk/overview> · Headless：<https://code.claude.com/docs/en/headless> · VS Code 集成：<https://code.claude.com/docs/en/vs-code>
- Artifacts：<https://support.claude.com/en/articles/17153992-what-are-artifacts-and-how-do-i-use-them>
- Streaming API（SSE 事件全集）：<https://platform.claude.com/docs/en/build-with-claude/streaming>
- MCP：<https://modelcontextprotocol.io/docs/getting-started/intro>

**其他竞品**：Cline Plan&Act/Checkpoints（docs.cline.bot）· Cursor 3.0（cursor.com/changelog/3-0）· opencode TUI（opencode.ai/docs/tui）

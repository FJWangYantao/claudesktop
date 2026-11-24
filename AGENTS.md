# claudesktop 开发约定

## 工作方式

1. **第一性原理**：先确认真实目标、约束和成功标准，再动手；不要套用惯例（惯例只是默认值，不是理由）。
2. **按复杂度选回答形式**：简单问题直接回答；复杂的方案 / 对比 / 梳理用 `answer-me-with-html` 出一份带样式的 HTML。
3. **坑写进 `HANDOFF.md`**：踩过的坑、绕过的弯路记进项目根的 `HANDOFF.md`；接新任务前先读一遍，避免重踩。
4. **浏览器默认用 Chrome**（含 Codex 的浏览器自动化）；要动 Vivaldi（用户自己的浏览器）之前先问。
5. **上线 / 部署 / 生产环境的任务**：把性能开销（CPU、内存、请求数、启动时间）写进设计取舍和验收标准。
6. **UI 不堆二级标题和说明**：能不写的标签、副标题、解释句就不写，让排版本身说明结构（同「设计指南-Claude风格UI.md」的反模式一节）。
   *Avoid unnecessary secondary labels and descriptions. Let the layout do the explaining.*
7. **截图验证用无头窗口**：先起隐藏窗口离屏渲染，不抢前台、不移动鼠标、不操作用户正在使用的窗口。

## 目录与边界

- `crates/core`：会话、内核抽象（`Kernel` / `KernelEvent`）、日志存储、ACP、git、插件、记忆、检查点。**不含 UI 依赖**，可单测。
- `crates/builtin`：自研内核（进程内直接调 provider），MCP、网关、休眠、自动模式分类器、思考强度都在这里。
- `apps/desktop`：gpui 薄视图层。会话 / 内核逻辑放 `crates/*`，界面只做状态映射和绘制。
- `vendor/`：打过补丁的上游 crate（gpui-pre / gpui-pre-windows / markdown），经 `[patch.crates-io]` 接入；改动标 `claudesktop patch` 并记进 `vendor/README.md`。

## 构建与验证（硬规范）

- 构建目录固定 `$env:CARGO_TARGET_DIR="D:/t3"`：项目内 `target/debug/build/ring-*` 会被安全软件动过，编译报 D8050。
- 提交前跑 `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`。
- 桌面端单测入口是 `cargo test -p claudesktop --bin claudesktop`（这个 crate 没有 lib target）。
- **`use gpui_kit::*` 的文件里写单元测试必须用 `#[core::prelude::v1::test]`**：裸 `#[test]` 会被 glob 导入的 gpui 宏劫持，展开时无限递归。
- **调试钩子统一走环境变量（`CLAUDESKTOP_*`），入口集中在 `main.rs`**；
  菜单预览用 `Popover::default_open` 只初始化一次（`CLAUDESKTOP_OPEN=model-menu` 预览设置页默认模型菜单，
  `composer-model-menu` 预览输入框模型菜单及固定思考底栏；`agent-completion` 配合 `CLAUDESKTOP_COMPLETION`
  预览真实补全面板），不改成受控模式。
  截图用 `--features headless-screenshot` 编译，设 `CLAUDESKTOP_SCREENSHOT=<PNG 路径>`：隐藏窗口离屏渲染后自动退出；`QUIET` 只是不激活，不能代替无头截图。
  正式 exe 的无前台验收可设 `CLAUDESKTOP_HEADLESS=1`（隐藏窗口、每 250ms 离屏绘制，不启用文件控制入口）。
- 提交信息用英文 conventional commits（见 git log），代码注释和界面文案用中文。
- Windows 分发包统一走 `tools/package-windows.ps1`（校对 exe 版本 / x64、压缩包内容和两份 SHA256）；`v*` tag 生成 Release 草稿，下载产物验收后再发布。
- **本文件会整份进自研内核的系统提示词**（`crates/builtin/src/instructions.rs`，每次请求都带，超过 20,000 字符截断），新增内容保持精炼；细则放 docs/dev。
- 新增要进日志的 `KernelEvent` 时，同时想清楚 `Record::worth_keeping`、`history::rebuild`（续接时进不进上下文）
  和 `Session::apply_event`（重放时怎么显示）三处。重放日志先折算回退，有所有权用 `into_effective` 原地保留，借用时用 `iter_effective`。

## 按需查阅的模块手册（改对应代码前先 read_file）

- 界面组件 / 配色 / rem / 动效 / 滚轮 / 侧栏 / 设置页 / gpui-kit 踩坑 → [docs/dev/gpui-ui.md](./docs/dev/gpui-ui.md)
- 自研内核 / 自动审批 / ask_user / MCP / 插件 / 搜索 / 记忆 / 思考档位 / 休眠 → [docs/dev/kernel-and-mcp.md](./docs/dev/kernel-and-mcp.md)
- 用量网关 / 内存与帧率压测 / 独立验收包 → [docs/dev/gateway-and-bench.md](./docs/dev/gateway-and-bench.md)
- ACP 外部 agent 协议 / 工具内容 / 登录 / 结构化提问 → [docs/dev/acp-protocol.md](./docs/dev/acp-protocol.md)

视觉与交互的取舍依据见 [设计指南-Claude风格UI.md](./设计指南-Claude风格UI.md)，
网页 CSS 手法能否搬进 gpui 见 [调研报告-Claude前端CSS手法对照.md](./调研报告-Claude前端CSS手法对照.md)。

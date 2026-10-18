# claudesktop

Claude 风格的 AI 编程桌面端（Rust + GPUI），Windows 原生，**内核可更换**。

界面照着 Claude 系的暖纸底 + 赤陶橙做：浮起的圆角侧栏、顶栏兼做窗口拖拽区、
右对齐的用户气泡、流式 Markdown、工具行与 diff、权限卡片。内核是一个可替换的槽——
同一个界面下可以挂自研的进程内内核，也可以挂任意 ACP agent（Claude Code、pi 等），
一个会话绑一个内核，两个内核可以同时在线、并排跑同一个任务。

- 许可证：Apache-2.0（[LICENSE](./LICENSE)）
- 平台：目前只支持 **Windows**（GPUI 的窗口 / 输入法 / 通知层都按 Windows 打通并实测；macOS 理论上可编，未测）

## 功能

**内核三种**

| 内核 | 说明 |
|---|---|
| 自研内核（进程内） | 直接连 OpenAI 兼容或 Anthropic 原生端点；工具循环、权限、压缩、子 agent、MCP、插件全在这一层 |
| ACP agent | 走 `agent-client-protocol` 起外部进程（Claude Code 适配器、pi 等），多会话、权限审批、续接都支持 |
| 演示回放 | 内置假数据，改界面时用，不落盘 |

**自研内核**

- 两种协议：OpenAI 兼容（OpenAI / DeepSeek / GLM / Kimi / OpenRouter / Ollama / vLLM …）
  与 Anthropic 原生（思考块、`tool_use`、提示缓存），都是流式，正文 / 思考 / 用量齐全
- 多套 provider 档案，输入框旁可快速切档；每套可带 temperature / max_tokens / 额外 JSON 字段
- 工具：`read_file` / `list_dir` / `write_file` / `edit_file` / `shell`（pwsh，超时、取消连子进程一起结束）/
  `glob` / `grep` / `web_fetch` / `todo_write` / `task`；同一步的只读调用并行执行
- 权限：Manual / Accept edits / Plan（只读）/ Bypass；「总是允许」按项目落盘，工作目录外的写入永远询问
- 上下文：自动压缩（用量过窗口八成时总结较早对话），「超出上下文」报错后压缩一次重发，`/compact` 手动压缩
- 读项目说明：`~/.claude/CLAUDE.md` + 工作目录到仓库根各层的 `AGENTS.md` / `CLAUDE.md` 进系统提示词，每轮重读
- 子 agent：`task` 工具开子会话，同一步的几个并行跑，工具行缩进在各自的 Task 行下
- 图片输入：粘贴截图或选文件，一轮最多 8 张

**MCP 与插件**

- MCP：数据目录的 `mcp.json`（格式同 Claude Code 的 `.mcp.json`），本地 stdio / 远程 Streamable HTTP / 旧版 SSE，
  远程服务支持 OAuth 2.1 登录（DCR + PKCE，token 过期自动刷新），断线自动退避重连
- 插件：兼容 Claude Code 的插件格式（斜杠命令、技能、子 agent、MCP 服务、钩子），可从 git 地址安装 / 更新 / 卸载

**会话与安全**

- 每会话一份追加式事件日志，启动时重放恢复；历史会话续聊时优先 ACP `session/resume`，其次 `session/load`，
  都不行就用本地日志重建上下文
- 检查点与回退：写文件前后按内容寻址存下旧内容，用户消息上可选「回退对话和文件 / 只回对话 / 只回文件」
- Windows job object 防孤儿进程：应用退出（含被强杀）时连带结束 agent 进程树

**界面**

- 虚拟列表对话区：1 万行滚动帧间隔与空会话相同
- 三种视图模式（摘要 / 标准 / 详细）、深色模式、界面缩放与字体设置
- 命令面板（Ctrl+K，含搜消息正文）、快捷键速查（Ctrl+/）、应用内提示、系统通知
- 首页用量概览：会话数 / 消息数 / Token / 活跃天数 / 高峰时段 / 26 周热力图 / 各模型用量

**用量网关**

每个请求经本地网关转发并记一行日志（只记耗时、用量、重试、取消，**不记提示词和密钥**），
按设置里的单价表估算费用，支持峰谷价。ACP agent 也能选经网关。

## 构建与运行

### 依赖

- Windows 10/11
- Rust stable（edition 2024，用 `rustup` 装即可；仓库没有 pin 工具链版本）
- Visual Studio 生成工具（C++ 桌面开发工作负载，提供链接器和 Windows SDK）
- 不必装 Node，除非要用 ACP 内核（`npx` 起适配器）

### 编译

```bash
git clone https://github.com/FJWangYantao/claudesktop.git
cd claudesktop
cargo build --release -p claudesktop
# 产物：target/release/claudesktop.exe（单文件，图标和版本信息已编进去，约 36 MB）
```

首次编译要拉 GPUI 那套依赖，约 10 分钟；之后增量很快。

### 运行

直接双击 `claudesktop.exe` 也可以，直接跑则能看到 stderr：

```bash
cargo run -p claudesktop
# 或者
./target/release/claudesktop.exe
```

**第一次启动**不会猜后端：没配过任何内核时，首页出来的是配置引导，
选「自研内核」或「ACP agent」进设置页填好，再回首页开始用。

- **自研内核**：设置 → 内核与模型 → 填端点、API Key、模型。
  内置 DeepSeek / GLM / Kimi / Anthropic / OpenAI / OpenRouter / Ollama 预设，点一下就能带出端点
- **ACP agent**：设置 → 内核与模型 → 选 ACP，命令填例如
  `cmd /c npx -y @agentclientprotocol/claude-agent-acp`（Claude Code 适配器），
  之后按适配器自己的方式配密钥

设置存在数据目录的 `settings.json`，点「保存并应用」会重启对应内核，有对话的会话下次发消息时自动续接。

### 数据目录

默认 `%APPDATA%\claudesktop`（可用 `CLAUDESKTOP_DATA_DIR` 改）：

```
settings.json        设置（含 provider 档案，保存后请自行保管）
permissions.json     「总是允许」规则，按项目分组
mcp.json             MCP 服务器配置
plugins/             插件
sessions/            每会话一份事件日志 + 元信息，图片附件在同级 .assets/
gateway/<日期>.jsonl 用量日志
```

### 测试与检查

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace                                    # 235 个，不需要网络和密钥

# 真实端点 / 真实 agent / 远程 MCP（默认 ignore，会花钱并需要网络）
cargo test -p claudesktop-builtin -- --ignored
```

CI（[.github/workflows/ci.yml](./.github/workflows/ci.yml)）在 windows-latest 上跑上面的 fmt + clippy + test。
打 `v*` tag 会触发 [release.yml](./.github/workflows/release.yml) 生成 GitHub Release。

## 开发

环境变量（都在 `main.rs` / `settings.rs` 里集中读取）：

```bash
# 指定新会话的默认内核：builtin / acp / demo
CLAUDESKTOP_KERNEL=builtin

# 自研内核的端点（等价于在设置页里填，启动时环境变量优先）
CLAUDESKTOP_BASE_URL=http://127.0.0.1:8045/v1
CLAUDESKTOP_MODEL=gemini-3.8-flash
CLAUDESKTOP_API_KEY=sk-...
CLAUDESKTOP_PROTOCOL=anthropic          # 走 Anthropic 原生协议
CLAUDESKTOP_TEMPERATURE=0.3 CLAUDESKTOP_MAX_TOKENS=8192
CLAUDESKTOP_EXTRA_JSON='{"top_p":0.9}'  # 浅合并进请求体，同名键覆盖默认值
CLAUDESKTOP_DEBUG_HTTP=1                # 请求与错误细节打到 stderr
CLAUDESKTOP_MAX_STEPS=200               # 一轮最多的工具步数

# 数据目录与调试钩子
CLAUDESKTOP_DATA_DIR=D:\tmp\claudesktop
CLAUDESKTOP_OPEN=settings|fetch|restart # 启动 1.5s 后打开某个界面
CLAUDESKTOP_DROP='路径;路径'             # 模拟从资源管理器拖文件进来
CLAUDESKTOP_RERUN=builtin|acp|demo      # 把首条提示词放到另一个内核上重跑

# 性能压测：逐帧滚动并输出帧间隔与每帧工作量
CLAUDESKTOP_STRESS=10000                # 灌 1 万行再滚；加 _BUSY=1 看转圈，_TYPE=1 看打字
```

提交约定：英文 conventional commits，代码注释中文。改 UI 前请先读 [AGENTS.md](./AGENTS.md)（组件、配色、
动效、快捷键都有既定封装与惯例）和 [设计指南-Claude风格UI.md](./设计指南-Claude风格UI.md)。

## 结构

```
apps/desktop   # UI 壳（gpui-kit 0.7）：窗口框架、会话视图、输入区、设置 / 首页 / 侧栏
crates/core    # 会话编排层：Kernel trait + KernelEvent（复用 ACP 类型）、存储、检查点、插件
crates/builtin # 自研内核：provider、工具循环、权限、压缩、MCP、子 agent、用量网关
tools          # 调试与接入工具（截图脚本、mock agent、mock MCP 服务器）
```

| 文档 | 内容 |
|---|---|
| [AGENTS.md](./AGENTS.md) | 开发约定：UI 组件惯例、配色与字号规范、动效、快捷键、内核侧注意事项 |
| [设计指南-Claude风格UI.md](./设计指南-Claude风格UI.md) | Claude 系产品的设计理念与视觉系统，第七章是本项目的落地清单 |
| [调研报告-GPUI-Claude风格桌面Harness.md](./调研报告-GPUI-Claude风格桌面Harness.md) | 立项时的技术调研 |
| [设计说明-检查点与回退.md](./设计说明-检查点与回退.md) | 回退的粒度、文件被别人改过怎么办 |
| [设计说明-插件.md](./设计说明-插件.md) | 插件目录格式、钩子、与 Claude Code 的兼容范围 |
| [设计说明-MCP-OAuth.md](./设计说明-MCP-OAuth.md) | 远程 MCP 的 OAuth 登录流程 |
| [想法-用量统计网关.md](./想法-用量统计网关.md) | 网关、日志格式、费用估算与峰谷价 |

## tools

| 文件 | 用途 |
|---|---|
| [tools/shot.ps1](./tools/shot.ps1) | 抓窗口截图，改完 UI 后自检渲染 |
| [tools/mock-acp-agent.mjs](./tools/mock-acp-agent.mjs) | mock ACP agent，跑内核事件链测试 |
| [tools/mock-mcp-server.mjs](./tools/mock-mcp-server.mjs) | mock MCP 服务器（`--http` / `--sse` 起远程模式） |
| [tools/make-icon.py](./tools/make-icon.py) | 生成 `resources/claudesktop.ico` |

内嵌字体（Inter / Newsreader / JetBrains Mono）均为 SIL OFL，来源见
[apps/desktop/assets/fonts/README.md](./apps/desktop/assets/fonts/README.md)。
